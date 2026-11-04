//! Unwired PostgreSQL migration-owner laboratory.
//!
//! This is not an online migration protocol. Operators must independently
//! drain old writers, fence the primary, provision the owner/runtime roles and
//! preserve the expected lineage. No value passed here proves those facts.
//! It deliberately refuses pre-V1008 databases and does not change the epoch.

use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgConnection, Row as _};
use thiserror::Error;
use waddle_xmpp::ingress::ProtocolEpoch;

use super::runner::MIGRATION_LEDGER_ADVISORY_LOCK_KEY;
use super::MigrationRunner;
use crate::config::LineageConfig;
use crate::db::lineage::{self, AttestedLineage, LINEAGE_ADVISORY_LOCK_KEY};
use crate::db::physical_attestation::PgRoleName;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};
use crate::ingress_substrate::supported_protocol_epoch;

mod epoch_acl;
mod mam;
mod owner_rejection_grants;
#[cfg(feature = "test-support")]
mod transition_lab;

pub use epoch_acl::{inspect_dark_postgres_epoch_acl, DarkEpochAclStage};
pub use mam::{bootstrap_dark_postgres_mam_owner, DarkMamOwnerError};
pub use owner_rejection_grants::{
    grant_dark_owner_rejection_runtime_access, inspect_dark_owner_rejection_runtime_access,
    OwnerRejectionGrantError, OwnerRejectionGrantState,
};
#[cfg(feature = "test-support")]
pub use transition_lab::{
    observe_disposable_epoch_transition_lab, run_disposable_epoch_transition_lab,
    DisposableEpochTransitionAttempt, DisposableEpochTransitionConfig,
    DisposableEpochTransitionError, DisposableEpochTransitionFault,
    DisposableEpochTransitionObservation,
};

const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
const COMMIT_DEADLINE: Duration = Duration::from_secs(15);
const OPERATION_DEADLINE: Duration = Duration::from_secs(120);
const LOCK_TIMEOUT: &str = "3s";
const STATEMENT_TIMEOUT: &str = "30s";

/// The independent identity and separately provisioned roles for the lab.
#[derive(Clone, Debug)]
pub struct DarkMigrationOwnerConfig {
    expected: AttestedLineage,
    owner: PgRoleName,
    runtime: PgRoleName,
}

impl DarkMigrationOwnerConfig {
    pub fn new(
        expected: AttestedLineage,
        owner: PgRoleName,
        runtime: PgRoleName,
    ) -> Result<Self, DarkMigrationOwnerError> {
        if expected.postgres_identity.is_none() {
            return Err(DarkMigrationOwnerError::MissingPostgresIdentity);
        }
        if owner == runtime {
            return Err(DarkMigrationOwnerError::Roles);
        }
        Ok(Self {
            expected,
            owner,
            runtime,
        })
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn expected_lineage(&self) -> &AttestedLineage {
        &self.expected
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn owner(&self) -> &PgRoleName {
        &self.owner
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn runtime(&self) -> &PgRoleName {
        &self.runtime
    }
}

#[derive(Debug, Error)]
pub enum DarkMigrationOwnerError {
    #[error("migration owner requires an independently supplied PostgreSQL identity")]
    MissingPostgresIdentity,
    #[error("migration login, owner, and runtime roles are not sufficiently separated for the laboratory")]
    Roles,
    #[error("migration owner target does not match the expected primary and lineage")]
    Identity,
    #[error("migration owner backend has a TEMP namespace or unsafe replication mode")]
    ContaminatedSession,
    #[error("migration owner requires a committed V1008 ledger and epoch relation")]
    MissingEpochBaseline,
    #[error("migration owner does not support the live ingress protocol epoch")]
    UnsupportedEpoch,
    #[error("epoch ACL preflight could not match the live runtime login to the attested database")]
    RuntimeSessionUnverified,
    #[error("epoch ACL preflight found missing or changed catalog objects")]
    EpochCatalogDrift,
    #[error("epoch ACL preflight did not finish before the deadline")]
    PreflightTimeout,
    #[error("migration owner could not connect before the deadline")]
    ConnectTimeout,
    #[error("migration commit outcome is unknown; fence and reconcile before retrying")]
    CommitUnknown,
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("migration owner PostgreSQL connection failed")]
    Postgres(#[from] sqlx::Error),
}

/// Apply this binary's catalog on a new, exclusively owned PostgreSQL session.
///
/// The expected lineage and migrator login must be obtained independently of
/// `options`; the NOLOGIN owner needs read access to `pg_control_system()` for
/// physical identity verification. Use a direct, dedicated PostgreSQL backend, not a transaction
/// pooler: a fresh client connection alone does not guarantee a fresh backend.
/// The operator must drain legacy sessions and prevent new
/// writers and other migrations for the complete operation, and must fence
/// failover and the authoritative primary. Advisory locks only coordinate
/// cooperating Waddle processes; the role checks below are not a complete ACL
/// audit. The function does not attest any of those operational conditions.
///
/// An operation deadline, or any error or cancellation after COMMIT is
/// dispatched, has an unknown outcome;
/// do not retry until the old backend/primary is fenced and the ledger is
/// reconciled on the authoritative primary. A successful result means that
/// this connection received a successful COMMIT response, not that deployment
/// has been activated or that old writers have drained.
pub async fn run_dark_postgres_owner(
    options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<Vec<i64>, DarkMigrationOwnerError> {
    check_migrator_options(options, config, migrator)?;
    // The pool is private to this invocation. Direct-backend routing is an
    // operator prerequisite, not something a fresh client pool can prove.
    let pool = tokio::time::timeout(
        CONNECT_DEADLINE,
        PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone()),
    )
    .await
    .map_err(|_| DarkMigrationOwnerError::ConnectTimeout)??;
    let db = Database::from_postgres_pool("dark migration owner", pool.clone());
    let result = match tokio::time::timeout(
        OPERATION_DEADLINE,
        run_on_fresh_session(&db, config, migrator),
    )
    .await
    {
        Ok(result) => result,
        // The caller cannot tell whether cancellation raced COMMIT dispatch.
        Err(_) => Err(DarkMigrationOwnerError::CommitUnknown),
    };
    drop(db);
    // A failed/unknown commit is not made definite by closing the client.
    let _ = tokio::time::timeout(CONNECT_DEADLINE, pool.close()).await;
    result
}

fn check_migrator_options(
    options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<(), DarkMigrationOwnerError> {
    if migrator == &config.owner
        || migrator == &config.runtime
        || options.get_username() != migrator.as_str()
    {
        return Err(DarkMigrationOwnerError::Roles);
    }
    Ok(())
}

async fn run_on_fresh_session(
    db: &Database,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<Vec<i64>, DarkMigrationOwnerError> {
    let expected = config
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DarkMigrationOwnerError::MissingPostgresIdentity)?;
    let schema = &expected.schema.name.0;
    let quoted = format!("\"{}\"", schema.replace('"', "\"\""));
    let mut tx = db.begin().await?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
        .await?;
    tx.execute(
        "SELECT pg_catalog.set_config('lock_timeout', ?, true), \
         pg_catalog.set_config('statement_timeout', ?, true), \
         pg_catalog.set_config('search_path', ?, true)",
        crate::db_params![
            LOCK_TIMEOUT,
            STATEMENT_TIMEOUT,
            format!("{quoted}, pg_temp")
        ],
    )
    .await?;

    check_migration_login_and_assume_owner(
        &mut tx,
        config,
        migrator,
        schema,
        i64::from(expected.schema.oid),
    )
    .await?;
    // Lock lineage adoption before migrations. Neither check reads the epoch
    // or a protected table, and the runner never opens another connection.
    for lock in [
        LINEAGE_ADVISORY_LOCK_KEY,
        MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
    ] {
        tx.query(
            "SELECT pg_catalog.pg_advisory_xact_lock(?)",
            crate::db_params![lock],
        )
        .await?;
    }
    let lineage_config = LineageConfig {
        deployment_uuid: Some(config.expected.deployment_uuid),
        action: None,
    };
    let live = lineage::verify_qualified_in_transaction(
        &mut tx,
        DatabaseDriver::Postgres,
        &lineage_config,
        &format!("{quoted}._lineage"),
    )
    .await?;
    if live != config.expected {
        return Err(DarkMigrationOwnerError::Identity);
    }

    // Fail closed before any bootstrap DDL when this is an empty/pre-V1008
    // target. Consult catalogs, not an unqualified potentially shadowed name.
    let mut rows = tx.query(
        "SELECT pg_catalog.to_regclass(pg_catalog.format('%I.ingress_protocol_epoch', ?)) IS NOT NULL, \
         pg_catalog.to_regclass(pg_catalog.format('%I._migrations', ?)) IS NOT NULL",
        (schema.as_str(), schema.as_str()),
    ).await?;
    let row = rows
        .next()
        .await?
        .ok_or(DarkMigrationOwnerError::MissingEpochBaseline)?;
    let epoch_exists: bool = row.get(0)?;
    let ledger_exists: bool = row.get(1)?;
    if !epoch_exists || !ledger_exists {
        return Err(DarkMigrationOwnerError::MissingEpochBaseline);
    }

    // AX first avoids an AS-to-AX upgrade. AS is separately required by the
    // experimental guard's exact pg_locks predicate even when AX is held.
    tx.execute(
        &format!("LOCK TABLE ONLY {quoted}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE"),
        (),
    )
    .await?;
    tx.execute(
        &format!("LOCK TABLE ONLY {quoted}.ingress_protocol_epoch IN ACCESS SHARE MODE"),
        (),
    )
    .await?;
    check_protected_role_privileges(&mut tx, config, schema).await?;
    let mut rows = tx
        .query(
            &format!("SELECT epoch FROM {quoted}.ingress_protocol_epoch WHERE id = 1"),
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(DarkMigrationOwnerError::MissingEpochBaseline)?;
    let epoch: i64 = row.get(0)?;
    let epoch = u32::try_from(epoch).map_err(|_| DarkMigrationOwnerError::UnsupportedEpoch)?;
    if ProtocolEpoch::from_storage(epoch) > supported_protocol_epoch() {
        return Err(DarkMigrationOwnerError::UnsupportedEpoch);
    }
    tx.execute(
        "SELECT pg_catalog.set_config('waddle.protocol_epoch', ?, true), \
         pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)",
        crate::db_params![epoch.to_string()],
    ).await?;
    let mut rows = tx
        .query(
            &format!("SELECT count(*) FROM {quoted}._migrations WHERE version = 1008"),
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(DarkMigrationOwnerError::MissingEpochBaseline)?;
    let baseline: i64 = row.get(0)?;
    if baseline != 1 {
        return Err(DarkMigrationOwnerError::MissingEpochBaseline);
    }

    let newly_applied = MigrationRunner::single()
        .apply_pending_in_transaction(&mut tx, DatabaseDriver::Postgres)
        .await?;
    match tokio::time::timeout(COMMIT_DEADLINE, tx.commit()).await {
        Ok(Ok(())) => Ok(newly_applied),
        Ok(Err(_)) | Err(_) => Err(DarkMigrationOwnerError::CommitUnknown),
    }
}

// This deliberately accepts only the laboratory's direct three-role graph.
// It is not a whole-database ACL or SECURITY DEFINER audit; privileged role,
// ownership and ACL changes must remain quiescent throughout the operation.
async fn check_migration_login_and_assume_owner(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
    schema: &str,
    expected_schema_oid: i64,
) -> Result<(), DarkMigrationOwnerError> {
    let connection = tx
        .postgres_connection()
        .ok_or(DarkMigrationOwnerError::Roles)?;
    check_migration_login_on_connection(
        connection,
        config,
        migrator,
        schema,
        expected_schema_oid,
        OwnerRoleScope::Transaction,
    )
    .await
}

enum OwnerRoleScope {
    Transaction,
    Session,
}

async fn check_migration_login_on_connection(
    connection: &mut PgConnection,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
    schema: &str,
    expected_schema_oid: i64,
    scope: OwnerRoleScope,
) -> Result<(), DarkMigrationOwnerError> {
    let row = sqlx::query(ROLE_QUERY)
        .bind(config.owner.as_str())
        .bind(config.runtime.as_str())
        .bind(schema)
        .bind(migrator.as_str())
        .fetch_optional(&mut *connection)
        .await?
        .ok_or(DarkMigrationOwnerError::Roles)?;
    let valid_roles: bool = row.try_get(0)?;
    let schema_oid: i64 = row.try_get(1)?;
    let recovering: bool = row.try_get(2)?;
    let temp_schema_oid: i64 = row.try_get(3)?;
    let replication_role: String = row.try_get(4)?;
    if temp_schema_oid != 0 || replication_role != "origin" {
        return Err(DarkMigrationOwnerError::ContaminatedSession);
    }
    if !valid_roles {
        return Err(DarkMigrationOwnerError::Roles);
    }
    if schema_oid != expected_schema_oid || recovering {
        return Err(DarkMigrationOwnerError::Identity);
    }

    let quoted_owner = format!("\"{}\"", config.owner.as_str().replace('"', "\"\""));
    let command = match scope {
        OwnerRoleScope::Transaction => format!("SET LOCAL ROLE {quoted_owner}"),
        OwnerRoleScope::Session => format!("SET ROLE {quoted_owner}"),
    };
    sqlx::query(&command).execute(&mut *connection).await?;
    let row = sqlx::query(
        "SELECT session_user::text = $1 AND current_user::text = $2, \
         pg_catalog.current_schema()::text",
    )
    .bind(migrator.as_str())
    .bind(config.owner.as_str())
    .fetch_one(&mut *connection)
    .await?;
    if !row.try_get::<bool, _>(0)? {
        return Err(DarkMigrationOwnerError::Roles);
    }
    if row.try_get::<Option<String>, _>(1)?.as_deref() != Some(schema) {
        return Err(DarkMigrationOwnerError::Identity);
    }
    Ok(())
}

const ROLE_QUERY: &str = r#"SELECT session_user::text = m.rolname AND current_user::text = m.rolname
                  AND m.rolcanlogin AND NOT m.rolsuper AND NOT m.rolcreaterole
                  AND NOT m.rolcreatedb AND NOT m.rolreplication AND NOT m.rolbypassrls
                  AND NOT o.rolcanlogin AND NOT o.rolsuper AND NOT o.rolcreaterole
                  AND NOT o.rolcreatedb AND NOT o.rolreplication AND NOT o.rolbypassrls
                  AND r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreaterole
                  AND NOT r.rolcreatedb AND NOT r.rolreplication AND NOT r.rolbypassrls
                  AND n.nspowner = o.oid
                  AND m.oid <> d.datdba AND o.oid <> d.datdba AND r.oid <> d.datdba
                  AND NOT pg_catalog.has_database_privilege(m.oid, d.oid, 'CREATE,TEMP')
                  AND NOT pg_catalog.has_database_privilege(o.oid, d.oid, 'CREATE,TEMP')
                  AND NOT pg_catalog.has_database_privilege(r.oid, d.oid, 'CREATE,TEMP')
                  AND NOT pg_catalog.has_schema_privilege(m.oid, n.oid, 'CREATE')
                  AND NOT pg_catalog.has_schema_privilege(r.oid, n.oid, 'CREATE')
                  AND EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members x
                              WHERE x.member = m.oid AND x.roleid = o.oid
                                AND x.set_option AND NOT x.inherit_option AND NOT x.admin_option)
                  AND (SELECT count(*) FROM pg_catalog.pg_auth_members x WHERE x.member = m.oid) = 1
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members x
                                  WHERE x.member IN (o.oid, r.oid)
                                     OR x.roleid IN (m.oid, r.oid)
                                     OR (x.roleid = o.oid AND x.member <> m.oid))
                  AND pg_catalog.pg_has_role(m.oid, o.oid, 'SET')
                  AND NOT pg_catalog.pg_has_role(m.oid, o.oid, 'USAGE'),
                  n.oid::bigint, pg_catalog.pg_is_in_recovery(),
                  pg_catalog.pg_my_temp_schema()::bigint,
                  pg_catalog.current_setting('session_replication_role')
               FROM pg_catalog.pg_roles m
               JOIN pg_catalog.pg_roles o ON o.rolname = $1
               JOIN pg_catalog.pg_roles r ON r.rolname = $2
               JOIN pg_catalog.pg_namespace n ON n.nspname = $3
               JOIN pg_catalog.pg_database d ON d.datname = pg_catalog.current_database()
               WHERE m.rolname = $4"#;

// A bounded role check, not a whole-database ACL or SECURITY DEFINER audit.
// Provisioning must also prevent privilege changes throughout the operation.
async fn check_protected_role_privileges(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
) -> Result<(), DarkMigrationOwnerError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.pg_has_role(runtime_role.oid, epoch.relowner, 'MEMBER'), \
                pg_catalog.pg_has_role(runtime_role.oid, ledger.relowner, 'MEMBER'), \
                pg_catalog.has_table_privilege(runtime_role.oid, epoch.oid, \
                    'INSERT,UPDATE,DELETE,TRUNCATE,TRIGGER,REFERENCES'), \
                pg_catalog.has_table_privilege(runtime_role.oid, ledger.oid, \
                    'INSERT,UPDATE,DELETE,TRUNCATE,TRIGGER,REFERENCES'), \
                pg_catalog.has_any_column_privilege(runtime_role.oid, epoch.oid, \
                    'INSERT,REFERENCES'), \
                pg_catalog.has_any_column_privilege(runtime_role.oid, ledger.oid, \
                    'INSERT,UPDATE,REFERENCES'), \
                CASE WHEN pg_catalog.current_setting('server_version_num')::int >= 170000 \
                    THEN pg_catalog.has_table_privilege(runtime_role.oid, epoch.oid, 'MAINTAIN') \
                    ELSE false END, \
                CASE WHEN pg_catalog.current_setting('server_version_num')::int >= 170000 \
                    THEN pg_catalog.has_table_privilege(runtime_role.oid, ledger.oid, 'MAINTAIN') \
                    ELSE false END, \
                pg_catalog.has_any_column_privilege(runtime_role.oid, epoch.oid, 'UPDATE') \
         FROM pg_catalog.pg_roles runtime_role \
         JOIN pg_catalog.pg_namespace n ON n.nspname = ? \
         JOIN pg_catalog.pg_class epoch ON epoch.relnamespace = n.oid \
              AND epoch.relname = 'ingress_protocol_epoch' AND epoch.relkind = 'r' \
         JOIN pg_catalog.pg_class ledger ON ledger.relnamespace = n.oid \
              AND ledger.relname = '_migrations' AND ledger.relkind = 'r' \
         WHERE runtime_role.rolname = ?",
            (schema, config.runtime.as_str()),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(DarkMigrationOwnerError::MissingEpochBaseline)?;
    for column in 0..8 {
        if row.get::<bool>(column)? {
            return Err(DarkMigrationOwnerError::Roles);
        }
    }
    if row.get::<bool>(8)? {
        // V1009's FOR SHARE requires UPDATE(id). Accept that one privilege only
        // after checking the installed ledger, locked epoch guard, and exact ACL.
        let quoted = format!("\"{}\"", schema.replace('"', "\"\""));
        MigrationRunner::single()
            .verify_applied_through_in_transaction(
                tx,
                DatabaseDriver::Postgres,
                &format!("{quoted}._migrations"),
                1009,
            )
            .await?;
        epoch_acl::verify_v1009_epoch_owner_stage(tx, config, schema, &quoted).await?;
    }
    Ok(())
}
