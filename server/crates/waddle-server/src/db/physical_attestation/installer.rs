//! Dark installer for independently provisioned PostgreSQL roles.
//!
//! The operator owns role lifecycle. This module neither creates roles nor
//! grants the definer access to PostgreSQL's control function or lineage.

use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row};
use uuid::Uuid;

use super::{
    identity_select, quote_identifier, read_preflight_snapshot, read_snapshot, verify_identity,
    PhysicalAttestationConfig, PhysicalAttestationError, FUNCTION_NAME,
};
use crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY;

// The two-key advisory namespace is distinct from the existing one-key
// lineage and migration locks. The second key is derived from the trusted
// schema, not the operation ID: different operations for one target serialize.
const INSTALL_LOCK_NAMESPACE: i32 = 1_464_448_084;
const COMMIT_DEADLINE: Duration = Duration::from_secs(15);
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// An absent installation is not proof that an earlier request cannot commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationStatus {
    Committed,
    Indeterminate,
}

/// Install or verify one operation against independently provisioned roles.
/// Retrying the same operation ID and exact specification is safe only while
/// every installer follows this protocol on one fenced, authoritative primary.
/// A different operation or an unexpected pre-existing function is refused.
pub async fn install(
    options: &PgConnectOptions,
    config: PhysicalAttestationConfig,
    operation_id: Uuid,
) -> Result<PhysicalAttestationConfig, PhysicalAttestationError> {
    let mut connection = connect_installer(options, &config).await?;
    let mut tx = connection.begin().await?;
    prepare(&mut tx, &config, false).await?;
    let marker = marker(&config, operation_id);
    match inspect_locked(&mut tx, &config, &marker).await? {
        InstallationStatus::Committed => {}
        InstallationStatus::Indeterminate => {
            create_function(&mut tx, &config, &marker).await?;
            verify_prerequisites(&mut tx, &config).await?;
            verify_function(&mut tx, &config, &marker).await?;
            let snapshot = read_snapshot(&mut tx, &config).await?;
            verify_identity(&snapshot, &config)?;
        }
    }
    // A cancelled or failed COMMIT can have succeeded at PostgreSQL. The
    // operation ID is known to the caller before this future starts.
    match tokio::time::timeout(COMMIT_DEADLINE, tx.commit()).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => {
            return Err(PhysicalAttestationError::CommitUnknown { operation_id });
        }
    }
    drop(connection);
    match inspect_installation(options, &config, operation_id).await {
        Ok(InstallationStatus::Committed) => Ok(config),
        Ok(InstallationStatus::Indeterminate) => {
            Err(PhysicalAttestationError::PostCommitUnverified {
                operation_id,
                source: Box::new(PhysicalAttestationError::InstallationIndeterminate),
            })
        }
        Err(source) => Err(PhysicalAttestationError::PostCommitUnverified {
            operation_id,
            source: Box::new(source),
        }),
    }
}

/// Fresh, locked readback. Absence is indeterminate, not a rollback verdict.
/// The caller must separately establish primary fencing and transport trust.
pub async fn inspect_installation(
    options: &PgConnectOptions,
    config: &PhysicalAttestationConfig,
    operation_id: Uuid,
) -> Result<InstallationStatus, PhysicalAttestationError> {
    let mut connection = connect_installer(options, config).await?;
    let mut tx = connection.begin().await?;
    prepare(&mut tx, config, true).await?;
    let result = inspect_locked(&mut tx, config, &marker(config, operation_id)).await?;
    tx.commit().await?;
    Ok(result)
}

async fn connect_installer(
    options: &PgConnectOptions,
    config: &PhysicalAttestationConfig,
) -> Result<PgConnection, PhysicalAttestationError> {
    if options.get_username() != config.installer_login.as_str() {
        return Err(PhysicalAttestationError::WrongInstallerLogin);
    }
    tokio::time::timeout(CONNECT_DEADLINE, PgConnection::connect_with(options))
        .await
        .map_err(|_| PhysicalAttestationError::InstallationIndeterminate)?
        .map_err(PhysicalAttestationError::from)
}

async fn prepare(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
    read_only: bool,
) -> Result<(), PhysicalAttestationError> {
    // This must precede the first SELECT, including either advisory lock.
    let isolation = if read_only {
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY"
    } else {
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE"
    };
    sqlx::query(isolation).execute(&mut *connection).await?;
    sqlx::query("SET LOCAL lock_timeout = '5s'")
        .execute(&mut *connection)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '10s'")
        .execute(&mut *connection)
        .await?;
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;
    // Enrollment/adoption takes this same lock. Always order it before the
    // installation lock so lineage cannot change during the preflight/commit.
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(LINEAGE_ADVISORY_LOCK_KEY)
        .execute(&mut *connection)
        .await
        .map_err(lock_error)?;
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1, pg_catalog.hashtext($2))")
        .bind(INSTALL_LOCK_NAMESPACE)
        .bind(&config.trusted_schema.0)
        .execute(&mut *connection)
        .await
        .map_err(lock_error)?;
    let row = sqlx::query(
        "SELECT session_user::text, current_user::text, pg_catalog.pg_is_in_recovery()",
    )
    .fetch_one(&mut *connection)
    .await?;
    let session: String = row.try_get(0)?;
    let current: String = row.try_get(1)?;
    if session != config.installer_login.as_str() || current != config.installer_login.as_str() {
        return Err(PhysicalAttestationError::Mismatch {
            field: "installer login/session role",
        });
    }
    if row.try_get::<bool, _>(2)? {
        return Err(PhysicalAttestationError::NotPrimary);
    }
    verify_prerequisites(&mut *connection, config).await?;
    sqlx::query(&format!(
        "SET LOCAL ROLE {}",
        quote_identifier(config.definer_role.as_str())
    ))
    .execute(&mut *connection)
    .await?;
    let row = sqlx::query("SELECT session_user::text, current_user::text")
        .fetch_one(&mut *connection)
        .await?;
    if row.try_get::<String, _>(0)? != config.installer_login.as_str()
        || row.try_get::<String, _>(1)? != config.definer_role.as_str()
    {
        return Err(PhysicalAttestationError::Mismatch {
            field: "definer role handoff",
        });
    }
    // Both _lineage SELECT and pg_control_system EXECUTE belong to the
    // provisioned definer, not the restricted installer login.
    let snapshot = read_preflight_snapshot(connection, config).await?;
    verify_identity(&snapshot, config)
}

fn lock_error(error: sqlx::Error) -> PhysicalAttestationError {
    let code = error
        .as_database_error()
        .and_then(|database| database.code());
    if matches!(code.as_deref(), Some("55P03" | "57014")) {
        PhysicalAttestationError::InstallationIndeterminate
    } else {
        PhysicalAttestationError::Database(error)
    }
}

async fn verify_prerequisites(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
) -> Result<(), PhysicalAttestationError> {
    let row = sqlx::query(r#"
        SELECT
          NOT d.rolcanlogin AND NOT d.rolsuper AND NOT d.rolcreatedb AND NOT d.rolcreaterole
            AND NOT d.rolreplication AND NOT d.rolbypassrls AS definer_attributes,
          i.rolcanlogin AND NOT i.rolsuper AND NOT i.rolcreatedb AND NOT i.rolcreaterole
            AND NOT i.rolreplication AND NOT i.rolbypassrls AS installer_attributes,
          r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreatedb AND NOT r.rolcreaterole
            AND NOT r.rolreplication AND NOT r.rolbypassrls AS runtime_attributes,
          EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.roleid = d.oid AND m.member = i.oid
                    AND m.set_option AND NOT m.inherit_option AND NOT m.admin_option)
            AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
              WHERE m.member IN (d.oid, r.oid)
                 OR (m.member = i.oid AND m.roleid <> d.oid)
                 OR m.roleid IN (i.oid, r.oid)
                 OR (m.roleid = d.oid AND m.member <> i.oid)) AS role_graph,
          trusted.nspowner = d.oid AS trusted_owner,
          NOT pg_catalog.has_schema_privilege(r.oid, trusted.oid, 'CREATE')
            AND NOT pg_catalog.has_schema_privilege(r.oid, target.oid, 'CREATE')
            AND NOT pg_catalog.has_schema_privilege(i.oid, trusted.oid, 'CREATE')
            AND NOT pg_catalog.has_schema_privilege(i.oid, target.oid, 'CREATE')
            AND NOT pg_catalog.has_schema_privilege(d.oid, target.oid, 'CREATE')
            AND NOT pg_catalog.has_database_privilege(r.oid, pg_catalog.current_database(), 'CREATE')
            AND NOT pg_catalog.has_database_privilege(r.oid, pg_catalog.current_database(), 'TEMP')
            AND NOT pg_catalog.has_database_privilege(i.oid, pg_catalog.current_database(), 'CREATE')
            AND NOT pg_catalog.has_database_privilege(i.oid, pg_catalog.current_database(), 'TEMP')
            AND NOT pg_catalog.has_database_privilege(d.oid, pg_catalog.current_database(), 'CREATE') AS no_create,
          db.datdba NOT IN (i.oid, r.oid, d.oid)
            AND target.nspowner NOT IN (i.oid, r.oid, d.oid)
            AND lineage.relowner NOT IN (i.oid, r.oid, d.oid) AS non_owner,
          pg_catalog.has_schema_privilege(d.oid, target.oid, 'USAGE')
            AND pg_catalog.has_table_privilege(d.oid, lineage.oid, 'SELECT')
            AND pg_catalog.has_function_privilege(d.oid, 'pg_catalog.pg_control_system()'::pg_catalog.regprocedure, 'EXECUTE') AS definer_read,
          NOT pg_catalog.has_function_privilege(r.oid, 'pg_catalog.pg_control_system()'::pg_catalog.regprocedure, 'EXECUTE') AS runtime_control_denied,
          NOT pg_catalog.has_function_privilege(i.oid, 'pg_catalog.pg_control_system()'::pg_catalog.regprocedure, 'EXECUTE') AS installer_control_denied,
          NOT pg_catalog.has_table_privilege(r.oid, lineage.oid, 'INSERT, UPDATE, DELETE, TRUNCATE, TRIGGER, REFERENCES')
            AND NOT pg_catalog.has_any_column_privilege(r.oid, lineage.oid, 'INSERT, UPDATE') AS runtime_lineage_read_only,
          NOT pg_catalog.has_table_privilege(d.oid, lineage.oid, 'INSERT, UPDATE, DELETE, TRUNCATE, TRIGGER, REFERENCES')
            AND NOT pg_catalog.has_any_column_privilege(d.oid, lineage.oid, 'INSERT, UPDATE') AS definer_lineage_read_only,
          NOT pg_catalog.has_table_privilege(i.oid, lineage.oid, 'INSERT, UPDATE, DELETE, TRUNCATE, TRIGGER, REFERENCES')
            AND NOT pg_catalog.has_any_column_privilege(i.oid, lineage.oid, 'INSERT, UPDATE') AS installer_lineage_read_only,
          NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
              COALESCE(trusted.nspacl, pg_catalog.acldefault('n', trusted.nspowner))) a
            WHERE a.grantee NOT IN (trusted.nspowner, r.oid)
              OR (a.grantee = r.oid AND (a.privilege_type <> 'USAGE' OR a.is_grantable))) AS trusted_acl
        FROM pg_catalog.pg_roles d
        JOIN pg_catalog.pg_roles i ON i.rolname = $2
        JOIN pg_catalog.pg_roles r ON r.rolname = $3
        JOIN pg_catalog.pg_namespace trusted ON trusted.nspname = $4
        JOIN pg_catalog.pg_namespace target ON target.nspname = $5
        JOIN pg_catalog.pg_class lineage ON lineage.relnamespace = target.oid AND lineage.relname = '_lineage' AND lineage.relkind = 'r'
        JOIN pg_catalog.pg_database db ON db.datname = pg_catalog.current_database()
        WHERE d.rolname = $1
    "#)
        .bind(config.definer_role.as_str())
        .bind(config.installer_login.as_str())
        .bind(config.runtime_login.as_str())
        .bind(&config.trusted_schema.0)
        .bind(&config.postgres().schema.name.0)
        .fetch_optional(&mut *connection).await?
        .ok_or(PhysicalAttestationError::Mismatch { field: "preprovisioned roles and schemas" })?;
    for (column, field) in [
        ("definer_attributes", "definer privileges"),
        ("installer_attributes", "installer privileges"),
        ("runtime_attributes", "runtime privileges"),
        ("role_graph", "role membership"),
        ("trusted_owner", "trusted schema owner"),
        ("no_create", "installer, runtime or definer CREATE/TEMP"),
        ("non_owner", "database or lineage ownership"),
        ("definer_read", "definer read grants"),
        (
            "runtime_control_denied",
            "runtime control function privileges",
        ),
        (
            "installer_control_denied",
            "installer control function privileges",
        ),
        (
            "runtime_lineage_read_only",
            "runtime lineage modification privileges",
        ),
        (
            "definer_lineage_read_only",
            "definer lineage modification privileges",
        ),
        (
            "installer_lineage_read_only",
            "installer lineage modification privileges",
        ),
        ("trusted_acl", "trusted schema ACL grantees"),
    ] {
        if !row.try_get::<bool, _>(column)? {
            return Err(PhysicalAttestationError::Mismatch { field });
        }
    }
    Ok(())
}

async fn inspect_locked(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
    marker: &str,
) -> Result<InstallationStatus, PhysicalAttestationError> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = $2")
        .bind(&config.trusted_schema.0).bind(FUNCTION_NAME).fetch_one(&mut *connection).await?;
    if count == 0 {
        return Ok(InstallationStatus::Indeterminate);
    }
    if count != 1 {
        return Err(PhysicalAttestationError::InstallationConflict);
    }
    // Never invoke an existing SECURITY DEFINER function before this exact
    // catalog/ACL check, even when it advertises the expected comment marker.
    verify_function(&mut *connection, config, marker).await?;
    let snapshot = read_snapshot(connection, config).await?;
    verify_identity(&snapshot, config)?;
    Ok(InstallationStatus::Committed)
}

fn marker(config: &PhysicalAttestationConfig, operation_id: Uuid) -> String {
    let mut digest = Sha256::new();
    for part in [
        "waddle-physical-attestation-v1".to_owned(),
        config.lineage.deployment_uuid.to_string(),
        config.lineage.lineage_uuid.to_string(),
        config.postgres().system_identifier.0.to_string(),
        config.postgres().database.oid.to_string(),
        config.postgres().database.name.0.clone(),
        config.postgres().schema.oid.to_string(),
        config.postgres().schema.name.0.clone(),
        config.installer_login.as_str().to_owned(),
        config.runtime_login.as_str().to_owned(),
        config.definer_role.as_str().to_owned(),
        config.trusted_schema.0.clone(),
        identity_select(config),
    ] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!(
        "waddle-attestation-v1:{operation_id}:{}",
        hex::encode(digest.finalize())
    )
}

async fn create_function(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
    marker: &str,
) -> Result<(), PhysicalAttestationError> {
    let function = config.function_path();
    let runtime = quote_identifier(config.runtime_login.as_str());
    let trusted = quote_identifier(&config.trusted_schema.0);
    let sql = format!(
        r#"CREATE FUNCTION {function}()
RETURNS TABLE (
    lineage_uuid text, deployment_uuid text,
    stored_system_identifier text, stored_database_oid text, stored_database_name text,
    stored_schema_oid text, stored_schema_name text,
    live_system_identifier text, live_database_oid text, live_database_name text,
    live_schema_oid text, live_schema_name text
)
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $waddle_attestation${}$waddle_attestation$"#,
        identity_select(config)
    );
    sqlx::query(&sql).execute(&mut *connection).await?;
    for statement in [
        format!("REVOKE ALL ON FUNCTION {function}() FROM PUBLIC"),
        format!("GRANT USAGE ON SCHEMA {trusted} TO {runtime}"),
        format!("GRANT EXECUTE ON FUNCTION {function}() TO {runtime}"),
        // The marker contains only a fixed prefix, UUID and lowercase hex.
        format!("COMMENT ON FUNCTION {function}() IS '{marker}'"),
    ] {
        sqlx::query(&statement).execute(&mut *connection).await?;
    }
    Ok(())
}

async fn verify_function(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
    marker: &str,
) -> Result<(), PhysicalAttestationError> {
    let row = sqlx::query(r#"
        SELECT p.proowner = d.oid AND n.nspowner = d.oid
          AND l.lanname = 'sql' AND p.prokind = 'f' AND p.pronargs = 0
          AND p.pronargdefaults = 0 AND p.proargdefaults IS NULL AND p.provariadic = 0
          AND p.prosqlbody IS NULL AND p.probin IS NULL AND p.protrftypes IS NULL AND p.prosupport = 0
          AND p.proretset AND p.prorettype = 'pg_catalog.record'::pg_catalog.regtype
          AND p.proallargtypes = pg_catalog.array_fill('pg_catalog.text'::pg_catalog.regtype::oid, ARRAY[12])
          AND p.proargmodes = pg_catalog.array_fill('t'::"char", ARRAY[12])
          AND p.proargnames = ARRAY['lineage_uuid', 'deployment_uuid',
            'stored_system_identifier', 'stored_database_oid', 'stored_database_name',
            'stored_schema_oid', 'stored_schema_name', 'live_system_identifier',
            'live_database_oid', 'live_database_name', 'live_schema_oid', 'live_schema_name']::text[]
          AND p.prosecdef AND p.provolatile = 'v' AND p.proparallel = 'u' AND NOT p.proleakproof AND NOT p.proisstrict
          AND p.proconfig = ARRAY['search_path=pg_catalog, pg_temp']::text[]
          AND p.prosrc = $4 AND pg_catalog.obj_description(p.oid, 'pg_proc') = $5
          AND pg_catalog.has_function_privilege(r.oid, p.oid, 'EXECUTE')
          AND pg_catalog.has_schema_privilege(r.oid, n.oid, 'USAGE')
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
              COALESCE(p.proacl, pg_catalog.acldefault('f', p.proowner))) a
            WHERE a.grantee NOT IN (p.proowner, r.oid)
              OR (a.grantee = r.oid AND a.is_grantable)) AS exact
        FROM pg_catalog.pg_proc p
        JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
        JOIN pg_catalog.pg_roles d ON d.rolname = $2
        JOIN pg_catalog.pg_roles r ON r.rolname = $3
        JOIN pg_catalog.pg_language l ON l.oid = p.prolang
        WHERE n.nspname = $1 AND p.proname = $6
    "#)
        .bind(&config.trusted_schema.0)
        .bind(config.definer_role.as_str())
        .bind(config.runtime_login.as_str())
        .bind(identity_select(config))
        .bind(marker)
        .bind(FUNCTION_NAME)
        .fetch_optional(connection).await?;
    if !matches!(row, Some(ref row) if matches!(row.try_get::<bool, _>("exact"), Ok(true))) {
        return Err(PhysicalAttestationError::InstallationConflict);
    }
    Ok(())
}
