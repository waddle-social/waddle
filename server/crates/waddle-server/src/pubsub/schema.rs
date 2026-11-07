use crate::db::lineage::{PgIdentity, PgSystemIdentifier};
use crate::db::{CatalogFenceLock, DatabaseDriver, IntoParams, Rows, Transaction};
use tracing::{error, info, warn};
use waddle_xmpp::XmppError;

use waddle_xmpp::pubsub::AccessModel;
use waddle_xmpp_core::pubsub::{PEP_NODE_AVATAR_DATA, PEP_NODE_AVATAR_METADATA};

use super::owner::{self, PUBSUB_SCHEMA_ADVISORY_LOCK_KEY};
use super::DatabasePubSubStorage;

/// Schema version for the pubsub + projection tables managed in this
/// module. Legacy URL-based initialization resets these tables on a version
/// mismatch; guarded owner preparation refuses to reset persisted data.
/// There is no in-place migration path. Bump this number on any
/// schema-shape change, AND if a stricter parser may reject XML
/// payloads previously written by an older server (otherwise the
/// next read silently defaults the user to inactive; see
/// [`crate::dnd_projection`] preamble).
///
/// History:
/// * v5 — initial `dnd_projection` (#367)
/// * v6 — added `dnd_projection_source_version` singleton counter
///   to replace wall-clock millis as the LWW key (round-7 review)
// v7 (#719): notification_settings_projection gains the
// `rich_payload_opt_in` column (XEP-0492 `<advanced/>` rich-payload
// opt-in). Version-gated drop-and-recreate carries the schema change.
// * v8 — pubsub_nodes gains nullable node_type for XEP-0060
//   `pubsub#type` profile metadata (XEP-0472/XEP-0501).
pub(super) const PUBSUB_SCHEMA_VERSION: i64 = 8;

const DATA_TABLES: &[&str] = &[
    "pubsub_nodes",
    "pubsub_items",
    "pubsub_subscriptions",
    "pubsub_affiliations",
    "pubsub_owner_configured",
    "dnd_projection",
    "dnd_projection_source_version",
    "notification_settings_projection",
    "notification_settings_projection_source_version",
    "pubsub_push_publications",
];

#[derive(Clone, Copy)]
enum BootstrapPolicy<'a> {
    Legacy,
    GuardedOwner(&'a PgIdentity),
}

impl DatabasePubSubStorage {
    pub(super) async fn initialize(&self) -> Result<(), XmppError> {
        self.initialize_with_policy(BootstrapPolicy::Legacy).await
    }

    pub(super) async fn initialize_guarded_owner(
        &self,
        identity: &PgIdentity,
    ) -> Result<(), XmppError> {
        self.initialize_with_policy(BootstrapPolicy::GuardedOwner(identity))
            .await
    }

    async fn initialize_with_policy(&self, policy: BootstrapPolicy<'_>) -> Result<(), XmppError> {
        if self.db.uses_attested_postgres_pool() {
            return Err(XmppError::internal(
                "PubSub owner initialization requires an owner database handle".to_string(),
            ));
        }
        let driver = self.db.driver();
        let mut tx = match driver {
            DatabaseDriver::Postgres => {
                self.db
                    .begin_catalog_fenced(&[CatalogFenceLock::Exclusive(
                        PUBSUB_SCHEMA_ADVISORY_LOCK_KEY,
                    )])
                    .await
            }
            DatabaseDriver::Sqlite => self.db.begin_immediate().await,
        }
        .map_err(|error| XmppError::internal(error.to_string()))?;
        if driver == DatabaseDriver::Postgres {
            // The catalog fence and PubSub gate are held by this same
            // READ COMMITTED transaction before any catalog or relation read.
            tx.execute("SET LOCAL lock_timeout = '30s'", ())
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            tx.execute("SET LOCAL statement_timeout = '300000ms'", ())
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            if let BootstrapPolicy::GuardedOwner(identity) = policy {
                pin_owner_schema(&mut tx, identity).await?;
                owner::lock_existing_relations_tx(&mut tx, &identity.schema.name.0).await?;
                owner::begin_owner_phase_tx(&mut tx, identity).await?;
                super::runtime_attach::verify_owner_preflight(&mut tx, &identity.schema.name.0)
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
            } else {
                let schema = owner::pin_legacy_schema_path_tx(&mut tx).await?;
                owner::lock_existing_relations_tx(&mut tx, &schema).await?;
                owner::invalidate_completion_marker_tx(&mut tx, &schema).await?;
            }
        }
        SchemaTransaction {
            tx: &mut tx,
            driver,
        }
        .initialize(policy)
        .await?;
        if let BootstrapPolicy::GuardedOwner(identity) = policy {
            owner::complete_owner_phase_tx(&mut tx, identity).await?;
        }
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }
}

async fn pin_owner_schema(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), XmppError> {
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "PubSub owner schema")
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let mut rows = tx
        .query(
            "SELECT (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
         db.oid::bigint, db.datname::text, n.oid::bigint, n.nspname::text, \
         n.nspname = pg_catalog.current_schema() AND owner.rolname = current_user \
         AND NOT owner.rolcanlogin AND NOT owner.rolsuper AND NOT owner.rolcreatedb \
         AND NOT owner.rolcreaterole AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
         AND owner.oid <> db.datdba AND NOT pg_catalog.pg_is_in_recovery() \
         AND NOT pg_catalog.has_database_privilege(owner.oid, db.oid, 'TEMP') \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member = owner.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(\
           COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) a \
           WHERE a.grantee = 0 OR (a.grantee <> n.nspowner \
             AND (a.privilege_type <> 'USAGE' OR a.is_grantable))) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
           WHERE c.relnamespace = pg_catalog.pg_my_temp_schema()) \
         FROM pg_catalog.pg_namespace n JOIN pg_catalog.pg_roles owner ON owner.oid = n.nspowner \
         JOIN pg_catalog.pg_database db ON db.datname = pg_catalog.current_database() \
         WHERE n.nspname = ?",
            crate::db_params![schema],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let valid = match rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        Some(row) => {
            let system: String = row
                .get(0)
                .map_err(|error| XmppError::internal(error.to_string()))?;
            system.parse::<PgSystemIdentifier>().ok() == Some(identity.system_identifier)
                && row
                    .get::<i64>(1)
                    .map_err(|error| XmppError::internal(error.to_string()))?
                    == i64::from(identity.database.oid)
                && row
                    .get::<String>(2)
                    .map_err(|error| XmppError::internal(error.to_string()))?
                    == identity.database.name.0
                && row
                    .get::<i64>(3)
                    .map_err(|error| XmppError::internal(error.to_string()))?
                    == i64::from(identity.schema.oid)
                && row
                    .get::<String>(4)
                    .map_err(|error| XmppError::internal(error.to_string()))?
                    == *schema
                && row
                    .get::<bool>(5)
                    .map_err(|error| XmppError::internal(error.to_string()))?
        }
        None => false,
    };
    if !valid {
        return Err(XmppError::internal(
            "PubSub owner physical identity, role, or temporary schema mismatch".to_string(),
        ));
    }
    let quoted = crate::db::physical_attestation::quote_identifier(schema);
    tx.query(
        "SELECT pg_catalog.set_config('search_path', ?, true)",
        crate::db_params![format!("{quoted}, pg_temp")],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

struct SchemaTransaction<'tx, 'db> {
    tx: &'tx mut Transaction<'db>,
    driver: DatabaseDriver,
}

impl SchemaTransaction<'_, '_> {
    async fn execute(&mut self, sql: &str, params: impl IntoParams) -> Result<u64, XmppError> {
        self.tx
            .execute(sql, params)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    async fn query(&mut self, sql: &str, params: impl IntoParams) -> Result<Rows, XmppError> {
        self.tx
            .query(sql, params)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    async fn relation_exists(&mut self, table: &'static str) -> Result<bool, XmppError> {
        let exists_sql = match self.driver {
            DatabaseDriver::Postgres => "SELECT pg_catalog.to_regclass(?) IS NOT NULL",
            DatabaseDriver::Sqlite => {
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?)"
            }
        };
        let mut rows = self.query(exists_sql, crate::db_params![table]).await?;
        rows.next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
            .ok_or_else(|| {
                XmppError::internal("PubSub relation probe returned no row".to_string())
            })?
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    async fn existing_row_count(&mut self, table: &'static str) -> Result<i64, XmppError> {
        if !self.relation_exists(table).await? {
            return Ok(0);
        }
        let mut rows = self
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await?;
        rows.next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
            .ok_or_else(|| XmppError::internal("PubSub item count returned no row".to_string()))?
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    async fn initialize(&mut self, policy: BootstrapPolicy<'_>) -> Result<(), XmppError> {
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS pubsub_schema_version (
                version INTEGER NOT NULL PRIMARY KEY
            )
            "#,
            (),
        )
        .await?;

        let mut rows = self
            .query("SELECT version FROM pubsub_schema_version", ())
            .await?;
        let mut versions = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            versions.push(
                row.get::<i64>(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }
        let current = versions.first().copied();
        if matches!(policy, BootstrapPolicy::GuardedOwner(_)) {
            if versions.len() > 1
                || current.is_some_and(|version| !(1..=PUBSUB_SCHEMA_VERSION).contains(&version))
            {
                return Err(XmppError::internal(
                    "PubSub owner preparation refused ambiguous or unknown schema version"
                        .to_string(),
                ));
            }
            if current != Some(PUBSUB_SCHEMA_VERSION) {
                for &table in DATA_TABLES {
                    if self.existing_row_count(table).await? != 0 {
                        return Err(XmppError::internal(format!(
                            "PubSub owner preparation refused schema reset with data in {table}"
                        )));
                    }
                }
            } else {
                // A current marker alone does not prove all tables survived.
                // Recreating a missing item table while preserving a push
                // watermark could make a lost publication look applied.
                let mut complete = true;
                let mut populated = false;
                for &table in DATA_TABLES {
                    if self.relation_exists(table).await? {
                        populated |= self.existing_row_count(table).await? != 0;
                    } else {
                        complete = false;
                    }
                }
                if populated {
                    if !complete {
                        return Err(XmppError::internal(
                            "PubSub owner preparation refused incomplete populated schema"
                                .to_string(),
                        ));
                    }
                    // Do not repair or rewrite a populated current store.
                    // Restricted attachment independently verifies its shape.
                    return Ok(());
                }
            }
        }

        if current != Some(PUBSUB_SCHEMA_VERSION) {
            // Drop-and-recreate: CLAUDE.md greenlights breaking changes.
            // Surface the destruction loudly so a developer with a
            // file-backed dev SQLite doesn't lose hours of state on
            // a quiet branch swap.
            let prior_items = self.existing_row_count("pubsub_items").await?;
            if prior_items > 0 {
                error!(
                    from_version = ?current,
                    to_version = PUBSUB_SCHEMA_VERSION,
                    pubsub_items_dropped = prior_items,
                    "pubsub schema version mismatch — DROPPING all pubsub tables \
                     (including any DND, bookmark, vCard4, and avatar state). \
                     Set WADDLE_DATABASE_URL/WADDLE_XMPP_PUBSUB_DATABASE_URL to a \
                     fresh file if you wanted to preserve this data."
                );
            } else {
                warn!(
                    from_version = ?current,
                    to_version = PUBSUB_SCHEMA_VERSION,
                    "pubsub schema version mismatch — dropping and recreating tables"
                );
            }
            for table in [
                "dnd_projection",
                "dnd_projection_source_version",
                "notification_settings_projection",
                "notification_settings_projection_source_version",
                "pubsub_push_publications",
                "pubsub_items",
                "pubsub_subscriptions",
                "pubsub_affiliations",
                "pubsub_owner_configured",
                "pubsub_nodes",
            ] {
                self.execute(&format!("DROP TABLE IF EXISTS {table}"), ())
                    .await?;
            }
            self.execute("DELETE FROM pubsub_schema_version", ())
                .await?;
        }

        self.create_schema().await?;

        if current != Some(PUBSUB_SCHEMA_VERSION) {
            self.execute(
                "INSERT INTO pubsub_schema_version (version) VALUES (?)",
                crate::db_params![PUBSUB_SCHEMA_VERSION],
            )
            .await?;
        }
        self.execute(
            "CREATE TABLE IF NOT EXISTS pubsub_push_publications (\
                service_jid TEXT NOT NULL, node_name TEXT NOT NULL, \
                revision BIGINT NOT NULL CHECK (revision > 0), \
                job_token TEXT NOT NULL, payload_hash TEXT NOT NULL, \
                PRIMARY KEY (service_jid, node_name))",
            (),
        )
        .await?;
        self.repair_legacy_avatar_nodes().await
    }

    /// Startup repair of legacy XEP-0084 avatar nodes.
    ///
    /// Older binaries auto-created client-published avatar nodes on the
    /// Presence default, which Waddle's authz admits only the owner to, so
    /// the avatar was invisible to every peer. Every startup reopens such
    /// nodes unless the owner explicitly configured them (recorded in
    /// `pubsub_owner_configured` by the XEP-0060 §8.2 configure path).
    /// Running on every startup, not once, also repairs nodes created by
    /// old pods during a rolling deploy.
    async fn repair_legacy_avatar_nodes(&mut self) -> Result<(), XmppError> {
        let reopened = self
            .execute(
                "UPDATE pubsub_nodes SET access_model = ? \
                 WHERE access_model = ? AND node_name IN (?, ?) \
                 AND NOT EXISTS (SELECT 1 FROM pubsub_owner_configured oc \
                     WHERE oc.owner_jid = pubsub_nodes.owner_jid \
                     AND oc.node_name = pubsub_nodes.node_name)",
                crate::db_params![
                    AccessModel::Open.to_string(),
                    AccessModel::Presence.to_string(),
                    PEP_NODE_AVATAR_DATA,
                    PEP_NODE_AVATAR_METADATA
                ],
            )
            .await?;
        if reopened > 0 {
            info!(reopened, "reopened legacy Presence avatar nodes");
        }
        Ok(())
    }

    async fn create_schema(&mut self) -> Result<(), XmppError> {
        // Bug 1 fix: use BIGINT for *_at_ms columns on Postgres (32-bit INTEGER
        // would overflow Unix-millis i64 timestamps ~1.7 trillion). Sqlite uses INTEGER.
        let nodes_ddl = match self.driver {
            DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_nodes (
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    access_model TEXT NOT NULL,
                    publish_model TEXT NOT NULL,
                    node_type TEXT,
                    max_items INTEGER NOT NULL,
                    persist_items INTEGER NOT NULL,
                    deliver_payloads INTEGER NOT NULL,
                    notify_retract INTEGER NOT NULL,
                    notify_delete INTEGER NOT NULL,
                    send_last_published_item TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (owner_jid, node_name)
                )
                "#
            }
            DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_nodes (
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    access_model TEXT NOT NULL,
                    publish_model TEXT NOT NULL,
                    node_type TEXT,
                    max_items BIGINT NOT NULL,
                    persist_items INTEGER NOT NULL,
                    deliver_payloads INTEGER NOT NULL,
                    notify_retract INTEGER NOT NULL,
                    notify_delete INTEGER NOT NULL,
                    send_last_published_item TEXT NOT NULL,
                    created_at_ms BIGINT NOT NULL,
                    PRIMARY KEY (owner_jid, node_name)
                )
                "#
            }
        };
        self.execute(nodes_ddl, ()).await?;
        let notification_settings_projection_ddl = match self.driver {
            DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS notification_settings_projection (
                    owner_bare_jid TEXT NOT NULL,
                    conversation_jid TEXT NOT NULL,
                    conversation_kind TEXT NOT NULL CHECK (conversation_kind IN ('direct', 'private_group', 'public_group')),
                    mode TEXT NOT NULL CHECK (mode IN ('always', 'on-mention', 'never')),
                    rich_payload_opt_in INTEGER NOT NULL DEFAULT 0 CHECK (rich_payload_opt_in IN (0, 1)),
                    source_version INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    source_node TEXT NOT NULL,
                    source_item_id TEXT NOT NULL,
                    PRIMARY KEY (owner_bare_jid, conversation_jid)
                )
                "#
            }
            DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS notification_settings_projection (
                    owner_bare_jid TEXT NOT NULL,
                    conversation_jid TEXT NOT NULL,
                    conversation_kind TEXT NOT NULL CHECK (conversation_kind IN ('direct', 'private_group', 'public_group')),
                    mode TEXT NOT NULL CHECK (mode IN ('always', 'on-mention', 'never')),
                    rich_payload_opt_in INTEGER NOT NULL DEFAULT 0 CHECK (rich_payload_opt_in IN (0, 1)),
                    source_version BIGINT NOT NULL,
                    updated_at_ms BIGINT NOT NULL,
                    source_node TEXT NOT NULL,
                    source_item_id TEXT NOT NULL,
                    PRIMARY KEY (owner_bare_jid, conversation_jid)
                )
                "#
            }
        };
        self.execute(notification_settings_projection_ddl, ())
            .await?;
        let notification_settings_projection_source_version_ddl = match self.driver {
            DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS notification_settings_projection_source_version (
                    id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
                    current_version INTEGER NOT NULL
                )
                "#
            }
            DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS notification_settings_projection_source_version (
                    id BIGINT NOT NULL PRIMARY KEY CHECK (id = 1),
                    current_version BIGINT NOT NULL
                )
                "#
            }
        };
        self.execute(notification_settings_projection_source_version_ddl, ())
            .await?;
        self.execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_notification_settings_projection_owner_mode
                ON notification_settings_projection(owner_bare_jid, mode)
            "#,
            (),
        )
        .await?;

        // Waddle DND projection (#367). Single row per user; stores the
        // typed `<dnd xmlns='urn:waddle:dnd:0'>` payload XML so the T1
        // gate can parse-and-evaluate without a second IO hop into
        // `pubsub_items`. LWW on republish — there is no MUC-style
        // multi-source merge to perform.
        //
        // ## Access pattern
        //
        // The ONLY query shape is a point-lookup by `owner_bare_jid`
        // (the PRIMARY KEY's implicit index). If a future PR
        // introduces another query (e.g. a janitor scanning by
        // `updated_at_ms < cutoff`), it MUST add a covering index in
        // the same migration — the table grows by one row per user
        // and would otherwise force a full scan on every janitor tick.
        let dnd_projection_ddl = match self.driver {
            DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS dnd_projection (
                    owner_bare_jid TEXT NOT NULL PRIMARY KEY,
                    payload_xml TEXT NOT NULL,
                    source_version INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                )
                "#
            }
            DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS dnd_projection (
                    owner_bare_jid TEXT NOT NULL PRIMARY KEY,
                    payload_xml TEXT NOT NULL,
                    source_version BIGINT NOT NULL,
                    updated_at_ms BIGINT NOT NULL
                )
                "#
            }
        };
        self.execute(dnd_projection_ddl, ()).await?;

        // Singleton-row monotonic counter for the DND projection's
        // `source_version`. Switching from wall-clock millis to a
        // DB-backed counter (the same pattern as
        // `notification_settings_projection_source_version`) closes
        // the NTP-backwards-jump hole — the LWW guard
        // `WHERE excluded.source_version > dnd_projection.source_version`
        // is only safe when source_version is monotonic. With wall-
        // clock millis, an NTP slew could make a newer publish look
        // older and silently drop it from the projection while
        // `pubsub_items` still gets updated, leaving the two stores
        // disagreed. Round-7 Copilot review on PR #759.
        let dnd_projection_source_version_ddl = match self.driver {
            DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS dnd_projection_source_version (
                    id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
                    current_version INTEGER NOT NULL
                )
                "#
            }
            DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS dnd_projection_source_version (
                    id BIGINT NOT NULL PRIMARY KEY CHECK (id = 1),
                    current_version BIGINT NOT NULL
                )
                "#
            }
        };
        self.execute(dnd_projection_source_version_ddl, ()).await?;

        let items_ddl = match self.driver {
            DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_items (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    item_id TEXT NOT NULL,
                    payload_xml TEXT,
                    publisher_jid TEXT,
                    published_at_ms INTEGER NOT NULL,
                    UNIQUE (owner_jid, node_name, item_id),
                    FOREIGN KEY (owner_jid, node_name)
                        REFERENCES pubsub_nodes(owner_jid, node_name)
                        ON DELETE CASCADE
                )
                "#
            }
            DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_items (
                    seq BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    item_id TEXT NOT NULL,
                    payload_xml TEXT,
                    publisher_jid TEXT,
                    published_at_ms BIGINT NOT NULL,
                    UNIQUE (owner_jid, node_name, item_id),
                    FOREIGN KEY (owner_jid, node_name)
                        REFERENCES pubsub_nodes(owner_jid, node_name)
                        ON DELETE CASCADE
                )
                "#
            }
        };
        self.execute(items_ddl, ()).await?;

        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_pubsub_items_node_seq ON pubsub_items (owner_jid, node_name, seq DESC)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_pubsub_items_owner_item ON pubsub_items (owner_jid, item_id)",
            (),
        )
        .await?;

        let subs_ddl = match self.driver {
            DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_subscriptions (
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    subid TEXT NOT NULL,
                    subscriber_jid TEXT NOT NULL,
                    state TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (owner_jid, node_name, subid),
                    FOREIGN KEY (owner_jid, node_name)
                        REFERENCES pubsub_nodes(owner_jid, node_name)
                        ON DELETE CASCADE
                )
                "#
            }
            DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_subscriptions (
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    subid TEXT NOT NULL,
                    subscriber_jid TEXT NOT NULL,
                    state TEXT NOT NULL,
                    created_at_ms BIGINT NOT NULL,
                    PRIMARY KEY (owner_jid, node_name, subid),
                    FOREIGN KEY (owner_jid, node_name)
                        REFERENCES pubsub_nodes(owner_jid, node_name)
                        ON DELETE CASCADE
                )
                "#
            }
        };
        self.execute(subs_ddl, ()).await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_pubsub_subs_subscriber ON pubsub_subscriptions (owner_jid, subscriber_jid)",
            (),
        )
        .await?;

        let affs_ddl = match self.driver {
            DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_affiliations (
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    entity_jid TEXT NOT NULL,
                    affiliation TEXT NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (owner_jid, node_name, entity_jid),
                    FOREIGN KEY (owner_jid, node_name)
                        REFERENCES pubsub_nodes(owner_jid, node_name)
                        ON DELETE CASCADE
                )
                "#
            }
            DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_affiliations (
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    entity_jid TEXT NOT NULL,
                    affiliation TEXT NOT NULL,
                    updated_at_ms BIGINT NOT NULL,
                    PRIMARY KEY (owner_jid, node_name, entity_jid),
                    FOREIGN KEY (owner_jid, node_name)
                        REFERENCES pubsub_nodes(owner_jid, node_name)
                        ON DELETE CASCADE
                )
                "#
            }
        };
        self.execute(affs_ddl, ()).await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_pubsub_affs_entity ON pubsub_affiliations (owner_jid, entity_jid)",
            (),
        )
        .await?;
        self.execute(
            "CREATE TABLE IF NOT EXISTS pubsub_owner_configured (\
                owner_jid TEXT NOT NULL, \
                node_name TEXT NOT NULL, \
                PRIMARY KEY (owner_jid, node_name), \
                FOREIGN KEY (owner_jid, node_name) \
                    REFERENCES pubsub_nodes(owner_jid, node_name) ON DELETE CASCADE)",
            (),
        )
        .await?;

        Ok(())
    }
}
