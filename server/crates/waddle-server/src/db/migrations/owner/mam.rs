//! Unwired owner-only PostgreSQL MAM bootstrap on a dedicated backend.

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row as _};
use thiserror::Error;
use waddle_xmpp::mam::{MamSchemaBootstrapError, SqlxMamStorage};

use super::{
    check_migration_login_on_connection, check_migrator_options, DarkMigrationOwnerConfig,
    DarkMigrationOwnerError, OwnerRoleScope, CONNECT_DEADLINE, OPERATION_DEADLINE,
};
use crate::db::lineage::{PgSystemIdentifier, LINEAGE_ADVISORY_LOCK_KEY};
use crate::db::migrations::runner::MIGRATION_LEDGER_ADVISORY_LOCK_KEY;
use crate::db::migrations::MigrationRunner;
use crate::db::physical_attestation::{quote_identifier, validate_identifier, PgRoleName};
use crate::db::DatabaseError;

#[derive(Debug, Error)]
pub enum DarkMamOwnerError {
    #[error(transparent)]
    Owner(#[from] DarkMigrationOwnerError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("MAM owner session failed")]
    Postgres(#[from] sqlx::Error),
    #[error("MAM owner session has an invalid or unbounded timeout setting")]
    InvalidTimeout,
    #[error("MAM bootstrap refused before dispatching DDL")]
    PreDdl(#[source] MamSchemaBootstrapError),
    #[error("MAM owner work may be partial or unknown; fence and reconcile before retry")]
    PartialOrUnknown,
}

/// Bootstrap on one direct, non-pooled backend after the global catalog commits.
///
/// Expected identity and roles must come from an independent trusted handoff.
/// Operators must fence the primary, drain old sessions, and keep writers,
/// DDL and role changes quiescent across global migration and this operation.
/// Transaction/statement poolers are unsupported. The session locks cannot
/// establish those conditions. A timeout, dropped future, lost response, or
/// initializer error may leave partial work. Dropping the socket does not prove
/// the backend stopped; fence it and reconcile before retry.
pub async fn bootstrap_dark_postgres_mam_owner(
    options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<(), DarkMamOwnerError> {
    check_migrator_options(options, config, migrator)?;
    match tokio::time::timeout(
        OPERATION_DEADLINE,
        run_on_dedicated_connection(options, config, migrator),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(DarkMamOwnerError::PartialOrUnknown),
    }
}

async fn run_on_dedicated_connection(
    options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<(), DarkMamOwnerError> {
    let identity = config
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DarkMigrationOwnerError::MissingPostgresIdentity)?;
    let schema = &identity.schema.name.0;
    validate_identifier(schema, "MAM schema").map_err(|_| DarkMigrationOwnerError::Identity)?;
    let quoted = quote_identifier(schema);
    let mut connection =
        tokio::time::timeout(CONNECT_DEADLINE, PgConnection::connect_with(options))
            .await
            .map_err(|_| DarkMigrationOwnerError::ConnectTimeout)??;
    bound_session_timeouts(&mut connection).await?;
    sqlx::query("SELECT pg_catalog.set_config('search_path', $1, false)")
        .bind(format!("{quoted}, pg_temp"))
        .execute(&mut connection)
        .await?;
    check_migration_login_on_connection(
        &mut connection,
        config,
        migrator,
        schema,
        i64::from(identity.schema.oid),
        OwnerRoleScope::Session,
    )
    .await?;
    let backend = verify_owner_identity(&mut connection, config, migrator).await?;
    for key in [
        LINEAGE_ADVISORY_LOCK_KEY,
        MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
    ] {
        sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut connection)
            .await?;
    }
    if verify_owner_identity(&mut connection, config, migrator).await? != backend {
        return Err(DarkMigrationOwnerError::Identity.into());
    }
    MigrationRunner::single()
        .verify_complete_on_postgres_connection(&mut connection, &format!("{quoted}._migrations"))
        .await?;
    // V1017 must have created the counter on this schema before MAM's
    // independent autocommit initialization can begin.
    let counter_owned: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_roles o ON o.oid = c.relowner \
         WHERE n.nspname = $1 AND c.relname = 'mam_archive_sequences' \
           AND c.relkind = 'r' AND c.relpersistence = 'p' AND o.rolname = $2)",
    )
    .bind(schema)
    .bind(config.owner.as_str())
    .fetch_one(&mut connection)
    .await?;
    if !counter_owned {
        return Err(DarkMigrationOwnerError::Identity.into());
    }

    // The MAM function owns the direct connection while holding its session
    // lock; errors and cancellation drop the socket instead of pooling it.
    let mut connection = SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(
        connection,
        schema,
        config.owner.as_str(),
    )
    .await
    .map_err(classify_mam_bootstrap_error)?;
    verify_completed_bootstrap(&mut connection, config, migrator, backend)
        .await
        .map_err(|_| DarkMamOwnerError::PartialOrUnknown)?;

    for key in [
        MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
        LINEAGE_ADVISORY_LOCK_KEY,
    ] {
        let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
            .bind(key)
            .fetch_one(&mut connection)
            .await
            .map_err(|_| DarkMamOwnerError::PartialOrUnknown)?;
        if !unlocked {
            return Err(DarkMamOwnerError::PartialOrUnknown);
        }
    }
    connection
        .close()
        .await
        .map_err(|_| DarkMamOwnerError::PartialOrUnknown)
}

async fn verify_completed_bootstrap(
    connection: &mut PgConnection,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
    backend: i32,
) -> Result<(), DarkMamOwnerError> {
    let schema = &config
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DarkMigrationOwnerError::MissingPostgresIdentity)?
        .schema
        .name
        .0;
    let mut tx = connection.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "SELECT pg_catalog.set_config('lock_timeout', pg_catalog.current_setting('lock_timeout'), true), \
         pg_catalog.set_config('statement_timeout', pg_catalog.current_setting('statement_timeout'), true)",
    )
    .execute(&mut *tx)
    .await?;
    if verify_owner_identity(&mut tx, config, migrator).await? != backend {
        return Err(DarkMigrationOwnerError::Identity.into());
    }
    MigrationRunner::single()
        .verify_complete_on_postgres_connection(
            &mut tx,
            &format!("{}._migrations", quote_identifier(schema)),
        )
        .await?;
    SqlxMamStorage::verify_postgres_schema_on_connection(&mut tx, schema, config.owner.as_str())
        .await
        .map_err(|_| DarkMamOwnerError::PartialOrUnknown)?;
    tx.commit().await?;
    Ok(())
}

fn classify_mam_bootstrap_error(error: MamSchemaBootstrapError) -> DarkMamOwnerError {
    match error {
        MamSchemaBootstrapError::TransactionOpen
        | MamSchemaBootstrapError::AlreadyHeld
        | MamSchemaBootstrapError::PreexistingObjectConflict => DarkMamOwnerError::PreDdl(error),
        MamSchemaBootstrapError::Lock(_)
        | MamSchemaBootstrapError::Preflight(_)
        | MamSchemaBootstrapError::PartialOrUnknown(_)
        | MamSchemaBootstrapError::Unlock(_)
        | MamSchemaBootstrapError::LockNotHeld => DarkMamOwnerError::PartialOrUnknown,
    }
}

/// Bound an orphaned direct backend without weakening tighter role settings.
async fn bound_session_timeouts(connection: &mut PgConnection) -> Result<(), DarkMamOwnerError> {
    for (setting, cap_ms) in [
        ("lock_timeout", 3_000_u64),
        ("statement_timeout", 30_000_u64),
        ("idle_session_timeout", 60_000_u64),
        ("idle_in_transaction_session_timeout", 15_000_u64),
    ] {
        let value: String = sqlx::query_scalar("SELECT pg_catalog.current_setting($1)")
            .bind(setting)
            .fetch_one(&mut *connection)
            .await?;
        let millis = crate::db::read_only_timeout::timeout_millis(&value)
            .ok_or(DarkMamOwnerError::InvalidTimeout)?;
        if millis == 0 || millis > cap_ms {
            sqlx::query("SELECT pg_catalog.set_config($1, $2, false)")
                .bind(setting)
                .bind(format!("{cap_ms}ms"))
                .execute(&mut *connection)
                .await?;
        }
        let bounded: String = sqlx::query_scalar("SELECT pg_catalog.current_setting($1)")
            .bind(setting)
            .fetch_one(&mut *connection)
            .await?;
        let bounded = crate::db::read_only_timeout::timeout_millis(&bounded)
            .ok_or(DarkMamOwnerError::InvalidTimeout)?;
        if bounded == 0 || bounded > cap_ms {
            return Err(DarkMamOwnerError::InvalidTimeout);
        }
    }
    Ok(())
}

/// Compare persisted and live identity on this exact session with independent input.
async fn verify_owner_identity(
    connection: &mut PgConnection,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<i32, DarkMigrationOwnerError> {
    let expected = config
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DarkMigrationOwnerError::MissingPostgresIdentity)?;
    let table = format!("{}._lineage", quote_identifier(&expected.schema.name.0));
    let sql = format!(
        "SELECT l.id::bigint, l.format::bigint, l.lineage_uuid, l.deployment_uuid, \
         l.pg_system_identifier, l.pg_database_oid, l.pg_database_name, \
         l.pg_schema_oid, l.pg_schema_name, \
         (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
         d.oid::bigint, d.datname::text, n.oid::bigint, n.nspname::text, \
         pg_catalog.pg_is_in_recovery(), pg_catalog.pg_backend_pid(), \
         session_user::text, current_user::text, pg_catalog.current_schema()::text, \
         pg_catalog.pg_my_temp_schema()::bigint, \
         pg_catalog.current_setting('session_replication_role') \
         FROM {table} l JOIN pg_catalog.pg_database d \
         ON d.datname = pg_catalog.current_database() \
         JOIN pg_catalog.pg_namespace n ON n.nspname = $1"
    );
    let rows = sqlx::query(&sql)
        .bind(&expected.schema.name.0)
        .fetch_all(&mut *connection)
        .await?;
    let [row] = rows.as_slice() else {
        return Err(DarkMigrationOwnerError::Identity);
    };
    let stored_system = row
        .try_get::<String, _>(4)?
        .parse::<PgSystemIdentifier>()
        .map_err(|_| DarkMigrationOwnerError::Identity)?;
    let live_system = row
        .try_get::<String, _>(9)?
        .parse::<PgSystemIdentifier>()
        .map_err(|_| DarkMigrationOwnerError::Identity)?;
    let lineage = row
        .try_get::<String, _>(2)?
        .parse::<uuid::Uuid>()
        .map_err(|_| DarkMigrationOwnerError::Identity)?;
    let deployment = row
        .try_get::<String, _>(3)?
        .parse::<uuid::Uuid>()
        .map_err(|_| DarkMigrationOwnerError::Identity)?;
    if row.try_get::<i64, _>(0)? != 1
        || row.try_get::<i64, _>(1)? != 1
        || lineage != config.expected.lineage_uuid.0
        || deployment != config.expected.deployment_uuid.0
        || stored_system != expected.system_identifier
        || live_system != expected.system_identifier
        || row.try_get::<String, _>(5)? != expected.database.oid.to_string()
        || row.try_get::<String, _>(6)? != expected.database.name.0
        || row.try_get::<String, _>(7)? != expected.schema.oid.to_string()
        || row.try_get::<String, _>(8)? != expected.schema.name.0
        || row.try_get::<i64, _>(10)? != i64::from(expected.database.oid)
        || row.try_get::<String, _>(11)? != expected.database.name.0
        || row.try_get::<i64, _>(12)? != i64::from(expected.schema.oid)
        || row.try_get::<String, _>(13)? != expected.schema.name.0
        || row.try_get::<bool, _>(14)?
        || row.try_get::<String, _>(16)? != migrator.as_str()
        || row.try_get::<String, _>(17)? != config.owner.as_str()
        || row.try_get::<Option<String>, _>(18)?.as_deref() != Some(expected.schema.name.0.as_str())
        || row.try_get::<i64, _>(19)? != 0
        || row.try_get::<String, _>(20)? != "origin"
    {
        return Err(DarkMigrationOwnerError::Identity);
    }
    Ok(row.try_get(15)?)
}
