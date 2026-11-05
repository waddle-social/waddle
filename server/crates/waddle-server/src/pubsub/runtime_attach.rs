//! Read-only verification of an owner-prepared PostgreSQL PubSub store.

use std::collections::HashMap;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

use super::owner::{
    self, PUBSUB_ATTACH_LOCK_FUNCTION, PUBSUB_SCHEMA_ADVISORY_LOCK_KEY, PUBSUB_SCHEMA_MARKER_TABLE,
    PUBSUB_SCHEMA_MARKER_VERSION,
};
use super::schema::PUBSUB_SCHEMA_VERSION;

/// A restricted PubSub store cannot attach to an unverified runtime database.
#[derive(Debug, thiserror::Error)]
pub enum PubSubRuntimeAttachError {
    #[error("PubSub runtime attachment requires an attested PostgreSQL database")]
    AttestedPostgresRequired,
    #[error("PubSub runtime attachment requires the main database lane")]
    MainLaneRequired,
    #[error("PubSub active PostgreSQL schema or search path differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not bound PostgreSQL waits for PubSub attachment")]
    ReadBounds,
    #[error("PubSub relation {table} is undergoing a concurrent schema change")]
    ConcurrentSchemaChange { table: &'static str },
    #[error("failed to lock PubSub relation {table}: {source}")]
    Relation {
        table: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("PubSub relation {table} has an unexpected shape")]
    InvalidTable { table: &'static str },
    #[error("PubSub relation {table} has an invalid column {column}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("PubSub relation {table} has invalid constraints")]
    InvalidConstraint { table: &'static str },
    #[error("PubSub relation {table} has invalid indexes")]
    InvalidIndex { table: &'static str },
    #[error("PubSub schema version is missing, ambiguous, or incompatible")]
    InvalidVersion,
    #[error("PubSub owner marker is missing, incomplete, malformed, or bound to another identity")]
    InvalidMarker,
    #[error("PubSub owner-controlled attach lock helper is missing, malformed, or misprivileged")]
    InvalidAttachLock,
    #[error("PubSub runtime role or privileges are unsafe at {component}")]
    UnsafeRuntimePrivileges { component: &'static str },
    #[error("PubSub identity sequence is missing, unsafe, or behind its rows")]
    InvalidSequence,
    #[error("PubSub projection counter {component} is behind its rows")]
    InvalidCounter { component: &'static str },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

#[derive(Clone, Copy)]
struct Column {
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    identity: &'static str,
    default: Option<&'static str>,
}

const fn column(name: &'static str, kind: &'static str, not_null: bool) -> Column {
    Column {
        name,
        kind,
        not_null,
        identity: "",
        default: None,
    }
}
const fn text(name: &'static str, not_null: bool) -> Column {
    column(name, "text", not_null)
}
const fn bigint(name: &'static str, not_null: bool) -> Column {
    column(name, "bigint", not_null)
}
const fn integer(name: &'static str, not_null: bool) -> Column {
    column(name, "integer", not_null)
}

struct Index {
    name: &'static str,
    columns: &'static str,
    options: &'static str,
}

struct Table {
    name: &'static str,
    columns: &'static [Column],
    primary: &'static str,
    unique: Option<&'static str>,
    foreign_node: bool,
    checks: &'static [&'static str],
    indexes: &'static [Index],
    insert: bool,
    update: bool,
    delete: bool,
}

const NO_CHECKS: &[&str] = &[];
const NO_INDEXES: &[Index] = &[];
const VERSION_COLUMNS: &[Column] = &[integer("version", true)];
const MARKER_COLUMNS: &[Column] = &[
    integer("marker_id", true),
    bigint("version", true),
    bigint("owner_phase", true),
    text("system_identifier", true),
    bigint("database_oid", true),
    text("database_name", true),
    bigint("schema_oid", true),
    text("schema_name", true),
];
const NODE_COLUMNS: &[Column] = &[
    text("owner_jid", true),
    text("node_name", true),
    text("access_model", true),
    text("publish_model", true),
    text("node_type", false),
    bigint("max_items", true),
    integer("persist_items", true),
    integer("deliver_payloads", true),
    integer("notify_retract", true),
    integer("notify_delete", true),
    text("send_last_published_item", true),
    bigint("created_at_ms", true),
];
const ITEM_COLUMNS: &[Column] = &[
    Column {
        name: "seq",
        kind: "bigint",
        not_null: true,
        identity: "d",
        default: None,
    },
    text("owner_jid", true),
    text("node_name", true),
    text("item_id", true),
    text("payload_xml", false),
    text("publisher_jid", false),
    bigint("published_at_ms", true),
];
const SUBSCRIPTION_COLUMNS: &[Column] = &[
    text("owner_jid", true),
    text("node_name", true),
    text("subid", true),
    text("subscriber_jid", true),
    text("state", true),
    bigint("created_at_ms", true),
];
const AFFILIATION_COLUMNS: &[Column] = &[
    text("owner_jid", true),
    text("node_name", true),
    text("entity_jid", true),
    text("affiliation", true),
    bigint("updated_at_ms", true),
];
const OWNER_CONFIG_COLUMNS: &[Column] = &[text("owner_jid", true), text("node_name", true)];
const DND_COLUMNS: &[Column] = &[
    text("owner_bare_jid", true),
    text("payload_xml", true),
    bigint("source_version", true),
    bigint("updated_at_ms", true),
];
const COUNTER_COLUMNS: &[Column] = &[bigint("id", true), bigint("current_version", true)];
const NOTIFICATION_COLUMNS: &[Column] = &[
    text("owner_bare_jid", true),
    text("conversation_jid", true),
    text("conversation_kind", true),
    text("mode", true),
    Column {
        name: "rich_payload_opt_in",
        kind: "integer",
        not_null: true,
        identity: "",
        default: Some("0"),
    },
    bigint("source_version", true),
    bigint("updated_at_ms", true),
    text("source_node", true),
    text("source_item_id", true),
];
const PUSH_COLUMNS: &[Column] = &[
    text("service_jid", true),
    text("node_name", true),
    bigint("revision", true),
    text("job_token", true),
    text("payload_hash", true),
];
const ITEM_INDEXES: &[Index] = &[
    Index {
        name: "idx_pubsub_items_node_seq",
        columns: "owner_jid,node_name,seq",
        options: "0,0,3",
    },
    Index {
        name: "idx_pubsub_items_owner_item",
        columns: "owner_jid,item_id",
        options: "0,0",
    },
];
const SUBSCRIPTION_INDEXES: &[Index] = &[Index {
    name: "idx_pubsub_subs_subscriber",
    columns: "owner_jid,subscriber_jid",
    options: "0,0",
}];
const AFFILIATION_INDEXES: &[Index] = &[Index {
    name: "idx_pubsub_affs_entity",
    columns: "owner_jid,entity_jid",
    options: "0,0",
}];
const NOTIFICATION_INDEXES: &[Index] = &[Index {
    name: "idx_notification_settings_projection_owner_mode",
    columns: "owner_bare_jid,mode",
    options: "0,0",
}];
const COUNTER_CHECKS: &[&str] = &["CHECK ((id = 1))"];
const NOTIFICATION_CHECKS: &[&str] = &[
    "CHECK ((conversation_kind = ANY (ARRAY['direct'::text, 'private_group'::text, 'public_group'::text])))",
    "CHECK ((mode = ANY (ARRAY['always'::text, 'on-mention'::text, 'never'::text])))",
    "CHECK ((rich_payload_opt_in = ANY (ARRAY[0, 1])))",
];
const PUSH_CHECKS: &[&str] = &["CHECK ((revision > 0))"];
const MARKER_CHECKS: &[&str] = &[
    "CHECK ((marker_id = 1))",
    "CHECK ((owner_phase = ANY (ARRAY[(0)::bigint, (1)::bigint])))",
    "CHECK ((version = 1))",
];

// Alphabetical lock order is shared by all PubSub runtime attachments.
const TABLES: &[Table] = &[
    Table {
        name: "dnd_projection",
        columns: DND_COLUMNS,
        primary: "owner_bare_jid",
        unique: None,
        foreign_node: false,
        checks: NO_CHECKS,
        indexes: NO_INDEXES,
        insert: true,
        update: true,
        delete: true,
    },
    Table {
        name: "dnd_projection_source_version",
        columns: COUNTER_COLUMNS,
        primary: "id",
        unique: None,
        foreign_node: false,
        checks: COUNTER_CHECKS,
        indexes: NO_INDEXES,
        insert: true,
        update: true,
        delete: false,
    },
    Table {
        name: "notification_settings_projection",
        columns: NOTIFICATION_COLUMNS,
        primary: "owner_bare_jid,conversation_jid",
        unique: None,
        foreign_node: false,
        checks: NOTIFICATION_CHECKS,
        indexes: NOTIFICATION_INDEXES,
        insert: true,
        update: true,
        delete: true,
    },
    Table {
        name: "notification_settings_projection_source_version",
        columns: COUNTER_COLUMNS,
        primary: "id",
        unique: None,
        foreign_node: false,
        checks: COUNTER_CHECKS,
        indexes: NO_INDEXES,
        insert: true,
        update: true,
        delete: false,
    },
    Table {
        name: "pubsub_affiliations",
        columns: AFFILIATION_COLUMNS,
        primary: "owner_jid,node_name,entity_jid",
        unique: None,
        foreign_node: true,
        checks: NO_CHECKS,
        indexes: AFFILIATION_INDEXES,
        insert: true,
        update: true,
        delete: true,
    },
    Table {
        name: "pubsub_items",
        columns: ITEM_COLUMNS,
        primary: "seq",
        unique: Some("owner_jid,node_name,item_id"),
        foreign_node: true,
        checks: NO_CHECKS,
        indexes: ITEM_INDEXES,
        insert: true,
        update: true,
        delete: true,
    },
    Table {
        name: "pubsub_nodes",
        columns: NODE_COLUMNS,
        primary: "owner_jid,node_name",
        unique: None,
        foreign_node: false,
        checks: NO_CHECKS,
        indexes: NO_INDEXES,
        insert: true,
        update: true,
        delete: true,
    },
    Table {
        name: "pubsub_owner_configured",
        columns: OWNER_CONFIG_COLUMNS,
        primary: "owner_jid,node_name",
        unique: None,
        foreign_node: true,
        checks: NO_CHECKS,
        indexes: NO_INDEXES,
        insert: true,
        update: false,
        delete: false,
    },
    Table {
        name: "pubsub_push_publications",
        columns: PUSH_COLUMNS,
        primary: "service_jid,node_name",
        unique: None,
        foreign_node: false,
        checks: PUSH_CHECKS,
        indexes: NO_INDEXES,
        insert: true,
        update: true,
        delete: false,
    },
    Table {
        name: PUBSUB_SCHEMA_MARKER_TABLE,
        columns: MARKER_COLUMNS,
        primary: "marker_id",
        unique: None,
        foreign_node: false,
        checks: MARKER_CHECKS,
        indexes: NO_INDEXES,
        insert: false,
        update: false,
        delete: false,
    },
    Table {
        name: "pubsub_schema_version",
        columns: VERSION_COLUMNS,
        primary: "version",
        unique: None,
        foreign_node: false,
        checks: NO_CHECKS,
        indexes: NO_INDEXES,
        insert: false,
        update: false,
        delete: false,
    },
    Table {
        name: "pubsub_subscriptions",
        columns: SUBSCRIPTION_COLUMNS,
        primary: "owner_jid,node_name,subid",
        unique: None,
        foreign_node: true,
        checks: NO_CHECKS,
        indexes: SUBSCRIPTION_INDEXES,
        insert: true,
        update: false,
        delete: true,
    },
];

pub(super) async fn verify(db: &Database) -> Result<(), PubSubRuntimeAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(PubSubRuntimeAttachError::AttestedPostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(PubSubRuntimeAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(PubSubRuntimeAttachError::AttestedPostgresRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;
    let schema = &capability.identity().schema.name.0;
    let quoted = crate::db::physical_attestation::quote_identifier(schema);
    let mut tx = db.begin().await?;
    // The advisory gate and definer helper run before relation locks. Keep
    // READ COMMITTED so later catalog checks get a fresh snapshot after any
    // owner transaction that held the gate; the helper's SUE locks then keep
    // DDL stable through every verification query.
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|_| PubSubRuntimeAttachError::ReadBounds)?;
    capability.attest_transaction(&mut tx).await?;
    tx.query(
        "SELECT pg_catalog.pg_advisory_xact_lock(?)",
        crate::db_params![PUBSUB_SCHEMA_ADVISORY_LOCK_KEY],
    )
    .await?;
    let mut active = tx
        .query(
            "SELECT n.oid::bigint = ? AND n.nspname = pg_catalog.current_schema() \
         AND COALESCE((pg_catalog.current_schemas(false))[1]::text,'') = n.nspname \
         AND pg_catalog.cardinality(pg_catalog.current_schemas(false)) <= 2 \
         AND (pg_catalog.cardinality(pg_catalog.current_schemas(false)) = 1 \
           OR pg_catalog.left((pg_catalog.current_schemas(false))[2]::text,8) = 'pg_temp_') \
         AND db.oid::bigint = ? AND db.datname = ? \
         FROM pg_catalog.pg_namespace n JOIN pg_catalog.pg_database db \
           ON db.datname = pg_catalog.current_database() WHERE n.nspname = ?",
            crate::db_params![
                i64::from(capability.identity().schema.oid),
                i64::from(capability.identity().database.oid),
                capability.identity().database.name.0.clone(),
                schema
            ],
        )
        .await?;
    if !matches!(active.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PubSubRuntimeAttachError::ActiveSchemaMismatch);
    }
    verify_runtime_role(&mut tx, schema).await?;
    verify_attach_lock_function(&mut tx, capability.identity()).await?;
    let attach_lock = format!(
        "{}.{}",
        quoted,
        crate::db::physical_attestation::quote_identifier(PUBSUB_ATTACH_LOCK_FUNCTION)
    );
    let mut locked = match tx.query(&format!("SELECT {attach_lock}()"), ()).await {
        Ok(locked) => locked,
        Err(source) if postgres_lock_not_available(&source) => {
            return Err(PubSubRuntimeAttachError::ConcurrentSchemaChange {
                table: attach_lock_error_table(&source),
            });
        }
        Err(source) => return Err(source.into()),
    };
    if locked.next().await?.is_none() {
        return Err(PubSubRuntimeAttachError::InvalidAttachLock);
    }
    drop(locked);
    for table in TABLES {
        verify_table(&mut tx, schema, table).await?;
        verify_columns(&mut tx, schema, table).await?;
        verify_constraints(&mut tx, schema, table).await?;
        verify_indexes(&mut tx, schema, table).await?;
        verify_table_acl(&mut tx, schema, table).await?;
    }
    if !verify_marker_shape_and_data_tx(&mut tx, capability.identity()).await? {
        return Err(PubSubRuntimeAttachError::InvalidMarker);
    }
    let mut versions = tx
        .query(
            &format!("SELECT version FROM {quoted}.pubsub_schema_version"),
            (),
        )
        .await?;
    let version = match versions.next().await? {
        Some(row) => Some(row.get::<i64>(0)?),
        None => None,
    };
    if version != Some(PUBSUB_SCHEMA_VERSION) || versions.next().await?.is_some() {
        return Err(PubSubRuntimeAttachError::InvalidVersion);
    }
    verify_sequence(&mut tx, schema, &quoted).await?;
    verify_counter(
        &mut tx,
        &quoted,
        "dnd_projection_source_version",
        "dnd_projection",
    )
    .await?;
    verify_counter(
        &mut tx,
        &quoted,
        "notification_settings_projection_source_version",
        "notification_settings_projection",
    )
    .await?;
    tx.rollback().await?;
    Ok(())
}

/// Before owner DDL or data probes, reject executable or unexpected relation
/// kinds and validate an existing marker as strictly as runtime attachment.
pub(super) async fn verify_owner_preflight(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PubSubRuntimeAttachError> {
    for table in TABLES {
        let mut rows = tx.query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition \
             AND c.relowner = n.nspowner AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND c.reloftype = 0 \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhrelid = c.oid OR h.inhparent = c.oid) \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw WHERE rw.ev_class = c.oid) \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid AND NOT t.tgisinternal) \
             FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, table.name],
        ).await?;
        if let Some(row) = rows.next().await? {
            if !row.get::<bool>(0)? {
                return Err(PubSubRuntimeAttachError::InvalidTable { table: table.name });
            }
            if table.name == "pubsub_schema_version" || table.name == PUBSUB_SCHEMA_MARKER_TABLE {
                verify_table(tx, schema, table).await?;
                verify_columns(tx, schema, table).await?;
                verify_constraints(tx, schema, table).await?;
                verify_indexes(tx, schema, table).await?;
            }
        }
    }
    Ok(())
}

pub(super) async fn verify_marker_shape_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PubSubRuntimeAttachError> {
    let marker = TABLES
        .iter()
        .find(|table| table.name == PUBSUB_SCHEMA_MARKER_TABLE)
        .ok_or(PubSubRuntimeAttachError::InvalidTable {
            table: PUBSUB_SCHEMA_MARKER_TABLE,
        })?;
    verify_table(tx, schema, marker).await?;
    verify_columns(tx, schema, marker).await?;
    verify_constraints(tx, schema, marker).await?;
    verify_indexes(tx, schema, marker).await
}

pub(super) async fn verify_marker_shape_and_data_tx(
    tx: &mut Transaction<'_>,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<bool, PubSubRuntimeAttachError> {
    verify_marker_shape_tx(tx, &identity.schema.name.0).await?;
    verify_marker_data(tx, identity).await
}

pub(super) async fn verify_owner_schema_shape_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PubSubRuntimeAttachError> {
    for table in TABLES {
        verify_owner_table_owner(tx, schema, table).await?;
        verify_table(tx, schema, table).await?;
        verify_columns(tx, schema, table).await?;
        verify_constraints(tx, schema, table).await?;
        verify_indexes(tx, schema, table).await?;
    }
    verify_owner_attach_lock_function(tx, schema).await?;
    Ok(())
}

async fn verify_attach_lock_function(
    tx: &mut Transaction<'_>,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<(), PubSubRuntimeAttachError> {
    verify_attach_lock_function_shape(tx, &identity.schema.name.0, Some(identity.schema.oid), true)
        .await
}

pub(super) async fn verify_owner_attach_lock_function(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PubSubRuntimeAttachError> {
    verify_attach_lock_function_shape(tx, schema, None, false).await
}

async fn verify_attach_lock_function_shape(
    tx: &mut Transaction<'_>,
    schema: &str,
    schema_oid: Option<u32>,
    verify_runtime_grant: bool,
) -> Result<(), PubSubRuntimeAttachError> {
    let body = owner::attach_lock_function_body(schema);
    let runtime_grant = if verify_runtime_grant {
        "pg_catalog.has_function_privilege(current_user, p.oid, 'EXECUTE') \
         AND NOT pg_catalog.has_function_privilege(current_user, p.oid, 'EXECUTE WITH GRANT OPTION') \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(p.proacl, \
           pg_catalog.acldefault('f', p.proowner))) a \
           WHERE a.grantee = 0 OR a.grantee NOT IN (p.proowner, runtime.oid) \
             OR (a.grantee = runtime.oid AND (a.privilege_type <> 'EXECUTE' OR a.is_grantable)) \
             OR (a.grantee = p.proowner AND a.privilege_type <> 'EXECUTE'))"
    } else {
        "NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(p.proacl, \
           pg_catalog.acldefault('f', p.proowner))) a \
           WHERE a.grantee = 0 OR (a.grantee <> p.proowner \
             AND (a.privilege_type <> 'EXECUTE' OR a.is_grantable)))"
    };
    let query = format!(
        "SELECT pg_catalog.count(*)::bigint, pg_catalog.min(p.oid::bigint), \
         COALESCE(pg_catalog.bool_and( \
           p.prokind = 'f' AND p.prosecdef AND p.proowner = n.nspowner \
           AND p.prorettype = 'pg_catalog.void'::pg_catalog.regtype \
           AND p.prolang = (SELECT oid FROM pg_catalog.pg_language WHERE lanname = 'sql') \
           AND p.pronargs = 0 AND p.pronargdefaults = 0 AND p.provariadic = 0 AND NOT p.proretset \
           AND p.provolatile = 'v' AND NOT p.proleakproof AND p.proparallel = 'u' \
           AND p.proconfig = ARRAY['search_path=pg_catalog, pg_temp']::text[] AND p.prosrc = ? \
           AND (?::bigint IS NULL OR n.oid::bigint = ?::bigint) \
           AND {runtime_grant}), false) \
         FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
         LEFT JOIN pg_catalog.pg_roles runtime ON runtime.rolname = current_user \
         WHERE n.nspname = ? AND p.proname = ?"
    );
    let mut rows = tx
        .query(
            &query,
            crate::db_params![
                body,
                schema_oid.map(i64::from),
                schema_oid.map(i64::from),
                schema,
                PUBSUB_ATTACH_LOCK_FUNCTION
            ],
        )
        .await?;
    let valid = match rows.next().await? {
        Some(row) => {
            row.get::<i64>(0)? == 1
                && row.get::<Option<i64>>(1)?.is_some_and(|oid| oid > 0)
                && row.get::<bool>(2)?
        }
        None => false,
    };
    if !valid {
        return Err(PubSubRuntimeAttachError::InvalidAttachLock);
    }
    Ok(())
}

fn postgres_lock_not_available(error: &DatabaseError) -> bool {
    matches!(
        error,
        DatabaseError::Internal(sqlx::Error::Database(database_error))
            if database_error.code().as_deref() == Some("55P03")
    )
}

fn attach_lock_error_table(error: &DatabaseError) -> &'static str {
    let message = match error {
        DatabaseError::Internal(sqlx::Error::Database(database_error)) => database_error.message(),
        _ => return "unknown PubSub relation",
    };
    TABLES
        .iter()
        .find(|table| message.contains(table.name))
        .map_or("unknown PubSub relation", |table| table.name)
}

async fn verify_owner_table_owner(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PubSubRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.relowner = n.nspowner FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, table.name],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PubSubRuntimeAttachError::InvalidTable { table: table.name });
    }
    Ok(())
}

async fn verify_marker_data(
    tx: &mut Transaction<'_>,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<bool, PubSubRuntimeAttachError> {
    let schema = crate::db::physical_attestation::quote_identifier(&identity.schema.name.0);
    let marker = crate::db::physical_attestation::quote_identifier(PUBSUB_SCHEMA_MARKER_TABLE);
    let mut rows = tx
        .query(
            &format!(
                "SELECT version, owner_phase, system_identifier, database_oid, database_name, schema_oid, schema_name \
                 FROM {schema}.{marker} WHERE marker_id = 1"
            ),
            (),
        )
        .await
        .map_err(|source| PubSubRuntimeAttachError::Relation {
            table: PUBSUB_SCHEMA_MARKER_TABLE,
            source,
        })?;
    let row = rows
        .next()
        .await?
        .ok_or(PubSubRuntimeAttachError::InvalidMarker)?;
    let version: i64 = row.get(0)?;
    let owner_phase: i64 = row.get(1)?;
    let system_identifier: String = row.get(2)?;
    let database_oid: i64 = row.get(3)?;
    let database_name: String = row.get(4)?;
    let schema_oid: i64 = row.get(5)?;
    let schema_name: String = row.get(6)?;
    let valid = version == PUBSUB_SCHEMA_MARKER_VERSION
        && matches!(owner_phase, 0 | 1)
        && system_identifier == identity.system_identifier.to_string()
        && database_oid == i64::from(identity.database.oid)
        && database_name == identity.database.name.0
        && schema_oid == i64::from(identity.schema.oid)
        && schema_name == identity.schema.name.0;
    drop(rows);
    if !valid {
        return Err(PubSubRuntimeAttachError::InvalidMarker);
    }
    let mut rows = tx
        .query(
            &format!("SELECT count(*)::bigint FROM {schema}.{marker}"),
            (),
        )
        .await
        .map_err(|source| PubSubRuntimeAttachError::Relation {
            table: PUBSUB_SCHEMA_MARKER_TABLE,
            source,
        })?;
    if !matches!(rows.next().await?, Some(row) if row.get::<i64>(0)? == 1) {
        return Err(PubSubRuntimeAttachError::InvalidMarker);
    }
    Ok(owner_phase == 1)
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PubSubRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition \
         AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND c.reloftype = 0 \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhrelid = c.oid OR h.inhparent = c.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw WHERE rw.ev_class = c.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid AND NOT t.tgisinternal) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint fk \
           LEFT JOIN pg_catalog.pg_class child ON child.oid = fk.conrelid \
           WHERE fk.contype = 'f' AND fk.confrelid = c.oid AND NOT \
             (c.relname = 'pubsub_nodes' AND child.relnamespace = n.oid AND child.relname IN \
              ('pubsub_items','pubsub_subscriptions','pubsub_affiliations','pubsub_owner_configured'))) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid AND t.tgisinternal \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint fk \
             JOIN pg_catalog.pg_class child ON child.oid = fk.conrelid \
             JOIN pg_catalog.pg_class parent ON parent.oid = fk.confrelid \
             WHERE fk.oid = t.tgconstraint AND fk.contype = 'f' AND parent.relname = 'pubsub_nodes' \
               AND parent.relnamespace = n.oid AND child.relnamespace = n.oid \
               AND child.relname IN ('pubsub_items','pubsub_subscriptions','pubsub_affiliations','pubsub_owner_configured') \
               AND (fk.conrelid = c.oid OR fk.confrelid = c.oid))) \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = ? AND c.relname = ?",
        crate::db_params![schema, table.name],
    ).await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PubSubRuntimeAttachError::InvalidTable { table: table.name });
    }
    Ok(())
}

async fn verify_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PubSubRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull, \
         a.attgenerated::text, a.attidentity::text, pg_catalog.pg_get_expr(d.adbin,d.adrelid), \
         a.attcollation = typ.typcollation \
         FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_type typ ON typ.oid = a.atttypid \
         LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
         WHERE n.nspname = ? AND c.relname = ? AND a.attnum > 0 AND NOT a.attisdropped",
        crate::db_params![schema, table.name],
    ).await?;
    let mut actual = HashMap::new();
    while let Some(row) = rows.next().await? {
        actual.insert(
            row.get::<String>(0)?,
            (
                row.get::<String>(1)?,
                row.get::<bool>(2)?,
                row.get::<String>(3)?,
                row.get::<String>(4)?,
                row.get::<Option<String>>(5)?,
                row.get::<bool>(6)?,
            ),
        );
    }
    if actual.len() != table.columns.len() {
        return Err(PubSubRuntimeAttachError::InvalidTable { table: table.name });
    }
    for column in table.columns {
        let Some((kind, not_null, generated, identity, default, default_collation)) =
            actual.get(column.name)
        else {
            return Err(PubSubRuntimeAttachError::InvalidColumn {
                table: table.name,
                column: column.name,
            });
        };
        if kind != column.kind
            || *not_null != column.not_null
            || !generated.is_empty()
            || identity != column.identity
            || default.as_deref() != column.default
            || !default_collation
        {
            return Err(PubSubRuntimeAttachError::InvalidColumn {
                table: table.name,
                column: column.name,
            });
        }
    }
    Ok(())
}

async fn verify_constraints(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PubSubRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT con.contype::text, con.condeferrable, con.condeferred, con.convalidated, \
         COALESCE(ix.indisvalid AND ix.indisready AND ix.indislive AND ix.indimmediate, false), \
         pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.conkey) WITH ORDINALITY k(attnum,pos) \
           JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum ORDER BY k.pos), ','), \
         pg_catalog.pg_get_constraintdef(con.oid), parent.relname::text, parent.relnamespace = n.oid, \
         con.confdeltype::text, con.confupdtype::text, con.confmatchtype::text, \
         pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.confkey) WITH ORDINALITY k(attnum,pos) \
           JOIN pg_catalog.pg_attribute a ON a.attrelid = parent.oid AND a.attnum = k.attnum ORDER BY k.pos), ','), \
         (SELECT count(*) = 4 AND COALESCE(pg_catalog.bool_and(t.tgenabled IN ('O','A')),false) \
           FROM pg_catalog.pg_trigger t WHERE t.tgconstraint = con.oid AND t.tgisinternal) \
         FROM pg_catalog.pg_constraint con JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         LEFT JOIN pg_catalog.pg_class parent ON parent.oid = con.confrelid \
         LEFT JOIN pg_catalog.pg_index ix ON ix.indexrelid = con.conindid \
         WHERE n.nspname = ? AND c.relname = ?",
        crate::db_params![schema, table.name],
    ).await?;
    let mut primary = false;
    let mut unique = false;
    let mut foreign = false;
    let mut checks = Vec::new();
    let mut count = 0usize;
    while let Some(row) = rows.next().await? {
        count += 1;
        let kind: String = row.get(0)?;
        let deferred: bool = row.get(1)?;
        let initially_deferred: bool = row.get(2)?;
        let validated: bool = row.get(3)?;
        let keys: String = row.get(5)?;
        match kind.as_str() {
            "p" => {
                primary = !primary
                    && !deferred
                    && !initially_deferred
                    && validated
                    && row.get::<bool>(4)?
                    && keys == table.primary
            }
            "u" => {
                unique = !unique
                    && !deferred
                    && !initially_deferred
                    && validated
                    && row.get::<bool>(4)?
                    && Some(keys.as_str()) == table.unique
            }
            "c" => {
                if !validated || deferred || initially_deferred {
                    return Err(PubSubRuntimeAttachError::InvalidConstraint { table: table.name });
                }
                checks.push(row.get::<String>(6)?);
            }
            "f" => {
                foreign = !foreign
                    && table.foreign_node
                    && !deferred
                    && !initially_deferred
                    && validated
                    && keys == "owner_jid,node_name"
                    && row.get::<Option<String>>(7)?.as_deref() == Some("pubsub_nodes")
                    && row.get::<Option<bool>>(8)? == Some(true)
                    && row.get::<String>(9)? == "c"
                    && row.get::<String>(10)? == "a"
                    && row.get::<String>(11)? == "s"
                    && row.get::<String>(12)? == "owner_jid,node_name"
                    && row.get::<bool>(13)?;
            }
            _ => return Err(PubSubRuntimeAttachError::InvalidConstraint { table: table.name }),
        }
    }
    checks.sort_unstable();
    let mut expected_checks = table.checks.to_vec();
    expected_checks.sort_unstable();
    let expected_count = 1
        + usize::from(table.unique.is_some())
        + usize::from(table.foreign_node)
        + table.checks.len();
    if !primary
        || unique != table.unique.is_some()
        || foreign != table.foreign_node
        || checks.iter().map(String::as_str).collect::<Vec<_>>() != expected_checks
        || count != expected_count
    {
        return Err(PubSubRuntimeAttachError::InvalidConstraint { table: table.name });
    }
    Ok(())
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PubSubRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.count(*)::bigint FROM pg_catalog.pg_index i \
         JOIN pg_catalog.pg_class c ON c.oid = i.indrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, table.name],
        )
        .await?;
    let count: i64 = rows
        .next()
        .await?
        .ok_or(PubSubRuntimeAttachError::InvalidIndex { table: table.name })?
        .get(0)?;
    if count != (1 + usize::from(table.unique.is_some()) + table.indexes.len()) as i64 {
        return Err(PubSubRuntimeAttachError::InvalidIndex { table: table.name });
    }
    let mut indexes = tx
        .query(
            "SELECT ix.relname::text, \
             i.indisvalid AND i.indisready AND i.indislive AND am.amname = 'btree' \
             AND i.indnkeyatts = i.indnatts AND i.indpred IS NULL AND i.indexprs IS NULL \
             AND (SELECT pg_catalog.count(*) = i.indnkeyatts \
               AND COALESCE(pg_catalog.bool_and(opc.opcdefault \
                 AND opc.opcmethod = ix.relam AND opc.opcintype = a.atttypid \
                 AND key_collation.collation_oid = a.attcollation), false) \
               FROM pg_catalog.unnest(i.indclass) WITH ORDINALITY opkey(opclass_oid, pos) \
               JOIN pg_catalog.unnest(i.indkey) WITH ORDINALITY indkey(attnum, pos) \
                 ON indkey.pos = opkey.pos \
               JOIN pg_catalog.unnest(i.indcollation) WITH ORDINALITY key_collation(collation_oid, pos) \
                 ON key_collation.pos = opkey.pos \
               JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = indkey.attnum \
               JOIN pg_catalog.pg_opclass opc ON opc.oid = opkey.opclass_oid) \
             FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
             WHERE n.nspname = ? AND t.relname = ?",
            crate::db_params![schema, table.name],
        )
        .await?;
    let mut operator_count = 0_i64;
    while let Some(row) = indexes.next().await? {
        operator_count += 1;
        if !row.get::<bool>(1)? {
            return Err(PubSubRuntimeAttachError::InvalidIndex { table: table.name });
        }
    }
    if operator_count != count {
        return Err(PubSubRuntimeAttachError::InvalidIndex { table: table.name });
    }
    for index in table.indexes {
        let mut rows = tx.query(
            "SELECT NOT i.indisunique AND NOT i.indisprimary AND i.indisvalid AND i.indisready AND i.indislive \
             AND i.indpred IS NULL AND i.indexprs IS NULL AND am.amname = 'btree' \
             AND i.indnkeyatts = i.indnatts, \
             pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(i.indkey) WITH ORDINALITY k(attnum,pos) \
               JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum WHERE k.pos <= i.indnkeyatts ORDER BY k.pos), ','), \
             pg_catalog.array_to_string(ARRAY(SELECT k.option::text FROM pg_catalog.unnest(i.indoption) WITH ORDINALITY k(option,pos) \
               WHERE k.pos <= i.indnkeyatts ORDER BY k.pos), ',') \
             FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
             WHERE n.nspname = ? AND t.relname = ? AND ix.relname = ? AND ix.relnamespace = n.oid",
            crate::db_params![schema, table.name, index.name],
        ).await?;
        let valid = match rows.next().await? {
            Some(row) => {
                row.get::<bool>(0)?
                    && row.get::<String>(1)? == index.columns
                    && row.get::<String>(2)? == index.options
            }
            None => false,
        };
        if !valid {
            return Err(PubSubRuntimeAttachError::InvalidIndex { table: table.name });
        }
    }
    Ok(())
}

async fn verify_runtime_role(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PubSubRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreatedb AND NOT r.rolcreaterole \
         AND NOT r.rolreplication AND NOT r.rolbypassrls AND session_user::text = r.rolname \
         AND current_user::text = r.rolname AND r.oid <> db.datdba AND r.oid <> n.nspowner \
         AND NOT owner.rolcanlogin AND NOT owner.rolsuper AND NOT owner.rolcreatedb \
         AND NOT owner.rolcreaterole AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
         AND owner.oid <> db.datdba \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member = r.oid OR m.roleid = r.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p WHERE p.pronamespace = n.oid AND p.proowner = r.oid) \
         AND pg_catalog.has_schema_privilege(r.oid,n.oid,'USAGE') \
         AND NOT pg_catalog.has_schema_privilege(r.oid,n.oid,'CREATE') \
         AND NOT pg_catalog.has_database_privilege(r.oid,db.oid,'CREATE,TEMP') \
         AND NOT pg_catalog.has_parameter_privilege(r.oid,'session_replication_role','SET') \
         AND pg_catalog.current_setting('session_replication_role') = 'origin' \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(n.nspacl,pg_catalog.acldefault('n',n.nspowner))) a \
           WHERE a.grantee = 0 OR (a.grantee <> n.nspowner AND (a.privilege_type <> 'USAGE' OR a.is_grantable))) \
         FROM pg_catalog.pg_roles r JOIN pg_catalog.pg_namespace n ON n.nspname = ? \
         JOIN pg_catalog.pg_roles owner ON owner.oid = n.nspowner \
         JOIN pg_catalog.pg_database db ON db.datname = pg_catalog.current_database() \
         WHERE r.rolname = current_user",
        crate::db_params![schema],
    ).await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PubSubRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "runtime role",
        });
    }
    Ok(())
}

async fn verify_table_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PubSubRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT c.relowner = n.nspowner AND c.relowner <> r.oid \
         AND NOT pg_catalog.pg_has_role(r.oid,c.relowner,'SET') \
         AND pg_catalog.has_table_privilege(r.oid,c.oid,'SELECT') \
         AND pg_catalog.has_table_privilege(r.oid,c.oid,'INSERT') = (? = 1) \
         AND pg_catalog.has_table_privilege(r.oid,c.oid,'UPDATE') = (? = 1) \
         AND pg_catalog.has_table_privilege(r.oid,c.oid,'DELETE') = (? = 1) \
         AND NOT pg_catalog.has_table_privilege(r.oid,c.oid,'TRUNCATE,REFERENCES,TRIGGER') \
         AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid,'REFERENCES') \
         AND NOT pg_catalog.has_table_privilege(r.oid,c.oid, \
          'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION,TRIGGER WITH GRANT OPTION') \
         AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid, \
          'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION') \
         AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
           THEN NOT pg_catalog.has_table_privilege(r.oid,c.oid,'MAINTAIN') ELSE true END \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) a \
           WHERE a.grantee NOT IN (c.relowner,r.oid)) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute col CROSS JOIN LATERAL pg_catalog.aclexplode(col.attacl) a \
           WHERE col.attrelid = c.oid AND col.attnum > 0 AND NOT col.attisdropped AND a.grantee <> c.relowner) \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
         JOIN pg_catalog.pg_roles r ON r.rolname=current_user WHERE n.nspname = ? AND c.relname = ?",
        crate::db_params![table.insert, table.update, table.delete, schema, table.name],
    ).await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PubSubRuntimeAttachError::UnsafeRuntimePrivileges {
            component: table.name,
        });
    }
    Ok(())
}

async fn verify_sequence(
    tx: &mut Transaction<'_>,
    schema: &str,
    quoted: &str,
) -> Result<(), PubSubRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT seq.relname::text, seq.relkind = 'S' AND seq.relpersistence = 'p' AND seq.relowner = n.nspowner \
         AND s.seqtypid = 'pg_catalog.int8'::pg_catalog.regtype AND s.seqstart = 1 AND s.seqincrement = 1 \
         AND s.seqmin = 1 AND s.seqmax = 9223372036854775807 AND s.seqcache = 1 AND NOT s.seqcycle \
         AND pg_catalog.has_sequence_privilege(r.oid,seq.oid,'SELECT') \
         AND NOT pg_catalog.has_sequence_privilege(r.oid,seq.oid,'USAGE,UPDATE') \
         AND NOT pg_catalog.has_sequence_privilege(r.oid,seq.oid,'SELECT WITH GRANT OPTION,USAGE WITH GRANT OPTION,UPDATE WITH GRANT OPTION') \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(seq.relacl,pg_catalog.acldefault('S',seq.relowner))) a \
           WHERE a.grantee NOT IN (seq.relowner,r.oid) OR (a.grantee = r.oid AND (a.privilege_type <> 'SELECT' OR a.is_grantable))) \
         FROM pg_catalog.pg_class t JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
         JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attname = 'seq' AND NOT a.attisdropped \
         JOIN pg_catalog.pg_depend d ON d.refobjid = t.oid AND d.refobjsubid = a.attnum \
           AND d.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass \
           AND d.classid = 'pg_catalog.pg_class'::pg_catalog.regclass AND d.objsubid = 0 AND d.deptype = 'i' \
         JOIN pg_catalog.pg_class seq ON seq.oid = d.objid AND seq.relnamespace = n.oid \
         JOIN pg_catalog.pg_sequence s ON s.seqrelid = seq.oid \
         JOIN pg_catalog.pg_roles r ON r.rolname = current_user \
         WHERE n.nspname = ? AND t.relname = 'pubsub_items'",
        crate::db_params![schema],
    ).await?;
    let Some(row) = rows.next().await? else {
        return Err(PubSubRuntimeAttachError::InvalidSequence);
    };
    let name: String = row.get(0)?;
    if !row.get::<bool>(1)? || rows.next().await?.is_some() {
        return Err(PubSubRuntimeAttachError::InvalidSequence);
    }
    let sequence = crate::db::physical_attestation::quote_identifier(&name);
    let mut state = tx
        .query(
            &format!("SELECT last_value::bigint, is_called FROM {quoted}.{sequence}"),
            (),
        )
        .await?;
    let state = state
        .next()
        .await?
        .ok_or(PubSubRuntimeAttachError::InvalidSequence)?;
    let last: i64 = state.get(0)?;
    let called: bool = state.get(1)?;
    let mut range = tx
        .query(
            &format!("SELECT min(seq), max(seq) FROM {quoted}.pubsub_items"),
            (),
        )
        .await?;
    let range = range
        .next()
        .await?
        .ok_or(PubSubRuntimeAttachError::InvalidSequence)?;
    let min: Option<i64> = range.get(0)?;
    let max: Option<i64> = range.get(1)?;
    if last < 1
        || last == i64::MAX
        || min.is_some_and(|min| min < 1)
        || max.is_some_and(|max| !called || max > last)
        || (!called && last != 1)
    {
        return Err(PubSubRuntimeAttachError::InvalidSequence);
    }
    Ok(())
}

async fn verify_counter(
    tx: &mut Transaction<'_>,
    quoted: &str,
    counter: &'static str,
    projection: &'static str,
) -> Result<(), PubSubRuntimeAttachError> {
    let sql = format!("SELECT (SELECT count(*) FROM {quoted}.\"{counter}\") <= 1 \
        AND NOT EXISTS (SELECT 1 FROM {quoted}.\"{counter}\" WHERE id <> 1 OR current_version < 0 \
          OR current_version = 9223372036854775807) \
        AND NOT EXISTS (SELECT 1 FROM {quoted}.\"{projection}\" WHERE source_version < 1 \
          OR source_version > COALESCE((SELECT current_version FROM {quoted}.\"{counter}\" WHERE id = 1), 0))");
    let mut rows = tx.query(&sql, ()).await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PubSubRuntimeAttachError::InvalidCounter { component: counter });
    }
    Ok(())
}
