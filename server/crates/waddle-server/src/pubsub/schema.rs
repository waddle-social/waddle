use tracing::{error, warn};
use waddle_xmpp::XmppError;

use super::DatabasePubSubStorage;

/// Schema version for the pubsub + projection tables managed in this
/// module. CLAUDE.md greenlights breaking changes, so a version
/// mismatch unconditionally drops + recreates the listed tables —
/// there is no in-place migration path. Bump this number on any
/// schema-shape change, AND if a stricter parser may reject XML
/// payloads previously written by an older server (otherwise the
/// next read silently defaults the user to inactive; see
/// [`crate::dnd_projection`] preamble).
const PUBSUB_SCHEMA_VERSION: i64 = 5;

impl DatabasePubSubStorage {
    pub(super) async fn initialize(&self) -> Result<(), XmppError> {
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
        let current: Option<i64> = match rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            Some(row) => Some(
                row.get(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            ),
            None => None,
        };

        if current != Some(PUBSUB_SCHEMA_VERSION) {
            // Drop-and-recreate: CLAUDE.md greenlights breaking changes.
            // Surface the destruction loudly so a developer with a
            // file-backed dev SQLite doesn't lose hours of state on
            // a quiet branch swap.
            let prior_items: i64 = self.row_count("pubsub_items").await.unwrap_or(0);
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
                "notification_settings_projection",
                "notification_settings_projection_source_version",
                "pubsub_items",
                "pubsub_subscriptions",
                "pubsub_affiliations",
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
        Ok(())
    }

    async fn create_schema(&self) -> Result<(), XmppError> {
        // Bug 1 fix: use BIGINT for *_at_ms columns on Postgres (32-bit INTEGER
        // would overflow Unix-millis i64 timestamps ~1.7 trillion). Sqlite uses INTEGER.
        let nodes_ddl = match self.db.driver() {
            crate::db::DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_nodes (
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    access_model TEXT NOT NULL,
                    publish_model TEXT NOT NULL,
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
            crate::db::DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS pubsub_nodes (
                    owner_jid TEXT NOT NULL,
                    node_name TEXT NOT NULL,
                    access_model TEXT NOT NULL,
                    publish_model TEXT NOT NULL,
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
        let notification_settings_projection_ddl = match self.db.driver() {
            crate::db::DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS notification_settings_projection (
                    owner_bare_jid TEXT NOT NULL,
                    conversation_jid TEXT NOT NULL,
                    conversation_kind TEXT NOT NULL CHECK (conversation_kind IN ('direct', 'private_group', 'public_group')),
                    mode TEXT NOT NULL CHECK (mode IN ('always', 'on-mention', 'never')),
                    source_version INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    source_node TEXT NOT NULL,
                    source_item_id TEXT NOT NULL,
                    PRIMARY KEY (owner_bare_jid, conversation_jid)
                )
                "#
            }
            crate::db::DatabaseDriver::Postgres => {
                r#"
                CREATE TABLE IF NOT EXISTS notification_settings_projection (
                    owner_bare_jid TEXT NOT NULL,
                    conversation_jid TEXT NOT NULL,
                    conversation_kind TEXT NOT NULL CHECK (conversation_kind IN ('direct', 'private_group', 'public_group')),
                    mode TEXT NOT NULL CHECK (mode IN ('always', 'on-mention', 'never')),
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
        let notification_settings_projection_source_version_ddl = match self.db.driver() {
            crate::db::DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS notification_settings_projection_source_version (
                    id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
                    current_version INTEGER NOT NULL
                )
                "#
            }
            crate::db::DatabaseDriver::Postgres => {
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
        let dnd_projection_ddl = match self.db.driver() {
            crate::db::DatabaseDriver::Sqlite => {
                r#"
                CREATE TABLE IF NOT EXISTS dnd_projection (
                    owner_bare_jid TEXT NOT NULL PRIMARY KEY,
                    payload_xml TEXT NOT NULL,
                    source_version INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                )
                "#
            }
            crate::db::DatabaseDriver::Postgres => {
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

        let items_ddl = match self.db.driver() {
            crate::db::DatabaseDriver::Sqlite => {
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
            crate::db::DatabaseDriver::Postgres => {
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

        let subs_ddl = match self.db.driver() {
            crate::db::DatabaseDriver::Sqlite => {
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
            crate::db::DatabaseDriver::Postgres => {
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

        let affs_ddl = match self.db.driver() {
            crate::db::DatabaseDriver::Sqlite => {
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
            crate::db::DatabaseDriver::Postgres => {
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

        Ok(())
    }
}
