//! Read-only attachment of the owner-prepared PostgreSQL notification outbox.
//!
//! The attachment verifies one bounded, read-only snapshot on the supplied
//! physically attested main pool. Owner preparation and attachment share the
//! schema advisory gate; attachment never reconnects, repairs, or migrates.

use thiserror::Error;

use crate::db::{CatalogFenceLock, Database, DatabaseDriver, DatabaseError, Transaction};

use super::schema::{
    predicate_tokens, COALESCE_COLUMNS, COALESCE_INDEX_NAME, COALESCE_PREDICATE,
    NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME, NOTIFICATION_CANDIDATES_CLASS_VALUES,
    NOTIFICATION_CANDIDATES_INDEXES, NOTIFICATION_CANDIDATES_REASON_CHECK_NAME,
    NOTIFICATION_CANDIDATES_REASON_VALUES, NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_NAME,
    NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES, NOTIFICATION_OUTBOX_CLASS_CHECK_NAME,
    NOTIFICATION_OUTBOX_INDEXES, OUTBOX_OWNER_PREPARED_REVISION, OUTBOX_OWNER_STATE_TABLE,
    OUTBOX_SCHEMA_GATE,
};

pub(crate) const CUSTODY_GATE: CatalogFenceLock =
    CatalogFenceLock::SharedPair(OUTBOX_SCHEMA_GATE.0, OUTBOX_SCHEMA_GATE.1);

#[derive(Debug, Error)]
pub enum NotificationOutboxRuntimeAttachError {
    #[error("notification outbox runtime attachment requires PostgreSQL")]
    PostgresRequired,
    #[error("notification outbox runtime attachment requires the main database lane")]
    MainLaneRequired,
    #[error("notification outbox runtime attachment requires a physically attested pool")]
    AttestedPoolRequired,
    #[error("notification outbox schema identity is invalid")]
    InvalidSchema,
    #[error("notification outbox schema is not the active attested schema")]
    ActiveSchemaMismatch,
    #[error("notification outbox owner preparation is missing or incomplete")]
    OwnerPreparationIncomplete,
    #[error("notification outbox owner transaction does not match the expected physical identity")]
    OwnerIdentityMismatch,
    #[error("notification outbox relation {table} is missing or has an invalid shape")]
    InvalidRelation { table: &'static str },
    #[error("notification outbox relation {table} has an invalid column contract")]
    InvalidColumns { table: &'static str },
    #[error("notification outbox relation {table} has an invalid key or check constraint")]
    InvalidConstraints { table: &'static str },
    #[error("notification outbox relation {table} has an invalid index contract")]
    InvalidIndexes { table: &'static str },
    #[error("the PostgreSQL runtime role or notification outbox grants are unsafe")]
    UnsafeRuntimeRole,
    #[error("could not verify the PostgreSQL {setting} bound for notification outbox attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("notification outbox relation lock failed: {0}")]
    RelationLock(#[source] DatabaseError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

#[derive(Clone, Copy)]
struct ColumnContract {
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    default: &'static str,
}

const CANDIDATE_COLUMNS: &[ColumnContract] = &[
    column("recipient_bare_jid", "text", true, ""),
    column("conversation_jid", "text", true, ""),
    column("sender_jid", "text", true, ""),
    column("thread_id", "text", true, "''"),
    column("stanza_id_by", "text", true, ""),
    column("stanza_id", "text", true, ""),
    column("class", "text", true, ""),
    column("reason", "text", true, ""),
    column("created_at_ms", "bigint", true, ""),
    column("policy_error_count", "integer", true, "0"),
    column("next_attempt_at_ms", "bigint", false, ""),
    column("outboxed_at_ms", "bigint", false, ""),
    column("suppressed_reason", "text", false, ""),
    column("noping", "integer", true, "0"),
    column("no_store", "integer", true, "0"),
    column("no_permanent_store", "integer", true, "0"),
    column("last_message_body", "text", false, ""),
    column("reaction", "integer", true, "0"),
    column("delivery_id", "text", false, ""),
    column("quarantined_at_ms", "bigint", false, ""),
    column("quarantined_suppressed_reason", "text", false, ""),
];

const OUTBOX_COLUMNS: &[ColumnContract] = &[
    column("job_id", "text", true, ""),
    column("recipient_bare_jid", "text", true, ""),
    column("push_service_jid", "text", true, ""),
    column("node", "text", true, ""),
    column("conversation_jid", "text", true, ""),
    column("sender_jid", "text", true, ""),
    column("sender_jids", "text", true, ""),
    column("thread_id", "text", true, "''"),
    column("class", "text", true, ""),
    column("message_count", "integer", true, ""),
    column("context_xml", "text", true, ""),
    column("summary_sender_jid", "text", false, ""),
    column("summary_body", "text", false, ""),
    column("approved_payload_xml", "text", false, ""),
    column("approved_publish_options_xml", "text", false, ""),
    column("queue_acceptance_may_exist", "integer", true, "1"),
    column("status", "text", true, ""),
    column("attempt_count", "integer", true, "0"),
    column("policy_error_count", "integer", true, "0"),
    column("last_error", "text", false, ""),
    column("next_attempt_at_ms", "bigint", false, ""),
    column("claimed_at_ms", "bigint", false, ""),
    column("claim_token", "text", false, ""),
    column("created_at_ms", "bigint", true, ""),
    column("updated_at_ms", "bigint", true, ""),
    column("published_at_ms", "bigint", false, ""),
];

const LINEAGE_COLUMNS: &[ColumnContract] = &[
    column("candidate_delivery_id", "text", true, ""),
    column("job_id", "text", true, ""),
    column("settled_at_ms", "bigint", false, ""),
];
const OWNER_STATE_COLUMNS: &[ColumnContract] = &[
    column("singleton", "integer", true, ""),
    column("prepared_revision", "bigint", true, ""),
];

const CANDIDATE_PK: &[&str] = &[
    "recipient_bare_jid",
    "conversation_jid",
    "thread_id",
    "stanza_id_by",
    "stanza_id",
    "class",
];
const OUTBOX_PK: &[&str] = &["job_id"];
const LINEAGE_PK: &[&str] = &["candidate_delivery_id", "job_id"];
const OWNER_STATE_PK: &[&str] = &["singleton"];

const fn column(
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    default: &'static str,
) -> ColumnContract {
    ColumnContract {
        name,
        kind,
        not_null,
        default,
    }
}

/// Validate and attach to the same attested pool handle supplied by startup.
pub(super) async fn attach_existing(
    db: Database,
) -> Result<super::NotificationOutboxStore, NotificationOutboxRuntimeAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(NotificationOutboxRuntimeAttachError::PostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(NotificationOutboxRuntimeAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(NotificationOutboxRuntimeAttachError::AttestedPoolRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;
    let identity = capability.identity();
    let schema_identity = &identity.schema;
    let schema = schema_identity.name.0.as_str();
    crate::db::physical_attestation::validate_identifier(schema, "notification outbox schema")
        .map_err(|_| NotificationOutboxRuntimeAttachError::InvalidSchema)?;

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
                NotificationOutboxRuntimeAttachError::InvalidTimeoutSetting { setting }
            }
            crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                NotificationOutboxRuntimeAttachError::Database(error)
            }
        })?;

    lock_runtime_attach_relations_tx(&mut tx, schema).await?;

    capability.attest_transaction(&mut tx).await?;
    verify_runtime_attach_tx(&mut tx, identity).await?;
    tx.rollback().await?;
    Ok(super::NotificationOutboxStore {
        db,
        owner_preparation: None,
    })
}

/// Acquire the notification outbox schema gate and qualified catalog-stable
/// relation locks on a caller-owned transaction. Joint custody diagnostics
/// use this before acquiring the Push Service gate so owner ancestry adoption
/// and runtime inspection share the same lock order.
pub(crate) async fn lock_runtime_attach_relations_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    tx.execute(
        "SELECT pg_catalog.pg_advisory_xact_lock_shared(?::integer, ?::integer)",
        crate::db_params![OUTBOX_SCHEMA_GATE.0, OUTBOX_SCHEMA_GATE.1],
    )
    .await?;

    lock_runtime_attach_relations_after_gate_tx(tx, schema).await
}

/// Lock relations after the caller has acquired the outbox schema gate.
pub(crate) async fn lock_runtime_attach_relations_after_gate_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    let qschema = crate::db::physical_attestation::quote_identifier(schema);
    let lock_sql = format!(
        "LOCK TABLE {qschema}.\"notification_candidates\", \
         {qschema}.\"notification_outbox_lineage\", \
         {qschema}.\"notification_outbox\" IN SHARE UPDATE EXCLUSIVE MODE"
    );
    tx.execute(&lock_sql, ())
        .await
        .map_err(NotificationOutboxRuntimeAttachError::RelationLock)?;
    let marker_lock =
        format!("LOCK TABLE {qschema}.\"{OUTBOX_OWNER_STATE_TABLE}\" IN ACCESS SHARE MODE");
    tx.execute(&marker_lock, ())
        .await
        .map_err(NotificationOutboxRuntimeAttachError::RelationLock)?;
    Ok(())
}

/// Verify the already locked outbox relations and owner marker on the same
/// transaction that holds the outbox gate. Does not open or end the caller's
/// transaction.
pub(crate) async fn verify_runtime_attach_tx(
    tx: &mut Transaction<'_>,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    let schema = identity.schema.name.0.as_str();
    let schema_oid = i64::from(identity.schema.oid);
    let qschema = crate::db::physical_attestation::quote_identifier(schema);
    verify_active_schema(tx, schema, schema_oid).await?;
    verify_relation(tx, schema, "notification_candidates", 3).await?;
    verify_relation(tx, schema, "notification_outbox", 3).await?;
    verify_relation(tx, schema, "notification_outbox_lineage", 0).await?;
    verify_relation(tx, schema, OUTBOX_OWNER_STATE_TABLE, 2).await?;
    verify_columns(tx, schema, "notification_candidates", CANDIDATE_COLUMNS).await?;
    verify_columns(tx, schema, "notification_outbox", OUTBOX_COLUMNS).await?;
    verify_columns(tx, schema, "notification_outbox_lineage", LINEAGE_COLUMNS).await?;
    verify_columns(tx, schema, OUTBOX_OWNER_STATE_TABLE, OWNER_STATE_COLUMNS).await?;
    verify_constraints(tx, schema).await?;
    verify_indexes(
        tx,
        schema,
        "notification_candidates",
        &NOTIFICATION_CANDIDATES_INDEXES,
    )
    .await?;
    verify_indexes(
        tx,
        schema,
        "notification_outbox",
        &NOTIFICATION_OUTBOX_INDEXES,
    )
    .await?;
    verify_indexes(
        tx,
        schema,
        "notification_outbox_lineage",
        &["idx_notification_outbox_lineage_job"],
    )
    .await?;
    verify_indexes(tx, schema, OUTBOX_OWNER_STATE_TABLE, &[]).await?;
    verify_runtime_acl(tx, schema).await?;

    let marker_sql = format!(
        "SELECT prepared_revision FROM {qschema}.\"{OUTBOX_OWNER_STATE_TABLE}\" WHERE singleton = 1"
    );
    let mut rows = tx.query(&marker_sql, ()).await?;
    let revision = rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?;
    drop(rows);
    if revision != Some(OUTBOX_OWNER_PREPARED_REVISION) {
        return Err(NotificationOutboxRuntimeAttachError::OwnerPreparationIncomplete);
    }
    Ok(())
}

/// Check a completed outbox marker from an existing PostgreSQL owner
/// transaction. The shared advisory xact lock remains held until the caller
/// ends `tx`, so dependent owner writes stay serialized against outbox startup
/// invalidation and preparation.
pub(super) async fn verify_owner_preparation_complete_tx(
    tx: &mut Transaction<'_>,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    if tx.driver() != DatabaseDriver::Postgres {
        return Err(NotificationOutboxRuntimeAttachError::PostgresRequired);
    }
    let schema = identity.schema.name.0.as_str();
    crate::db::physical_attestation::validate_identifier(schema, "notification outbox schema")
        .map_err(|_| NotificationOutboxRuntimeAttachError::InvalidSchema)?;

    tx.query(
        "SELECT pg_catalog.pg_advisory_xact_lock_shared(?::integer, ?::integer)",
        crate::db_params![OUTBOX_SCHEMA_GATE.0, OUTBOX_SCHEMA_GATE.1],
    )
    .await?;

    let mut rows = tx
        .query(
            "SELECT (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
             db.oid::bigint, db.datname::text, n.oid::bigint, n.nspname::text, \
             n.nspname = pg_catalog.current_schema() AND NOT pg_catalog.pg_is_in_recovery() \
             FROM pg_catalog.pg_namespace n \
             JOIN pg_catalog.pg_database db ON db.datname = pg_catalog.current_database() \
             WHERE n.nspname = ?",
            crate::db_params![schema],
        )
        .await?;
    let identity_matches = match rows.next().await? {
        Some(row) => {
            let system: String = row.get(0)?;
            system
                .parse::<crate::db::lineage::PgSystemIdentifier>()
                .ok()
                == Some(identity.system_identifier)
                && row.get::<i64>(1)? == i64::from(identity.database.oid)
                && row.get::<String>(2)? == identity.database.name.0
                && row.get::<i64>(3)? == i64::from(identity.schema.oid)
                && row.get::<String>(4)? == *schema
                && row.get::<bool>(5)?
        }
        None => false,
    };
    drop(rows);
    if !identity_matches {
        return Err(NotificationOutboxRuntimeAttachError::OwnerIdentityMismatch);
    }

    let mut rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ? AND c.relkind = 'r')",
            crate::db_params![schema, OUTBOX_OWNER_STATE_TABLE],
        )
        .await?;
    let marker_exists = rows
        .next()
        .await?
        .map(|row| row.get::<bool>(0))
        .transpose()?
        .unwrap_or(false);
    drop(rows);
    if !marker_exists {
        return Err(NotificationOutboxRuntimeAttachError::OwnerPreparationIncomplete);
    }

    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    let lock =
        format!("LOCK TABLE {quoted_schema}.\"{OUTBOX_OWNER_STATE_TABLE}\" IN ACCESS SHARE MODE");
    tx.execute(&lock, ()).await?;
    verify_relation(tx, schema, OUTBOX_OWNER_STATE_TABLE, 2).await?;
    verify_columns(tx, schema, OUTBOX_OWNER_STATE_TABLE, OWNER_STATE_COLUMNS).await?;
    for (name, expression) in [
        (
            "notification_outbox_owner_state_singleton_check",
            "singleton=1",
        ),
        (
            "notification_outbox_owner_state_prepared_revision_check",
            "prepared_revision>=0",
        ),
    ] {
        if !verify_raw_check(tx, schema, OUTBOX_OWNER_STATE_TABLE, name, expression).await? {
            return Err(NotificationOutboxRuntimeAttachError::InvalidConstraints {
                table: OUTBOX_OWNER_STATE_TABLE,
            });
        }
    }
    verify_primary_key(tx, schema, OUTBOX_OWNER_STATE_TABLE, OWNER_STATE_PK).await?;

    let marker_sql = format!(
        "SELECT prepared_revision FROM {quoted_schema}.\"{OUTBOX_OWNER_STATE_TABLE}\" \
         WHERE singleton = 1"
    );
    let mut rows = tx.query(&marker_sql, ()).await?;
    let revision = rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?;
    drop(rows);
    if revision != Some(OUTBOX_OWNER_PREPARED_REVISION) {
        return Err(NotificationOutboxRuntimeAttachError::OwnerPreparationIncomplete);
    }
    Ok(())
}

async fn verify_active_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
    expected_oid: i64,
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.current_schema()::text, n.oid::bigint \
             FROM pg_catalog.pg_namespace n WHERE n.nspname = ?",
            crate::db_params![schema],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(NotificationOutboxRuntimeAttachError::ActiveSchemaMismatch);
    };
    let active: Option<String> = row.get(0)?;
    let oid: i64 = row.get(1)?;
    if active.as_deref() != Some(schema) || oid != expected_oid {
        return Err(NotificationOutboxRuntimeAttachError::ActiveSchemaMismatch);
    }
    Ok(())
}

async fn verify_relation(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
    expected_checks: i64,
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition \
               AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
               AND c.reloftype = 0 AND c.relchecks = ? \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                               WHERE tr.tgrelid = c.oid AND NOT tr.tgisinternal) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw \
                               WHERE rw.ev_class = c.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh \
                               WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint co \
                               WHERE co.conrelid = c.oid AND co.contype NOT IN ('p', 'c')) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint fk \
                               WHERE fk.contype = 'f' \
                                 AND (fk.conrelid = c.oid OR fk.confrelid = c.oid)) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![expected_checks, schema, table],
        )
        .await?;
    let valid = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !valid {
        return Err(NotificationOutboxRuntimeAttachError::InvalidRelation { table });
    }
    Ok(())
}

async fn verify_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
    contract: &[ColumnContract],
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                    a.attnotnull, COALESCE(pg_catalog.pg_get_expr(d.adbin, d.adrelid), ''), \
                    a.attgenerated::text, a.attidentity::text, \
                    a.attcollation = typ.typcollation \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class t ON t.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_type typ ON typ.oid = a.atttypid \
             LEFT JOIN pg_catalog.pg_attrdef d \
               ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = ? AND t.relname = ? AND t.relkind = 'r' \
               AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            crate::db_params![schema, table],
        )
        .await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        columns.push((
            row.get::<String>(0)?,
            row.get::<String>(1)?,
            row.get::<bool>(2)?,
            normalize_expression(&row.get::<String>(3)?),
            row.get::<String>(4)?,
            row.get::<String>(5)?,
            row.get::<bool>(6)?,
        ));
    }
    drop(rows);
    if columns.len() != contract.len()
        || contract.iter().any(|expected| {
            !columns.iter().any(|actual| {
                actual.0 == expected.name
                    && actual.1 == expected.kind
                    && actual.2 == expected.not_null
                    && actual.3 == normalize_expression(expected.default)
                    && actual.4.is_empty()
                    && actual.5.is_empty()
                    && actual.6
            })
        })
    {
        return Err(NotificationOutboxRuntimeAttachError::InvalidColumns { table });
    }
    Ok(())
}

fn normalize_expression(expression: &str) -> String {
    expression
        .to_ascii_lowercase()
        .replace("::text", "")
        .replace("::integer", "")
        .replace("::bigint", "")
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '(' && *character != ')')
        .collect()
}

async fn verify_constraints(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    let checks = [
        (
            "notification_candidates",
            NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME,
            "class",
            false,
            NOTIFICATION_CANDIDATES_CLASS_VALUES.as_slice(),
        ),
        (
            "notification_candidates",
            NOTIFICATION_CANDIDATES_REASON_CHECK_NAME,
            "reason",
            false,
            NOTIFICATION_CANDIDATES_REASON_VALUES.as_slice(),
        ),
        (
            "notification_candidates",
            NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_NAME,
            "suppressed_reason",
            true,
            NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES.as_slice(),
        ),
        (
            "notification_outbox",
            NOTIFICATION_OUTBOX_CLASS_CHECK_NAME,
            "class",
            false,
            NOTIFICATION_CANDIDATES_CLASS_VALUES.as_slice(),
        ),
        (
            "notification_outbox",
            "notification_outbox_status_check",
            "status",
            false,
            &["queued", "in-progress", "published", "failed"],
        ),
    ];
    for (table, name, column, nullable, values) in checks {
        let mut rows = tx
            .query(
                "SELECT co.contype::text, co.condeferrable, co.condeferred, co.convalidated, \
                        pg_catalog.pg_get_constraintdef(co.oid, true) \
                 FROM pg_catalog.pg_constraint co \
                 JOIN pg_catalog.pg_class t ON t.oid = co.conrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                 WHERE n.nspname = ? AND t.relname = ? AND co.conname = ?",
                crate::db_params![schema, table, name],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(NotificationOutboxRuntimeAttachError::InvalidConstraints { table });
        };
        let valid = row.get::<String>(0)? == "c"
            && !row.get::<bool>(1)?
            && !row.get::<bool>(2)?
            && row.get::<bool>(3)?
            && normalize_check(&row.get::<String>(4)?) == expected_check(column, nullable, values);
        if !valid {
            return Err(NotificationOutboxRuntimeAttachError::InvalidConstraints { table });
        }
    }
    let queue_check = verify_raw_check(
        tx,
        schema,
        "notification_outbox",
        "notification_outbox_queue_acceptance_may_exist_check",
        "queue_acceptance_may_exist=anyarray[0,1]",
    )
    .await?;
    if !queue_check {
        return Err(NotificationOutboxRuntimeAttachError::InvalidConstraints {
            table: "notification_outbox",
        });
    }
    for (name, expression) in [
        (
            "notification_outbox_owner_state_singleton_check",
            "singleton=1",
        ),
        (
            "notification_outbox_owner_state_prepared_revision_check",
            "prepared_revision>=0",
        ),
    ] {
        if !verify_raw_check(tx, schema, OUTBOX_OWNER_STATE_TABLE, name, expression).await? {
            return Err(NotificationOutboxRuntimeAttachError::InvalidConstraints {
                table: OUTBOX_OWNER_STATE_TABLE,
            });
        }
    }
    verify_primary_key(tx, schema, "notification_candidates", CANDIDATE_PK).await?;
    verify_primary_key(tx, schema, "notification_outbox", OUTBOX_PK).await?;
    verify_primary_key(tx, schema, "notification_outbox_lineage", LINEAGE_PK).await?;
    verify_primary_key(tx, schema, OUTBOX_OWNER_STATE_TABLE, OWNER_STATE_PK).await?;
    Ok(())
}

async fn verify_raw_check(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
    name: &'static str,
    expected: &str,
) -> Result<bool, NotificationOutboxRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT co.contype::text, co.condeferrable, co.condeferred, co.convalidated, \
                    pg_catalog.pg_get_constraintdef(co.oid, true) \
             FROM pg_catalog.pg_constraint co \
             JOIN pg_catalog.pg_class t ON t.oid = co.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = ? AND t.relname = ? AND co.conname = ?",
            crate::db_params![schema, table, name],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    Ok(row.get::<String>(0)? == "c"
        && !row.get::<bool>(1)?
        && !row.get::<bool>(2)?
        && row.get::<bool>(3)?
        && normalize_check(&row.get::<String>(4)?) == expected)
}

fn normalize_check(definition: &str) -> String {
    definition
        .to_ascii_lowercase()
        .replace("::text[]", "")
        .replace("::text", "")
        .replace("::integer", "")
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '(' && *character != ')')
        .collect::<String>()
        .trim_start_matches("check")
        .to_owned()
}

fn expected_check(column: &str, nullable: bool, values: &[&str]) -> String {
    let values = values
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(",");
    if nullable {
        format!("{column}isnullor{column}=anyarray[{values}]")
    } else {
        format!("{column}=anyarray[{values}]")
    }
}

async fn verify_primary_key(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
    expected: &[&str],
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text FROM pg_catalog.pg_constraint co \
             JOIN pg_catalog.pg_class t ON t.oid = co.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             CROSS JOIN LATERAL pg_catalog.unnest(co.conkey) \
               WITH ORDINALITY k(attnum, position) \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             WHERE co.contype = 'p' AND NOT co.condeferrable AND co.convalidated \
               AND n.nspname = ? AND t.relname = ? ORDER BY k.position",
            crate::db_params![schema, table],
        )
        .await?;
    let mut actual = Vec::new();
    while let Some(row) = rows.next().await? {
        actual.push(row.get::<String>(0)?);
    }
    if actual.iter().map(String::as_str).collect::<Vec<_>>() != expected {
        return Err(NotificationOutboxRuntimeAttachError::InvalidConstraints { table });
    }
    Ok(())
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
    named_indexes: &[&'static str],
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT ix.relname::text FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
             WHERE n.nspname = ? AND t.relname = ? ORDER BY ix.relname",
            crate::db_params![schema, table],
        )
        .await?;
    let mut names = Vec::new();
    while let Some(row) = rows.next().await? {
        names.push(row.get::<String>(0)?);
    }
    drop(rows);
    if names.len() != named_indexes.len() + 1 {
        return Err(NotificationOutboxRuntimeAttachError::InvalidIndexes { table });
    }
    for name in named_indexes {
        if !names.iter().any(|actual| actual == name) {
            return Err(NotificationOutboxRuntimeAttachError::InvalidIndexes { table });
        }
        verify_named_index(tx, schema, table, name).await?;
    }
    Ok(())
}

async fn verify_named_index(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
    name: &'static str,
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT i.indisunique, i.indisvalid, i.indisready, i.indislive, \
                    i.indisprimary, i.indpred IS NULL, i.indexprs IS NULL, \
                    am.amname = 'btree', i.indnkeyatts::bigint, i.indnatts::bigint, \
                    pg_catalog.array_to_string(ARRAY( \
                      SELECT a.attname::text FROM pg_catalog.unnest(i.indkey) \
                        WITH ORDINALITY k(attnum, position) \
                      JOIN pg_catalog.pg_attribute a \
                        ON a.attrelid = t.oid AND a.attnum = k.attnum \
                      WHERE k.position <= i.indnkeyatts ORDER BY k.position), ','), \
                    COALESCE(pg_catalog.pg_get_expr(i.indpred, i.indrelid), ''), \
                    COALESCE(( \
                      SELECT pg_catalog.bool_and(op.opcdefault \
                         AND op.opcintype = a.atttypid \
                         AND k.collation_oid = a.attcollation) \
                      FROM unnest( \
                           i.indkey::smallint[], i.indclass::oid[], i.indcollation::oid[]) \
                           k(attnum, opclass, collation_oid) \
                      JOIN pg_catalog.pg_attribute a \
                        ON a.attrelid = t.oid AND a.attnum = k.attnum \
                      JOIN pg_catalog.pg_opclass op ON op.oid = k.opclass), false) \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
             WHERE n.nspname = ? AND t.relname = ? AND ix.relname = ?",
            crate::db_params![schema, table, name],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(NotificationOutboxRuntimeAttachError::InvalidIndexes { table });
    };
    let unique: bool = row.get(0)?;
    let healthy: bool = row.get(1)? && row.get(2)? && row.get(3)?;
    let primary: bool = row.get(4)?;
    let no_predicate: bool = row.get(5)?;
    let no_expression: bool = row.get(6)?;
    let btree: bool = row.get(7)?;
    let key_count: i64 = row.get(8)?;
    let total_count: i64 = row.get(9)?;
    let columns: String = row.get(10)?;
    let predicate: String = row.get(11)?;
    let default_operator_and_collation: bool = row.get(12)?;
    let (expected_unique, expected_columns, expected_predicate) = expected_index(name);
    if unique != expected_unique
        || !healthy
        || primary
        || !no_expression
        || !btree
        || !default_operator_and_collation
        || key_count != expected_columns.len() as i64
        || total_count != expected_columns.len() as i64
        || columns != expected_columns.join(",")
        || (expected_predicate.is_empty() != no_predicate)
        || (!expected_predicate.is_empty()
            && predicate_tokens(&predicate) != predicate_tokens(expected_predicate))
    {
        return Err(NotificationOutboxRuntimeAttachError::InvalidIndexes { table });
    }
    Ok(())
}

fn expected_index(name: &str) -> (bool, Vec<&'static str>, &'static str) {
    match name {
        "idx_notification_candidates_recipient_created" => {
            (false, vec!["recipient_bare_jid", "created_at_ms"], "")
        }
        "idx_notification_candidates_identity" => (
            true,
            vec![
                "recipient_bare_jid",
                "conversation_jid",
                "thread_id",
                "stanza_id",
                "class",
            ],
            "",
        ),
        "idx_notification_candidates_pending_worker" => (
            false,
            vec![
                "created_at_ms",
                "recipient_bare_jid",
                "conversation_jid",
                "thread_id",
                "stanza_id_by",
                "stanza_id",
                "class",
            ],
            "outboxed_at_ms IS NULL",
        ),
        "idx_notification_candidates_outboxed_prune" => (
            false,
            vec![
                "outboxed_at_ms",
                "recipient_bare_jid",
                "conversation_jid",
                "thread_id",
                "stanza_id_by",
                "stanza_id",
                "class",
            ],
            "outboxed_at_ms IS NOT NULL",
        ),
        COALESCE_INDEX_NAME => (true, COALESCE_COLUMNS.to_vec(), COALESCE_PREDICATE),
        "idx_notification_outbox_conversation_status" => (
            false,
            vec![
                "recipient_bare_jid",
                "conversation_jid",
                "thread_id",
                "status",
            ],
            "",
        ),
        "idx_notification_outbox_status_next_attempt" => (
            false,
            vec!["status", "next_attempt_at_ms", "created_at_ms"],
            "",
        ),
        "idx_notification_outbox_retention_prune" => (
            false,
            vec!["status", "updated_at_ms", "job_id"],
            "status = ANY (ARRAY['published', 'failed'])",
        ),
        "idx_notification_outbox_lineage_job" => (false, vec!["job_id"], ""),
        _ => (false, Vec::new(), "invalid"),
    }
}

async fn verify_runtime_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), NotificationOutboxRuntimeAttachError> {
    for (table, writable) in [
        ("notification_candidates", true),
        ("notification_outbox", true),
        ("notification_outbox_lineage", true),
        (OUTBOX_OWNER_STATE_TABLE, false),
    ] {
        let mut rows = tx
            .query(
                r#"SELECT session_user::text = current_user::text
                        AND current_user::text = runtime.rolname
                        AND runtime.rolcanlogin AND NOT runtime.rolsuper
                        AND NOT runtime.rolcreaterole AND NOT runtime.rolcreatedb
                        AND NOT runtime.rolreplication AND NOT runtime.rolbypassrls
                        AND runtime.oid <> database.datdba
                        AND NOT pg_catalog.has_database_privilege(runtime.oid, database.oid, 'CREATE,TEMP')
                        AND NOT pg_catalog.has_parameter_privilege(runtime.oid, 'session_replication_role', 'SET')
                        AND pg_catalog.current_setting('session_replication_role') = 'origin'
                        AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                                        WHERE m.member = runtime.oid OR m.roleid = runtime.oid)
                        AND pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'USAGE')
                        AND NOT pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'CREATE')
                        AND ns.nspowner = owner.oid AND relation.relowner = owner.oid
                        AND owner.oid <> runtime.oid AND owner.oid <> database.datdba
                        AND NOT owner.rolcanlogin AND NOT owner.rolsuper
                        AND NOT owner.rolcreaterole AND NOT owner.rolcreatedb
                        AND NOT owner.rolreplication AND NOT owner.rolbypassrls
                        AND NOT pg_catalog.pg_has_role(runtime.oid, owner.oid, 'SET')
                        AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                          CASE WHEN pg_catalog.cardinality(ns.nspacl) > 0 THEN ns.nspacl
                               ELSE pg_catalog.acldefault('n', ns.nspowner) END
                        ) acl WHERE acl.grantee = 0 OR (acl.grantee <> owner.oid
                          AND (acl.privilege_type <> 'USAGE' OR acl.is_grantable)))
                        AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT')
                        AND ((? <> 0 AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT,UPDATE,DELETE'))
                          OR (? = 0 AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT,UPDATE,DELETE')))
                        AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'TRUNCATE,REFERENCES,TRIGGER')
                        AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid,
                          'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION, UPDATE WITH GRANT OPTION,
                           DELETE WITH GRANT OPTION, TRUNCATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION,
                           TRIGGER WITH GRANT OPTION')
                        AND NOT pg_catalog.has_any_column_privilege(runtime.oid, relation.oid,
                          'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION, UPDATE WITH GRANT OPTION,
                           REFERENCES WITH GRANT OPTION')
                        AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000
                          THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN')
                          ELSE true END
                        AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                          CASE WHEN pg_catalog.cardinality(relation.relacl) > 0 THEN relation.relacl
                               ELSE pg_catalog.acldefault('r', relation.relowner) END
                        ) acl WHERE acl.grantee NOT IN (owner.oid, runtime.oid)
                          OR (acl.grantee = runtime.oid AND (acl.is_grantable OR
                            (? <> 0 AND acl.privilege_type NOT IN ('SELECT','INSERT','UPDATE','DELETE')) OR
                            (? = 0 AND acl.privilege_type <> 'SELECT'))))
                        AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
                          CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl
                          WHERE a.attrelid = relation.oid AND a.attnum > 0 AND NOT a.attisdropped)
                 FROM pg_catalog.pg_roles runtime
                 JOIN pg_catalog.pg_namespace ns ON ns.nspname = ?
                 JOIN pg_catalog.pg_roles owner ON owner.oid = ns.nspowner
                 JOIN pg_catalog.pg_database database ON database.datname = pg_catalog.current_database()
                 JOIN pg_catalog.pg_class relation ON relation.relnamespace = ns.oid
                   AND relation.relname = ? AND relation.relkind = 'r'
                 WHERE runtime.rolname = current_user"#,
                crate::db_params![i64::from(writable), i64::from(writable), i64::from(writable), i64::from(writable), schema, table],
            )
            .await?;
        let safe = match rows.next().await? {
            Some(row) => row.get::<bool>(0)?,
            None => false,
        };
        if !safe {
            return Err(NotificationOutboxRuntimeAttachError::UnsafeRuntimeRole);
        }
    }
    Ok(())
}
