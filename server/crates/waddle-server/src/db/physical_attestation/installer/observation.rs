//! Bounded operator observation. This path never invokes the installed function.

use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{ConnectOptions, Connection, PgConnection, Row};
use uuid::Uuid;

use super::super::super::read_only_timeout::timeout_millis;
use super::super::{parse_snapshot, quote_identifier, verify_identity};
use super::super::{PhysicalAttestationConfig, PhysicalAttestationError, FUNCTION_NAME};
use super::{marker, verify_function, verify_prerequisites, INSTALL_LOCK_NAMESPACE};
use crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY;

const DEADLINE: Duration = Duration::from_secs(30);

/// A scoped, point-in-time observation; none of these variants is authority to
/// start a writer, nor proof that an absent operation cannot later commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationObservation {
    Observed,
    Conflict { reason: &'static str },
    Indeterminate { reason: &'static str },
}

/// Observe one installation using an independently supplied expected identity.
/// The endpoint must be a direct, authenticated connection to the authoritative
/// primary; callers must establish primary fencing and exclude non-cooperating
/// administrative changes. This function does not invoke the attestor.
pub async fn observe_installation(
    options: &PgConnectOptions,
    config: &PhysicalAttestationConfig,
    operation_id: Uuid,
) -> InstallationObservation {
    // PostgreSQL startup values are NUL-terminated. SQLx forwards application
    // and database names verbatim, so embedded NULs could otherwise inject an
    // `options` parameter before the inherited timeout bounds are inspected.
    if !matches!(options.get_ssl_mode(), PgSslMode::VerifyFull)
        || options.get_socket().is_some()
        || options.get_host().is_empty()
        || options.get_host().starts_with('/')
        || options.get_host().contains('\0')
        || options.get_username().contains('\0')
        || options
            .get_database()
            .is_none_or(|name| name.contains('\0'))
        || options
            .get_application_name()
            .is_some_and(|name| name.contains('\0'))
        || options.get_options().is_some()
        || options.get_database() != Some(config.postgres().database.name.0.as_str())
        || options.get_username() != config.installer_login.as_str()
    {
        return InstallationObservation::Indeterminate {
            reason: "connection_configuration",
        };
    }
    match tokio::time::timeout(DEADLINE, observe(options, config, operation_id)).await {
        Ok(Ok(observation)) => observation,
        Ok(Err(PhysicalAttestationError::Mismatch { field })) => {
            InstallationObservation::Conflict { reason: field }
        }
        Ok(Err(PhysicalAttestationError::InstallationConflict)) => {
            InstallationObservation::Conflict {
                reason: "installation_catalog",
            }
        }
        Ok(Err(_)) => InstallationObservation::Indeterminate {
            reason: "observation_failed",
        },
        Err(_) => InstallationObservation::Indeterminate { reason: "deadline" },
    }
}

async fn observe(
    options: &PgConnectOptions,
    config: &PhysicalAttestationConfig,
    operation_id: Uuid,
) -> Result<InstallationObservation, PhysicalAttestationError> {
    let options = options.clone().disable_statement_logging();
    let mut connection = PgConnection::connect_with(&options).await?;
    // SET is parsed without name lookup. Do it before any SELECT, so a hostile
    // inherited search_path cannot select user-defined functions or operators.
    sqlx::query("SET search_path = pg_catalog, pg_temp")
        .execute(&mut connection)
        .await?;
    check_backend_bounds(&mut connection).await?;
    let row = sqlx::query(
        "SELECT session_user::pg_catalog.text, current_user::pg_catalog.text, pg_catalog.pg_is_in_recovery()",
    )
    .fetch_one(&mut connection)
    .await?;
    if row.try_get::<String, _>(0)? != config.installer_login.as_str()
        || row.try_get::<String, _>(1)? != config.installer_login.as_str()
    {
        return Err(PhysicalAttestationError::Mismatch {
            field: "installer login/session role",
        });
    }
    if row.try_get::<bool, _>(2)? {
        return Err(PhysicalAttestationError::NotPrimary);
    }
    // Session locks are acquired before BEGIN so the repeatable-read snapshot
    // is taken only after both cooperating installer/lineage locks are held.
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
        .bind(LINEAGE_ADVISORY_LOCK_KEY)
        .execute(&mut connection)
        .await?;
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1, pg_catalog.hashtext($2))")
        .bind(INSTALL_LOCK_NAMESPACE)
        .bind(&config.trusted_schema.0)
        .execute(&mut connection)
        .await?;
    let mut tx = connection.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL row_security = off")
        .execute(&mut *tx)
        .await?;
    verify_prerequisites(&mut tx, config).await?;
    sqlx::query(&format!(
        "SET LOCAL ROLE {}",
        quote_identifier(config.definer_role.as_str())
    ))
    .execute(&mut *tx)
    .await?;
    let role = sqlx::query("SELECT session_user::pg_catalog.text, current_user::pg_catalog.text")
        .fetch_one(&mut *tx)
        .await?;
    if role.try_get::<String, _>(0)? != config.installer_login.as_str()
        || role.try_get::<String, _>(1)? != config.definer_role.as_str()
    {
        return Err(PhysicalAttestationError::Mismatch {
            field: "definer role handoff",
        });
    }
    read_locked_identity(&mut tx, config).await?;
    let count: i64 = sqlx::query_scalar("SELECT pg_catalog.count(*) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = $2")
        .bind(&config.trusted_schema.0)
        .bind(FUNCTION_NAME)
        .fetch_one(&mut *tx)
        .await?;
    let observation = match count {
        0 => InstallationObservation::Indeterminate {
            reason: "installation_not_visible",
        },
        1 => {
            verify_function(&mut tx, config, &marker(config, operation_id)).await?;
            InstallationObservation::Observed
        }
        _ => InstallationObservation::Conflict {
            reason: "installation_catalog",
        },
    };
    tx.rollback().await?;
    connection.close().await?;
    Ok(observation)
}

async fn check_backend_bounds(
    connection: &mut PgConnection,
) -> Result<(), PhysicalAttestationError> {
    // Reject unbounded/looser settings instead of overriding them at startup:
    // startup overrides would silently lengthen a stricter inherited limit.
    // transaction_timeout requires PostgreSQL 17 or newer.
    let row = sqlx::query("SELECT pg_catalog.current_setting('lock_timeout'), pg_catalog.current_setting('statement_timeout'), pg_catalog.current_setting('idle_in_transaction_session_timeout'), pg_catalog.current_setting('idle_session_timeout'), pg_catalog.current_setting('transaction_timeout', true)")
        .fetch_one(connection)
        .await?;
    for (index, cap) in [
        (0, 2_000),
        (1, 5_000),
        (2, 10_000),
        (3, 10_000),
        (4, 20_000),
    ] {
        let value: Option<String> = row.try_get(index)?;
        let valid = value
            .as_deref()
            .and_then(timeout_millis)
            .is_some_and(|millis| millis > 0 && millis <= cap);
        if !valid {
            return Err(PhysicalAttestationError::InstallationIndeterminate);
        }
    }
    Ok(())
}

async fn read_locked_identity(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
) -> Result<(), PhysicalAttestationError> {
    let schema = quote_identifier(&config.postgres().schema.name.0);
    sqlx::query(&format!(
        "LOCK TABLE ONLY {schema}._lineage IN ACCESS SHARE MODE"
    ))
    .execute(&mut *connection)
    .await?;
    // Pin the actual relation and its composite row type, and reject inherited
    // relations and incompatible column types before touching lineage values.
    let binding = sqlx::query(r#"
        SELECT c.oid::pg_catalog.int8, c.reltype::pg_catalog.int8
        FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = $1 AND n.oid::pg_catalog.int8 = $2
          AND c.relname = '_lineage' AND c.relkind = 'r' AND c.reltype <> 0
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
          AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = pg_catalog.pg_backend_pid() AND l.locktype = 'relation' AND l.relation = c.oid AND l.mode = 'AccessShareLock' AND l.granted)
          AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute a
               WHERE a.attrelid = c.oid AND NOT a.attisdropped
                 AND ((a.attname IN ('id', 'format') AND a.atttypid = 'pg_catalog.int4'::pg_catalog.regtype)
                   OR (a.attname IN ('lineage_uuid', 'deployment_uuid', 'pg_system_identifier', 'pg_database_oid', 'pg_database_name', 'pg_schema_oid', 'pg_schema_name') AND a.atttypid = 'pg_catalog.text'::pg_catalog.regtype))) = 9
    "#)
    .bind(&config.postgres().schema.name.0)
    .bind(i64::from(config.postgres().schema.oid))
    .fetch_optional(&mut *connection)
    .await?
    .ok_or(PhysicalAttestationError::Mismatch { field: "lineage relation" })?;
    let oid: i64 = binding.try_get(0)?;
    let row_type: i64 = binding.try_get(1)?;
    // A namespace rename can change the relation resolved by the next parse.
    // Test the whole-row type and tableoid before evaluating lineage fields;
    // the explicit l.* cannot be shadowed by a same-named column. Limiting to
    // two rows detects a broken singleton without retaining unbounded data.
    let guard = "pg_catalog.pg_typeof(l.*)::pg_catalog.oid = $1::pg_catalog.int8::pg_catalog.oid AND l.tableoid = $2::pg_catalog.int8::pg_catalog.oid";
    // Fetch one extra character so an overlong value can never compare as a
    // valid prefix (numeric fields can have arbitrarily many leading zeroes).
    let bounded = |column: &str| {
        format!("CASE WHEN {guard} THEN pg_catalog.substring(l.{column}, 1, 129) END")
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {}, {}, \
         (SELECT system_identifier::pg_catalog.text FROM pg_catalog.pg_control_system()), \
         (SELECT oid::pg_catalog.text FROM pg_catalog.pg_database WHERE datname = pg_catalog.current_database()), \
         pg_catalog.current_database()::pg_catalog.text, \
         (SELECT oid::pg_catalog.text FROM pg_catalog.pg_namespace WHERE nspname = $3), \
         $3::pg_catalog.text, \
         CASE WHEN {guard} THEN l.format::pg_catalog.int8 END, \
         pg_catalog.pg_typeof(l.*)::pg_catalog.oid::pg_catalog.int8, \
         l.tableoid::pg_catalog.int8, CASE WHEN {guard} THEN l.id END \
         FROM ONLY {schema}._lineage AS l LIMIT 2",
        bounded("lineage_uuid"), bounded("deployment_uuid"), bounded("pg_system_identifier"),
        bounded("pg_database_oid"), bounded("pg_database_name"), bounded("pg_schema_oid"),
        bounded("pg_schema_name"),
    );
    let rows = sqlx::query(&sql)
        .bind(row_type)
        .bind(oid)
        .bind(&config.postgres().schema.name.0)
        .fetch_all(connection)
        .await?;
    if rows.len() != 1 {
        return Err(PhysicalAttestationError::MissingLineage);
    }
    let row = &rows[0];
    if row.try_get::<i64, _>(13)? != row_type
        || row.try_get::<i64, _>(14)? != oid
        || row.try_get::<Option<i32>, _>(15)? != Some(1)
    {
        return Err(PhysicalAttestationError::Mismatch {
            field: "lineage relation binding",
        });
    }
    for index in 0..7 {
        if row
            .try_get::<Option<&str>, _>(index)?
            .is_some_and(|value| value.chars().count() > 128)
        {
            return Err(PhysicalAttestationError::InvalidValue {
                field: "oversized lineage field",
            });
        }
    }
    verify_identity(&parse_snapshot(row)?, config)
}
