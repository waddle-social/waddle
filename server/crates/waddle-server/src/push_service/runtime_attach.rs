//! Read-only verification of an owner-prepared Push Service schema.

use std::collections::HashMap;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

const MARKER_TABLE: &str = "push_service_schema_marker";
const SCHEMA_VERSION: i64 = 1;
const TABLES: [&str; 7] = [
    "push_owner_locks",
    "push_nodes",
    "push_node_locks",
    "push_devices",
    "push_delivery_attempts",
    "push_publish_jobs",
    "push_publication_orders",
];

#[derive(Debug, thiserror::Error)]
pub enum PushServiceAttachError {
    #[error("Push Service runtime attachment requires PostgreSQL")]
    PostgresRequired,
    #[error("Push Service runtime attachment requires the main admission lane")]
    MainLaneRequired,
    #[error("Push Service runtime attachment requires an attested main pool")]
    AttestedPoolRequired,
    #[error("Push Service runtime attachment found a different active database or schema")]
    ActiveSchemaMismatch,
    #[error("Push Service schema marker is missing, malformed, or incompatible")]
    InvalidMarker,
    #[error("Push Service relation {table} is absent or has an unsafe shape")]
    InvalidTable { table: &'static str },
    #[error("Push Service relation {table} has an unexpected column {column}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("Push Service relation {table} has unexpected constraints")]
    InvalidConstraint { table: &'static str },
    #[error("Push Service relation {table} has unexpected indexes")]
    InvalidIndex { table: &'static str },
    #[error("Push Service relation {table} has an inbound foreign key")]
    InboundForeignKey { table: &'static str },
    #[error("Push Service relation {table} is undergoing a concurrent schema change")]
    ConcurrentSchemaChange { table: &'static str },
    #[error("Push Service runtime role or grants are unsafe: {component}")]
    UnsafeRuntimePrivileges { component: &'static str },
    #[error("could not verify the PostgreSQL {setting} bound for Push Service attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("failed to inspect Push Service relation {table}: {source}")]
    Relation {
        table: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

#[derive(Clone, Copy)]
struct Column {
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    default: Option<&'static str>,
}

#[derive(Clone, Copy)]
struct Check {
    name: &'static str,
    definition: &'static str,
}

#[derive(Clone, Copy)]
struct Index {
    name: &'static str,
    columns: &'static [&'static str],
    unique: bool,
    primary: bool,
    predicate: Option<&'static str>,
}

#[derive(Clone, Copy)]
struct ForeignKey {
    table: &'static str,
    columns: &'static [&'static str],
    referenced_table: &'static str,
    referenced_columns: &'static [&'static str],
}

#[derive(Clone, Copy)]
struct Table {
    name: &'static str,
    columns: &'static [Column],
    primary_key: &'static [&'static str],
    checks: &'static [Check],
    unique_constraints: &'static [&'static [&'static str]],
    indexes: &'static [Index],
    foreign_keys: &'static [ForeignKey],
    insert: bool,
    update: bool,
    delete: bool,
}

const OWNER_LOCK_COLUMNS: &[Column] = &[
    Column {
        name: "owner_bare_jid",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "updated_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
];
const NODE_COLUMNS: &[Column] = &[
    Column {
        name: "node",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "owner_bare_jid",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "app_id",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "status",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "created_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "updated_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
];
const NODE_LOCK_COLUMNS: &[Column] = &[
    Column {
        name: "node",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "updated_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
];
const DEVICE_COLUMNS: &[Column] = &[
    Column {
        name: "device_id",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "node",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "platform",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "environment",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "provider_endpoint",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "provider_token",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "provider_key_material",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "status",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "failure_count",
        kind: "integer",
        not_null: true,
        default: Some("0"),
    },
    Column {
        name: "last_error",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "created_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "updated_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "provider_auth",
        kind: "text",
        not_null: false,
        default: None,
    },
];
const ATTEMPT_COLUMNS: &[Column] = &[
    Column {
        name: "attempt_id",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "node",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "device_id",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "platform",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "item_id",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "status",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "last_error",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "created_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "publish_job_id",
        kind: "text",
        not_null: false,
        default: None,
    },
];
const ORDER_COLUMNS: &[Column] = &[
    Column {
        name: "node",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "next_order",
        kind: "bigint",
        not_null: true,
        default: None,
    },
];
const PUBLISH_JOB_COLUMNS: &[Column] = &[
    Column {
        name: "job_id",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "owner_bare_jid",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "push_service_jid",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "node",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "item_id",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "payload_xml",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "publish_options_xml",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "ancestry_job_id",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "acceptance_scope",
        kind: "text",
        not_null: true,
        default: Some("'legacy'"),
    },
    Column {
        name: "backing_published_at_ms",
        kind: "bigint",
        not_null: false,
        default: None,
    },
    Column {
        name: "publication_order",
        kind: "bigint",
        not_null: true,
        default: Some("0"),
    },
    Column {
        name: "backing_state",
        kind: "text",
        not_null: true,
        default: Some("'pending'"),
    },
    Column {
        name: "uncertain_send",
        kind: "integer",
        not_null: true,
        default: Some("0"),
    },
    Column {
        name: "terminal_disposition",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "upstream_completed",
        kind: "integer",
        not_null: true,
        default: Some("0"),
    },
    Column {
        name: "status",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "attempt_count",
        kind: "integer",
        not_null: true,
        default: Some("0"),
    },
    Column {
        name: "last_error",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "next_retry_at_ms",
        kind: "bigint",
        not_null: false,
        default: None,
    },
    Column {
        name: "claimed_at_ms",
        kind: "bigint",
        not_null: false,
        default: None,
    },
    Column {
        name: "claim_token",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "created_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "updated_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "published_at_ms",
        kind: "bigint",
        not_null: false,
        default: None,
    },
];
const MARKER_COLUMNS: &[Column] = &[
    Column {
        name: "marker_id",
        kind: "integer",
        not_null: true,
        default: None,
    },
    Column {
        name: "version",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "owner_phase",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "system_identifier",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "database_oid",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "database_name",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "schema_oid",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "schema_name",
        kind: "text",
        not_null: true,
        default: None,
    },
];

const NO_CHECKS: &[Check] = &[];
const OWNER_LOCK_INDEXES: &[Index] =
    &[Index::primary("push_owner_locks_pkey", &["owner_bare_jid"])];
const NODE_INDEXES: &[Index] = &[
    Index::primary("push_nodes_pkey", &["node"]),
    Index::unique(
        "push_nodes_owner_bare_jid_app_id_key",
        &["owner_bare_jid", "app_id"],
    ),
    Index::ordinary("idx_push_nodes_owner", &["owner_bare_jid", "status"]),
];
const NODE_LOCK_INDEXES: &[Index] = &[Index::primary("push_node_locks_pkey", &["node"])];
const DEVICE_INDEXES: &[Index] = &[
    Index::primary("push_devices_pkey", &["node", "device_id"]),
    Index::ordinary("idx_push_devices_node_status", &["node", "status"]),
];
const ATTEMPT_INDEXES: &[Index] = &[
    Index::primary("push_delivery_attempts_pkey", &["attempt_id"]),
    Index::ordinary(
        "idx_push_delivery_attempts_node_created",
        &["node", "created_at_ms"],
    ),
    Index::ordinary(
        "idx_push_delivery_attempts_device_created",
        &["device_id", "created_at_ms"],
    ),
    Index::ordinary(
        "idx_push_delivery_attempts_item_status",
        &["node", "item_id", "status", "device_id"],
    ),
    Index::ordinary(
        "idx_push_delivery_attempts_acceptance_status",
        &["publish_job_id", "status", "device_id"],
    ),
];
const ORDER_INDEXES: &[Index] = &[Index::primary("push_publication_orders_pkey", &["node"])];
const PUBLISH_JOB_INDEXES: &[Index] = &[
    Index::primary("push_publish_jobs_pkey", &["job_id"]),
    Index::partial_unique(
        "idx_push_publish_jobs_canonical_delivery",
        &["ancestry_job_id"],
        "ancestry_job_id IS NOT NULL",
    ),
    Index::ordinary(
        "idx_push_publish_jobs_status_created",
        &["status", "created_at_ms"],
    ),
    Index::ordinary("idx_push_publish_jobs_node_status", &["node", "status"]),
];
const MARKER_INDEXES: &[Index] = &[Index::primary(
    "push_service_schema_marker_pkey",
    &["marker_id"],
)];

const NODE_CHECKS: &[Check] = &[Check {
    name: "push_nodes_status_check",
    definition: "status = ANY (ARRAY['active'::text, 'disabled'::text])",
}];
const DEVICE_CHECKS: &[Check] = &[
    Check {
        name: "push_devices_platform_check",
        definition: "platform = ANY (ARRAY['web'::text, 'apns'::text, 'fcm'::text])",
    },
    Check {
        name: "push_devices_status_check",
        definition: "status = ANY (ARRAY['active'::text, 'disabled'::text])",
    },
];
const PUBLISH_JOB_CHECKS: &[Check] = &[
    Check { name: "push_publish_jobs_acceptance_scope_check", definition: "acceptance_scope = ANY (ARRAY['legacy'::text, 'wire'::text, 'canonical'::text])" },
    Check { name: "push_publish_jobs_backing_state_check", definition: "backing_state = ANY (ARRAY['pending'::text, 'published'::text, 'superseded'::text, 'not-configured'::text])" },
    Check { name: "push_publish_jobs_uncertain_send_check", definition: "uncertain_send = ANY (ARRAY[0, 1])" },
    Check { name: "push_publish_jobs_terminal_disposition_check", definition: "terminal_disposition IS NULL OR terminal_disposition = 'registration-revoked'" },
    Check { name: "push_publish_jobs_upstream_completed_check", definition: "upstream_completed = ANY (ARRAY[0, 1])" },
    Check { name: "push_publish_jobs_status_check", definition: "status = ANY (ARRAY['queued'::text, 'in-progress'::text, 'published'::text, 'failed'::text])" },
];
const MARKER_CHECKS: &[Check] = &[
    Check {
        name: "push_service_schema_marker_marker_id_check",
        definition: "marker_id = 1",
    },
    Check {
        name: "push_service_schema_marker_version_check",
        definition: "version = 1",
    },
    Check {
        name: "push_service_schema_marker_owner_phase_check",
        definition: "owner_phase = ANY (ARRAY[0, 1])",
    },
];

const OWNER_LOCK_TABLE: Table = Table {
    name: "push_owner_locks",
    columns: OWNER_LOCK_COLUMNS,
    primary_key: &["owner_bare_jid"],
    checks: NO_CHECKS,
    unique_constraints: &[],
    indexes: OWNER_LOCK_INDEXES,
    foreign_keys: &[],
    insert: true,
    update: true,
    delete: false,
};
const NODE_TABLE: Table = Table {
    name: "push_nodes",
    columns: NODE_COLUMNS,
    primary_key: &["node"],
    checks: NODE_CHECKS,
    unique_constraints: &[&["owner_bare_jid", "app_id"]],
    indexes: NODE_INDEXES,
    foreign_keys: &[],
    insert: true,
    update: true,
    delete: true,
};
const NODE_LOCK_TABLE: Table = Table {
    name: "push_node_locks",
    columns: NODE_LOCK_COLUMNS,
    primary_key: &["node"],
    checks: NO_CHECKS,
    unique_constraints: &[],
    indexes: NODE_LOCK_INDEXES,
    foreign_keys: &[ForeignKey {
        table: "push_node_locks",
        columns: &["node"],
        referenced_table: "push_nodes",
        referenced_columns: &["node"],
    }],
    insert: true,
    update: true,
    delete: false,
};
const DEVICE_TABLE: Table = Table {
    name: "push_devices",
    columns: DEVICE_COLUMNS,
    primary_key: &["node", "device_id"],
    checks: DEVICE_CHECKS,
    unique_constraints: &[],
    indexes: DEVICE_INDEXES,
    foreign_keys: &[ForeignKey {
        table: "push_devices",
        columns: &["node"],
        referenced_table: "push_nodes",
        referenced_columns: &["node"],
    }],
    insert: true,
    update: true,
    delete: true,
};
const ATTEMPT_TABLE: Table = Table {
    name: "push_delivery_attempts",
    columns: ATTEMPT_COLUMNS,
    primary_key: &["attempt_id"],
    checks: NO_CHECKS,
    unique_constraints: &[],
    indexes: ATTEMPT_INDEXES,
    foreign_keys: &[
        ForeignKey {
            table: "push_delivery_attempts",
            columns: &["node"],
            referenced_table: "push_nodes",
            referenced_columns: &["node"],
        },
        ForeignKey {
            table: "push_delivery_attempts",
            columns: &["node", "device_id"],
            referenced_table: "push_devices",
            referenced_columns: &["node", "device_id"],
        },
    ],
    insert: true,
    update: false,
    delete: true,
};
const PUBLISH_JOB_TABLE: Table = Table {
    name: "push_publish_jobs",
    columns: PUBLISH_JOB_COLUMNS,
    primary_key: &["job_id"],
    checks: PUBLISH_JOB_CHECKS,
    unique_constraints: &[],
    indexes: PUBLISH_JOB_INDEXES,
    foreign_keys: &[ForeignKey {
        table: "push_publish_jobs",
        columns: &["node"],
        referenced_table: "push_nodes",
        referenced_columns: &["node"],
    }],
    insert: true,
    update: true,
    delete: true,
};
const ORDER_TABLE: Table = Table {
    name: "push_publication_orders",
    columns: ORDER_COLUMNS,
    primary_key: &["node"],
    checks: NO_CHECKS,
    unique_constraints: &[],
    indexes: ORDER_INDEXES,
    foreign_keys: &[],
    insert: true,
    update: true,
    delete: false,
};
const MARKER: Table = Table {
    name: MARKER_TABLE,
    columns: MARKER_COLUMNS,
    primary_key: &["marker_id"],
    checks: MARKER_CHECKS,
    unique_constraints: &[],
    indexes: MARKER_INDEXES,
    foreign_keys: &[],
    insert: false,
    update: false,
    delete: false,
};
const PUSH_TABLES: &[Table] = &[
    OWNER_LOCK_TABLE,
    NODE_TABLE,
    NODE_LOCK_TABLE,
    DEVICE_TABLE,
    ATTEMPT_TABLE,
    PUBLISH_JOB_TABLE,
    ORDER_TABLE,
];

impl Index {
    const fn primary(name: &'static str, columns: &'static [&'static str]) -> Self {
        Self {
            name,
            columns,
            unique: true,
            primary: true,
            predicate: None,
        }
    }
    const fn unique(name: &'static str, columns: &'static [&'static str]) -> Self {
        Self {
            name,
            columns,
            unique: true,
            primary: false,
            predicate: None,
        }
    }
    const fn partial_unique(
        name: &'static str,
        columns: &'static [&'static str],
        predicate: &'static str,
    ) -> Self {
        Self {
            name,
            columns,
            unique: true,
            primary: false,
            predicate: Some(predicate),
        }
    }
    const fn ordinary(name: &'static str, columns: &'static [&'static str]) -> Self {
        Self {
            name,
            columns,
            unique: false,
            primary: false,
            predicate: None,
        }
    }
}

pub(super) async fn verify(db: &Database) -> Result<(), PushServiceAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(PushServiceAttachError::PostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(PushServiceAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(PushServiceAttachError::AttestedPoolRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;
    let identity = capability.identity();
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "Push Service runtime schema")
        .map_err(|_| PushServiceAttachError::ActiveSchemaMismatch)?;
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);

    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|error| match error {
            crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                PushServiceAttachError::InvalidTimeoutSetting { setting }
            }
            crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                PushServiceAttachError::Database(error)
            }
        })?;

    // Hold catalog-stable locks before inspection. Read committed avoids an
    // old snapshot making a concurrent index build wait on this transaction.
    // SUE blocks concurrent regular and concurrent index builds while allowing
    // ordinary Push Service row writers to proceed.
    for table in TABLES {
        let qualified = format!("{quoted_schema}.{}", quote_static_identifier(table));
        if let Err(source) = tx
            .execute(
                &format!("LOCK TABLE {qualified} IN SHARE UPDATE EXCLUSIVE MODE NOWAIT"),
                (),
            )
            .await
        {
            if matches!(
                &source,
                DatabaseError::Internal(sqlx::Error::Database(error))
                    if error.code().as_deref() == Some("55P03")
            ) {
                return Err(PushServiceAttachError::ConcurrentSchemaChange { table });
            }
            return Err(PushServiceAttachError::Relation { table, source });
        }
    }
    let marker = format!("{quoted_schema}.{}", quote_static_identifier(MARKER_TABLE));
    tx.execute(&format!("LOCK TABLE {marker} IN ACCESS SHARE MODE"), ())
        .await
        .map_err(|source| PushServiceAttachError::Relation {
            table: MARKER_TABLE,
            source,
        })?;
    capability.attest_transaction(&mut tx).await?;
    verify_active_schema(&mut tx, &identity).await?;
    verify_runtime_role(&mut tx, schema).await?;
    if !verify_marker_shape_and_data_tx(&mut tx, &identity).await? {
        return Err(PushServiceAttachError::InvalidMarker);
    }
    verify_table(&mut tx, schema, &MARKER).await?;
    verify_table_acl(&mut tx, schema, &MARKER).await?;
    for table in PUSH_TABLES {
        verify_table(&mut tx, schema, table).await?;
        verify_table_acl(&mut tx, schema, table).await?;
    }
    tx.rollback().await?;
    Ok(())
}

pub(super) async fn verify_marker_shape_and_data_tx(
    tx: &mut Transaction<'_>,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<bool, PushServiceAttachError> {
    let schema = &identity.schema.name.0;
    verify_table(tx, schema, &MARKER).await?;
    verify_marker_data(tx, schema, identity).await
}

async fn verify_active_schema(
    tx: &mut Transaction<'_>,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<(), PushServiceAttachError> {
    let mut rows = tx
        .query(
            "SELECT namespace.oid::bigint = ? \
               AND namespace.nspname = pg_catalog.current_schema() \
               AND namespace.nspname = (pg_catalog.current_schemas(false))[1]::text \
               AND pg_catalog.cardinality(pg_catalog.current_schemas(false)) <= 2 \
               AND (pg_catalog.cardinality(pg_catalog.current_schemas(false)) = 1 \
                 OR pg_catalog.left((pg_catalog.current_schemas(false))[2]::text, 8) = 'pg_temp_') \
               AND database.oid::bigint = ? AND database.datname = ? \
             FROM pg_catalog.pg_namespace namespace \
             JOIN pg_catalog.pg_database database \
               ON database.datname = pg_catalog.current_database() \
             WHERE namespace.nspname = ?",
            crate::db_params![
                i64::from(identity.schema.oid),
                i64::from(identity.database.oid),
                identity.database.name.0.clone(),
                identity.schema.name.0.clone(),
            ],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PushServiceAttachError::ActiveSchemaMismatch);
    }
    Ok(())
}

async fn verify_marker_data(
    tx: &mut Transaction<'_>,
    schema: &str,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<bool, PushServiceAttachError> {
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    let quoted_table = quote_static_identifier(MARKER_TABLE);
    let mut rows = tx
        .query(
            &format!(
                "SELECT version, owner_phase, system_identifier, database_oid, database_name, schema_oid, schema_name \
                 FROM {quoted_schema}.{quoted_table} WHERE marker_id = 1"
            ),
            (),
        )
        .await
        .map_err(|source| PushServiceAttachError::Relation {
            table: MARKER_TABLE,
            source,
        })?;
    let row = rows
        .next()
        .await?
        .ok_or(PushServiceAttachError::InvalidMarker)?;
    let version = row.get::<i64>(0)?;
    let owner_phase = row.get::<i64>(1)?;
    let system_identifier = row.get::<String>(2)?;
    let database_oid = row.get::<i64>(3)?;
    let database_name = row.get::<String>(4)?;
    let schema_oid = row.get::<i64>(5)?;
    let schema_name = row.get::<String>(6)?;
    let matches = version == SCHEMA_VERSION && (owner_phase == 0 || owner_phase == 1);
    let matches = matches
        && system_identifier == identity.system_identifier.to_string()
        && database_oid == i64::from(identity.database.oid)
        && database_name == identity.database.name.0
        && schema_oid == i64::from(identity.schema.oid)
        && schema_name == identity.schema.name.0;
    drop(rows);
    if !matches {
        return Err(PushServiceAttachError::InvalidMarker);
    }
    let mut rows = tx
        .query(
            &format!("SELECT count(*)::bigint FROM {quoted_schema}.{quoted_table}"),
            (),
        )
        .await
        .map_err(|source| PushServiceAttachError::Relation {
            table: MARKER_TABLE,
            source,
        })?;
    if !matches!(rows.next().await?, Some(row) if row.get::<i64>(0)? == 1) {
        return Err(PushServiceAttachError::InvalidMarker);
    }
    Ok(owner_phase == 1)
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PushServiceAttachError> {
    let mut rows = tx
        .query(
            "SELECT relation.relkind = 'r' AND relation.relpersistence = 'p' \
               AND NOT relation.relispartition AND relation.relowner = namespace.nspowner \
               AND NOT relation.relrowsecurity AND NOT relation.relforcerowsecurity \
               AND relation.reloftype = 0 \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger trigger \
                               WHERE trigger.tgrelid = relation.oid AND NOT trigger.tgisinternal) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy policy \
                               WHERE policy.polrelid = relation.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rewrite \
                               WHERE rewrite.ev_class = relation.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inheritance \
                               WHERE inheritance.inhrelid = relation.oid \
                                  OR inheritance.inhparent = relation.oid), \
               (SELECT count(*)::bigint FROM pg_catalog.pg_attribute attribute \
                WHERE attribute.attrelid = relation.oid AND attribute.attnum > 0 \
                  AND NOT attribute.attisdropped) \
             FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname = ?",
            crate::db_params![schema, table.name],
        )
        .await
        .map_err(|source| PushServiceAttachError::Relation {
            table: table.name,
            source,
        })?;
    let Some(row) = rows.next().await? else {
        return Err(PushServiceAttachError::InvalidTable { table: table.name });
    };
    if !row.get::<bool>(0)? || row.get::<i64>(1)? != table.columns.len() as i64 {
        return Err(PushServiceAttachError::InvalidTable { table: table.name });
    }
    drop(rows);
    verify_columns(tx, schema, table).await?;
    verify_constraints(tx, schema, table).await?;
    verify_indexes(tx, schema, table).await?;
    verify_foreign_keys(tx, schema, table).await?;
    Ok(())
}

async fn verify_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PushServiceAttachError> {
    let mut rows = tx
        .query(
            "SELECT attribute.attname::text, pg_catalog.format_type(attribute.atttypid, attribute.atttypmod), \
               attribute.attnotnull, pg_catalog.pg_get_expr(default_value.adbin, default_value.adrelid), \
               attribute.attgenerated::text, attribute.attidentity::text, \
               attribute.attcollation = type_row.typcollation \
             FROM pg_catalog.pg_attribute attribute \
             JOIN pg_catalog.pg_class relation ON relation.oid = attribute.attrelid \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             JOIN pg_catalog.pg_type type_row ON type_row.oid = attribute.atttypid \
             LEFT JOIN pg_catalog.pg_attrdef default_value \
               ON default_value.adrelid = relation.oid AND default_value.adnum = attribute.attnum \
             WHERE namespace.nspname = ? AND relation.relname = ? \
               AND attribute.attnum > 0 AND NOT attribute.attisdropped",
            crate::db_params![schema, table.name],
        )
        .await
        .map_err(|source| PushServiceAttachError::Relation {
            table: table.name,
            source,
        })?;
    let mut actual = HashMap::new();
    while let Some(row) = rows.next().await? {
        actual.insert(
            row.get::<String>(0)?,
            (
                row.get::<String>(1)?,
                row.get::<bool>(2)?,
                row.get::<Option<String>>(3)?,
                row.get::<String>(4)?,
                row.get::<String>(5)?,
                row.get::<bool>(6)?,
            ),
        );
    }
    if actual.len() != table.columns.len() {
        return Err(PushServiceAttachError::InvalidTable { table: table.name });
    }
    for expected in table.columns {
        let Some((kind, not_null, default, generated, identity, collation)) =
            actual.get(expected.name)
        else {
            return Err(PushServiceAttachError::InvalidColumn {
                table: table.name,
                column: expected.name,
            });
        };
        if kind != expected.kind
            || *not_null != expected.not_null
            || normalize_sql(default.as_deref().unwrap_or("")).as_str()
                != normalize_sql(expected.default.unwrap_or("")).as_str()
            || !generated.is_empty()
            || !identity.is_empty()
            || !collation
        {
            return Err(PushServiceAttachError::InvalidColumn {
                table: table.name,
                column: expected.name,
            });
        }
    }
    Ok(())
}

async fn verify_constraints(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PushServiceAttachError> {
    let mut rows = tx
        .query(
            "SELECT constraint_row.conname::text, constraint_row.contype::text, \
               constraint_row.condeferrable, constraint_row.condeferred, constraint_row.convalidated, \
               pg_catalog.pg_get_expr(constraint_row.conbin, constraint_row.conrelid) \
             FROM pg_catalog.pg_constraint constraint_row \
             JOIN pg_catalog.pg_class relation ON relation.oid = constraint_row.conrelid \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname = ? \
             ORDER BY constraint_row.conname",
            crate::db_params![schema, table.name],
        )
        .await
        .map_err(|source| PushServiceAttachError::Relation {
            table: table.name,
            source,
        })?;
    let mut primary_key = false;
    let mut checks = HashMap::new();
    let mut unique = Vec::new();
    let mut other = false;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let deferrable: bool = row.get(2)?;
        let deferred: bool = row.get(3)?;
        let validated: bool = row.get(4)?;
        let definition: Option<String> = row.get(5)?;
        if deferrable || deferred || !validated {
            return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
        }
        match kind.as_str() {
            "p" => {
                primary_key =
                    verify_key_constraint(tx, schema, table.name, &name, "p", table.primary_key)
                        .await?;
            }
            "u" => {
                unique.push(verify_key_constraint(tx, schema, table.name, &name, "u", &[]).await?)
            }
            "c" => {
                let Some(definition) = definition else {
                    return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
                };
                checks.insert(name, normalize_sql(&definition));
            }
            "f" => {}
            _ => other = true,
        }
    }
    if !primary_key || other {
        return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
    }
    let mut actual_unique = Vec::new();
    for columns in table.unique_constraints {
        let mut found = false;
        for name in unique_constraint_names(table.name, columns) {
            if unique_constraint_matches(tx, schema, table.name, &name, columns).await? {
                found = true;
                break;
            }
        }
        actual_unique.push(found);
    }
    if actual_unique.iter().any(|value| !value) || unique.len() != table.unique_constraints.len() {
        return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
    }
    if checks.len() != table.checks.len() {
        return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
    }
    for check in table.checks {
        let Some(actual) = checks.get(check.name) else {
            return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
        };
        if actual != &normalize_sql(check.definition) {
            return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
        }
    }
    Ok(())
}

async fn verify_key_constraint(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
    constraint_name: &str,
    kind: &str,
    expected_columns: &[&str],
) -> Result<bool, PushServiceAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.array_to_string(ARRAY( \
               SELECT attribute.attname::text FROM pg_catalog.unnest(constraint_row.conkey) \
               WITH ORDINALITY key_column(attnum, position) \
               JOIN pg_catalog.pg_attribute attribute \
                 ON attribute.attrelid = relation.oid AND attribute.attnum = key_column.attnum \
               ORDER BY key_column.position), ',') \
             FROM pg_catalog.pg_constraint constraint_row \
             JOIN pg_catalog.pg_class relation ON relation.oid = constraint_row.conrelid \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname = ? \
               AND constraint_row.conname = ? AND constraint_row.contype = ?",
            crate::db_params![schema, table, constraint_name, kind],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    let actual: String = row.get(0)?;
    Ok(expected_columns.is_empty() || actual.split(',').collect::<Vec<_>>() == expected_columns)
}

fn unique_constraint_names(table: &str, columns: &[&str]) -> Vec<String> {
    if table == "push_nodes" && columns == ["owner_bare_jid", "app_id"] {
        vec!["push_nodes_owner_bare_jid_app_id_key".to_owned()]
    } else {
        Vec::new()
    }
}

async fn unique_constraint_matches(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
    constraint_name: &str,
    expected_columns: &[&str],
) -> Result<bool, PushServiceAttachError> {
    verify_key_constraint(tx, schema, table, constraint_name, "u", expected_columns).await
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PushServiceAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.count(*)::bigint FROM pg_catalog.pg_index index_row \
             JOIN pg_catalog.pg_class relation ON relation.oid = index_row.indrelid \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname = ?",
            crate::db_params![schema, table.name],
        )
        .await?;
    let count = rows
        .next()
        .await?
        .ok_or(PushServiceAttachError::InvalidIndex { table: table.name })?
        .get::<i64>(0)?;
    drop(rows);
    if count != table.indexes.len() as i64 {
        return Err(PushServiceAttachError::InvalidIndex { table: table.name });
    }
    for expected in table.indexes {
        let mut rows = tx
            .query(
                "SELECT index_row.indisunique, index_row.indisprimary, index_row.indisvalid, \
                   index_row.indisready, index_row.indislive, index_row.indpred IS NULL, \
                   index_row.indexprs IS NULL, access_method.amname = 'btree', \
                   index_row.indnkeyatts::bigint, index_row.indnatts::bigint, \
                   pg_catalog.array_to_string(ARRAY( \
                     SELECT attribute.attname::text \
                     FROM pg_catalog.unnest(index_row.indkey) WITH ORDINALITY key_column(attnum, position) \
                     JOIN pg_catalog.pg_attribute attribute \
                       ON attribute.attrelid = relation.oid AND attribute.attnum = key_column.attnum \
                     WHERE key_column.position <= index_row.indnkeyatts ORDER BY key_column.position), ','), \
                   (SELECT COALESCE(pg_catalog.bool_and(option = 0), true) \
                    FROM pg_catalog.unnest(index_row.indoption) WITH ORDINALITY option_row(option, position) \
                    WHERE option_row.position <= index_row.indnkeyatts), \
                   pg_catalog.pg_get_expr(index_row.indpred, index_row.indrelid), \
                   NOT EXISTS ( \
                     SELECT 1 \
                     FROM pg_catalog.unnest(index_row.indclass) WITH ORDINALITY indexed_class(class_oid, position) \
                     JOIN pg_catalog.unnest(index_row.indcollation) WITH ORDINALITY indexed_collation(collation_oid, position) USING (position) \
                     JOIN pg_catalog.unnest(index_row.indkey) WITH ORDINALITY key_column(attnum, position) USING (position) \
                     JOIN pg_catalog.pg_attribute attribute \
                       ON attribute.attrelid = relation.oid AND attribute.attnum = key_column.attnum \
                     WHERE indexed_class.position <= index_row.indnkeyatts \
                       AND (indexed_collation.collation_oid <> attribute.attcollation \
                         OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_opclass opclass \
                           WHERE opclass.oid = indexed_class.class_oid \
                             AND opclass.opcmethod = access_method.oid \
                             AND opclass.opcintype = attribute.atttypid \
                             AND opclass.opcdefault)) \
                   ) \
                 FROM pg_catalog.pg_index index_row \
                 JOIN pg_catalog.pg_class relation ON relation.oid = index_row.indrelid \
                 JOIN pg_catalog.pg_class index_relation ON index_relation.oid = index_row.indexrelid \
                 JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
                 JOIN pg_catalog.pg_am access_method ON access_method.oid = index_relation.relam \
                 WHERE namespace.nspname = ? AND relation.relname = ? \
                   AND index_relation.relname = ?",
                crate::db_params![schema, table.name, expected.name],
            )
            .await
            .map_err(|source| PushServiceAttachError::Relation { table: table.name, source })?;
        let Some(row) = rows.next().await? else {
            return Err(PushServiceAttachError::InvalidIndex { table: table.name });
        };
        let columns: String = row.get(10)?;
        let predicate: Option<String> = row.get(12)?;
        if row.get::<bool>(0)? != expected.unique
            || row.get::<bool>(1)? != expected.primary
            || !row.get::<bool>(2)?
            || !row.get::<bool>(3)?
            || !row.get::<bool>(4)?
            || row.get::<bool>(5)? != expected.predicate.is_none()
            || !row.get::<bool>(6)?
            || !row.get::<bool>(7)?
            || row.get::<i64>(8)? != expected.columns.len() as i64
            || row.get::<i64>(9)? != expected.columns.len() as i64
            || columns.split(',').collect::<Vec<_>>() != expected.columns
            || !row.get::<bool>(11)?
            || normalize_sql(predicate.as_deref().unwrap_or("")).as_str()
                != normalize_sql(expected.predicate.unwrap_or("")).as_str()
            || !row.get::<bool>(13)?
        {
            return Err(PushServiceAttachError::InvalidIndex { table: table.name });
        }
    }
    Ok(())
}

async fn verify_foreign_keys(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PushServiceAttachError> {
    let mut rows = tx
        .query(
            "SELECT source_namespace.nspname::text, source.relname::text, \
               pg_catalog.array_to_string(ARRAY(SELECT source_column.attname::text \
                 FROM pg_catalog.unnest(foreign_key.conkey) WITH ORDINALITY key_column(attnum, position) \
                 JOIN pg_catalog.pg_attribute source_column \
                   ON source_column.attrelid = source.oid AND source_column.attnum = key_column.attnum \
                 ORDER BY key_column.position), ','), \
               target_namespace.nspname::text, target.relname::text, \
               pg_catalog.array_to_string(ARRAY(SELECT target_column.attname::text \
                 FROM pg_catalog.unnest(foreign_key.confkey) WITH ORDINALITY key_column(attnum, position) \
                 JOIN pg_catalog.pg_attribute target_column \
                   ON target_column.attrelid = target.oid AND target_column.attnum = key_column.attnum \
                 ORDER BY key_column.position), ','), \
               foreign_key.confdeltype::text, foreign_key.confupdtype::text, \
               foreign_key.condeferrable, foreign_key.convalidated \
             FROM pg_catalog.pg_constraint foreign_key \
             JOIN pg_catalog.pg_class source ON source.oid = foreign_key.conrelid \
             JOIN pg_catalog.pg_class target ON target.oid = foreign_key.confrelid \
             JOIN pg_catalog.pg_namespace source_namespace ON source_namespace.oid = source.relnamespace \
             JOIN pg_catalog.pg_namespace target_namespace ON target_namespace.oid = target.relnamespace \
             WHERE foreign_key.contype = 'f' \
               AND ((source_namespace.nspname = ? AND source.relname = ?) \
                 OR (target_namespace.nspname = ? AND target.relname = ?))",
            crate::db_params![schema, table.name, schema, table.name],
        )
        .await
        .map_err(|source| PushServiceAttachError::Relation { table: table.name, source })?;
    let mut actual = Vec::new();
    while let Some(row) = rows.next().await? {
        actual.push((
            row.get::<String>(0)?,
            row.get::<String>(1)?,
            row.get::<String>(2)?,
            row.get::<String>(3)?,
            row.get::<String>(4)?,
            row.get::<String>(5)?,
            row.get::<String>(6)?,
            row.get::<String>(7)?,
            row.get::<bool>(8)?,
            row.get::<bool>(9)?,
        ));
    }

    let expected_incoming: Vec<&ForeignKey> = PUSH_TABLES
        .iter()
        .flat_map(|source| source.foreign_keys.iter())
        .filter(|foreign_key| foreign_key.referenced_table == table.name)
        .collect();
    let incoming: Vec<_> = actual
        .iter()
        .filter(|foreign_key| foreign_key.3 == schema && foreign_key.4 == table.name)
        .collect();
    for foreign_key in &incoming {
        if !expected_incoming
            .iter()
            .any(|expected| observed_foreign_key_matches(foreign_key, schema, expected))
        {
            return Err(PushServiceAttachError::InboundForeignKey { table: table.name });
        }
    }
    if incoming.len() != expected_incoming.len() {
        return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
    }

    let outgoing: Vec<_> = actual
        .iter()
        .filter(|foreign_key| foreign_key.0 == schema && foreign_key.1 == table.name)
        .collect();
    if outgoing.len() != table.foreign_keys.len() {
        return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
    }
    for expected in table.foreign_keys {
        if !outgoing
            .iter()
            .any(|observed| observed_foreign_key_matches(observed, schema, expected))
        {
            return Err(PushServiceAttachError::InvalidConstraint { table: table.name });
        }
    }
    Ok(())
}

fn observed_foreign_key_matches(
    observed: &(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        bool,
        bool,
    ),
    schema: &str,
    expected: &ForeignKey,
) -> bool {
    observed.0 == schema
        && observed.1 == expected.table
        && observed.2 == expected.columns.join(",")
        && observed.3 == schema
        && observed.4 == expected.referenced_table
        && observed.5 == expected.referenced_columns.join(",")
        && observed.6 == "c"
        && observed.7 == "a"
        && !observed.8
        && observed.9
}

async fn verify_runtime_role(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PushServiceAttachError> {
    let mut rows = tx
        .query(
            "SELECT runtime.rolcanlogin AND NOT runtime.rolsuper \
               AND NOT runtime.rolcreatedb AND NOT runtime.rolcreaterole \
               AND NOT runtime.rolreplication AND NOT runtime.rolbypassrls \
               AND session_user::text = runtime.rolname \
               AND current_user::text = runtime.rolname \
               AND runtime.oid <> database.datdba AND runtime.oid <> namespace.nspowner \
               AND owner.oid <> database.datdba AND NOT owner.rolcanlogin \
               AND NOT owner.rolsuper AND NOT owner.rolcreatedb \
               AND NOT owner.rolcreaterole AND NOT owner.rolreplication \
               AND NOT owner.rolbypassrls \
               AND NOT pg_catalog.has_database_privilege(owner.oid, database.oid, 'TEMP') \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                 WHERE membership.member = owner.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                 WHERE membership.member = runtime.oid OR membership.roleid = runtime.oid) \
               AND NOT pg_catalog.pg_has_role(runtime.oid, owner.oid, 'SET') \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc procedure \
                 WHERE procedure.pronamespace = namespace.oid AND procedure.proowner = runtime.oid) \
               AND pg_catalog.has_schema_privilege(runtime.oid, namespace.oid, 'USAGE') \
               AND NOT pg_catalog.has_schema_privilege(runtime.oid, namespace.oid, 'CREATE') \
               AND NOT pg_catalog.has_database_privilege(runtime.oid, database.oid, 'CREATE') \
               AND NOT pg_catalog.has_database_privilege(runtime.oid, database.oid, 'TEMP') \
               AND NOT pg_catalog.has_parameter_privilege(runtime.oid, 'session_replication_role', 'SET') \
               AND pg_catalog.current_setting('session_replication_role') = 'origin' \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                 COALESCE(namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                 WHERE acl.grantee = 0 OR (acl.grantee <> namespace.nspowner \
                   AND (acl.privilege_type <> 'USAGE' OR acl.is_grantable))) \
             FROM pg_catalog.pg_roles runtime \
             JOIN pg_catalog.pg_namespace namespace ON namespace.nspname = ? \
             JOIN pg_catalog.pg_roles owner ON owner.oid = namespace.nspowner \
             JOIN pg_catalog.pg_database database ON database.datname = pg_catalog.current_database() \
             WHERE runtime.rolname = current_user",
            crate::db_params![schema],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PushServiceAttachError::UnsafeRuntimePrivileges {
            component: "runtime role",
        });
    }
    Ok(())
}

async fn verify_table_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), PushServiceAttachError> {
    let insert = if table.insert { "TRUE" } else { "FALSE" };
    let update = if table.update { "TRUE" } else { "FALSE" };
    let delete = if table.delete { "TRUE" } else { "FALSE" };
    let mut rows = tx
        .query(
            &format!(
            "SELECT relation.relowner = namespace.nspowner \
               AND relation.relowner <> runtime.oid \
               AND NOT pg_catalog.pg_has_role(runtime.oid, relation.relowner, 'SET') \
               AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT') \
               AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT') = {insert} \
               AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'UPDATE') = {update} \
               AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'DELETE') = {delete} \
               AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'TRUNCATE,REFERENCES,TRIGGER') \
               AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, \
                 'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION,TRIGGER WITH GRANT OPTION') \
               AND NOT pg_catalog.has_any_column_privilege(runtime.oid, relation.oid, 'REFERENCES') \
               AND NOT pg_catalog.has_any_column_privilege(runtime.oid, relation.oid, \
                 'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION') \
               AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
                 THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN') ELSE true END \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                 COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))) acl \
                 WHERE acl.grantee NOT IN (relation.relowner, runtime.oid)) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute attribute \
                 CROSS JOIN LATERAL pg_catalog.aclexplode(attribute.attacl) acl \
                 WHERE attribute.attrelid = relation.oid AND attribute.attnum > 0 \
                   AND NOT attribute.attisdropped AND acl.grantee <> relation.relowner) \
             FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             JOIN pg_catalog.pg_roles runtime ON runtime.rolname = current_user \
             WHERE namespace.nspname = ? AND relation.relname = ?",
            ),
            crate::db_params![schema, table.name],
        )
        .await
        .map_err(|source| PushServiceAttachError::Relation { table: table.name, source })?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PushServiceAttachError::UnsafeRuntimePrivileges {
            component: table.name,
        });
    }
    Ok(())
}

fn quote_static_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn normalize_sql(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .replace("::text[]", "")
        .replace("::smallint[]", "")
        .replace("::text", "")
        .replace("::integer", "")
        .replace("::bigint", "")
        .replace("::smallint", "")
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '(' && *character != ')')
        .collect()
}
