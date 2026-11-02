//! Disposable, test-support-only epoch-zero owner transition experiment.
//!
//! This is not a registered migration or an online rollout protocol. The test
//! operator must quiesce old writers and all DDL, role, ACL, lineage, ledger,
//! epoch, manifest and marker writes, and fence failover for the full operation.
//! Runtime correlation assumes honest endpoints and no replay. Privileged
//! functions and arbitrary application-table ACLs are not audited.

use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPoolOptions};
use sqlx::Connection;
use thiserror::Error;

use super::epoch_acl::{
    correlate_runtime, inspect_catalog_in_transaction, runtime_pid, RELATION_GUARD_CANDIDATE,
};
use super::{
    DarkEpochAclStage, DarkMigrationOwnerConfig, DarkMigrationOwnerError, CONNECT_DEADLINE,
    OPERATION_DEADLINE,
};
use crate::config::LineageConfig;
use crate::db::lineage::{self, AttestedLineage, LINEAGE_ADVISORY_LOCK_KEY};
use crate::db::migrations::runner::MIGRATION_LEDGER_ADVISORY_LOCK_KEY;
use crate::db::physical_attestation::PgRoleName;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

const MARKER_VERSION: &str = "relation-guard-lab-1";
const MARKER_TABLE: &str = "waddle_epoch_transition_lab_marker";

/// Inputs for the disposable three-role test experiment.
#[derive(Clone, Debug)]
pub struct DisposableEpochTransitionConfig {
    owner: DarkMigrationOwnerConfig,
    migrator: PgRoleName,
}

impl DisposableEpochTransitionConfig {
    pub fn new(
        expected: AttestedLineage,
        migrator: PgRoleName,
        owner: PgRoleName,
        runtime: PgRoleName,
    ) -> Result<Self, DisposableEpochTransitionError> {
        if migrator == owner || migrator == runtime {
            return Err(DisposableEpochTransitionError::Roles);
        }
        Ok(Self {
            owner: DarkMigrationOwnerConfig::new(expected, owner, runtime)?,
            migrator,
        })
    }
}

/// Deterministic fault points for the disposable laboratory only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisposableEpochTransitionFault {
    None,
    AfterCandidate,
    AfterRevoke,
    AfterMarker,
    CommitAcknowledgementLost,
}

#[derive(Debug, Error)]
pub enum DisposableEpochTransitionError {
    #[error("disposable epoch transition roles are not sufficiently separated")]
    Roles,
    #[error("disposable epoch transition target is not the independently expected primary")]
    Identity,
    #[error("disposable epoch transition does not start at the exact V1009 ACL stage")]
    UnexpectedStage,
    #[error("disposable epoch transition marker is missing, unsafe, or already populated")]
    Marker,
    #[error("disposable epoch transition injected a pre-commit fault")]
    Injected,
    #[error("disposable epoch transition connection or setup timed out before commit")]
    SetupTimeout,
    #[error("disposable epoch transition commit outcome is unknown; do not replay without reconciliation")]
    CommitUnknown,
    #[error(transparent)]
    Observation(#[from] DarkMigrationOwnerError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("disposable epoch transition PostgreSQL connection failed")]
    Postgres(#[from] sqlx::Error),
}

/// Atomically replace the two laboratory guards, revoke runtime UPDATE(id),
/// and record the candidate identifier/checksum in a pre-provisioned lab table.
///
/// Only fixture names beginning with `migration_transition_lab_` are accepted.
/// `_migrations` and the protocol epoch remain unchanged. A success is a
/// received COMMIT acknowledgement, not deployment or writer admission.
/// An error or cancellation after COMMIT dispatch is indeterminate; never retry
/// without fencing and independently reconciling the target. The fault enum is
/// intentionally closed and compiled only with the `test-support` feature.
pub async fn run_disposable_epoch_transition_lab(
    migrator_options: &PgConnectOptions,
    runtime_options: &PgConnectOptions,
    config: &DisposableEpochTransitionConfig,
    fault: DisposableEpochTransitionFault,
) -> Result<(), DisposableEpochTransitionError> {
    match tokio::time::timeout(
        OPERATION_DEADLINE,
        run_with_connections(migrator_options, runtime_options, config, fault),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(DisposableEpochTransitionError::CommitUnknown),
    }
}

async fn run_with_connections(
    migrator_options: &PgConnectOptions,
    runtime_options: &PgConnectOptions,
    config: &DisposableEpochTransitionConfig,
    fault: DisposableEpochTransitionFault,
) -> Result<(), DisposableEpochTransitionError> {
    let expected = config
        .owner
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DisposableEpochTransitionError::Identity)?;
    if migrator_options.get_username() != config.migrator.as_str()
        || runtime_options.get_username() != config.owner.runtime.as_str()
        || !expected
            .database
            .name
            .0
            .starts_with("migration_transition_lab_")
        || !expected
            .schema
            .name
            .0
            .starts_with("migration_transition_lab_")
        || !config
            .migrator
            .as_str()
            .starts_with("migration_transition_lab_")
        || !config
            .owner
            .owner
            .as_str()
            .starts_with("migration_transition_lab_")
        || !config
            .owner
            .runtime
            .as_str()
            .starts_with("migration_transition_lab_")
    {
        return Err(DisposableEpochTransitionError::Roles);
    }

    let nonce = format!("waddle-transition-lab-{}", uuid::Uuid::new_v4().simple());
    let mut runtime = tokio::time::timeout(
        CONNECT_DEADLINE,
        PgConnection::connect_with(&runtime_options.clone().application_name(&nonce)),
    )
    .await
    .map_err(|_| DisposableEpochTransitionError::SetupTimeout)??;
    let mut runtime_tx = tokio::time::timeout(CONNECT_DEADLINE, runtime.begin())
        .await
        .map_err(|_| DisposableEpochTransitionError::SetupTimeout)??;
    tokio::time::timeout(
        CONNECT_DEADLINE,
        sqlx::query("SET TRANSACTION READ ONLY").execute(&mut *runtime_tx),
    )
    .await
    .map_err(|_| DisposableEpochTransitionError::SetupTimeout)??;
    let pid = tokio::time::timeout(
        CONNECT_DEADLINE,
        runtime_pid(&mut runtime_tx, &config.owner, &nonce),
    )
    .await
    .map_err(|_| DisposableEpochTransitionError::SetupTimeout)??;

    let pool = tokio::time::timeout(
        CONNECT_DEADLINE,
        PgPoolOptions::new()
            .max_connections(1)
            .connect_with(migrator_options.clone()),
    )
    .await
    .map_err(|_| DisposableEpochTransitionError::SetupTimeout)??;
    let db = Database::from_postgres_pool("disposable owner transition lab", pool.clone());
    let result = run_in_transaction(&db, &mut runtime_tx, config, pid, &nonce, fault).await;
    drop(db);
    let _ = tokio::time::timeout(CONNECT_DEADLINE, pool.close()).await;
    let _ = tokio::time::timeout(CONNECT_DEADLINE, runtime_tx.rollback()).await;
    drop(runtime);
    result
}

async fn run_in_transaction(
    db: &Database,
    runtime_tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    config: &DisposableEpochTransitionConfig,
    runtime_pid_before: i32,
    nonce: &str,
    fault: DisposableEpochTransitionFault,
) -> Result<(), DisposableEpochTransitionError> {
    let expected = config
        .owner
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DisposableEpochTransitionError::Identity)?;
    let schema = &expected.schema.name.0;
    let quoted = format!("\"{}\"", schema.replace('"', "\"\""));
    let quoted_owner = format!("\"{}\"", config.owner.owner.as_str().replace('"', "\"\""));
    let quoted_runtime = format!("\"{}\"", config.owner.runtime.as_str().replace('"', "\"\""));
    let mut tx = db.begin().await?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
        .await?;
    set_bounded_session(&mut tx, &quoted).await?;
    check_roles(&mut tx, config, schema, i64::from(expected.schema.oid)).await?;
    tx.execute(&format!("SET LOCAL ROLE {quoted_owner}"), ())
        .await?;
    check_current_role(&mut tx, config).await?;

    for key in [
        LINEAGE_ADVISORY_LOCK_KEY,
        MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
    ] {
        tx.query(
            "SELECT pg_catalog.pg_advisory_xact_lock(?)",
            crate::db_params![key],
        )
        .await?;
    }
    let lineage_config = LineageConfig {
        deployment_uuid: Some(config.owner.expected.deployment_uuid),
        action: None,
    };
    let observed = lineage::verify_qualified_in_transaction(
        &mut tx,
        DatabaseDriver::Postgres,
        &lineage_config,
        &format!("{quoted}._lineage"),
    )
    .await?;
    if observed != config.owner.expected {
        return Err(DisposableEpochTransitionError::Identity);
    }
    correlate_runtime(&mut tx, &config.owner, runtime_pid_before, nonce).await?;
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
    check_marker(&mut tx, config, schema, &quoted).await?;
    if inspect_catalog_in_transaction(&mut tx, &config.owner, schema, &quoted).await?
        != DarkEpochAclStage::V1009EpochZero
    {
        return Err(DisposableEpochTransitionError::UnexpectedStage);
    }
    tx.execute_batch(RELATION_GUARD_CANDIDATE).await?;
    inject(fault, DisposableEpochTransitionFault::AfterCandidate)?;
    tx.execute(
        &format!("REVOKE UPDATE (id) ON {quoted}.ingress_protocol_epoch FROM {quoted_runtime}"),
        (),
    )
    .await?;
    inject(fault, DisposableEpochTransitionFault::AfterRevoke)?;
    let checksum = hex::encode(Sha256::digest(RELATION_GUARD_CANDIDATE.as_bytes()));
    tx.execute(
        &format!("INSERT INTO {quoted}.{MARKER_TABLE} (version, checksum) VALUES (?, ?)"),
        crate::db_params![MARKER_VERSION, checksum.clone()],
    )
    .await?;
    inject(fault, DisposableEpochTransitionFault::AfterMarker)?;
    if inspect_catalog_in_transaction(&mut tx, &config.owner, schema, &quoted).await?
        != DarkEpochAclStage::RelationGuardEpochZero
    {
        return Err(DisposableEpochTransitionError::UnexpectedStage);
    }
    verify_marker(&mut tx, &quoted, &checksum).await?;
    if runtime_pid(runtime_tx, &config.owner, nonce).await? != runtime_pid_before {
        return Err(DarkMigrationOwnerError::RuntimeSessionUnverified.into());
    }
    correlate_runtime(&mut tx, &config.owner, runtime_pid_before, nonce).await?;
    check_current_role(&mut tx, config).await?;
    match tokio::time::timeout(CONNECT_DEADLINE, tx.commit()).await {
        Ok(Ok(())) if fault != DisposableEpochTransitionFault::CommitAcknowledgementLost => Ok(()),
        Ok(Ok(())) | Ok(Err(_)) | Err(_) => Err(DisposableEpochTransitionError::CommitUnknown),
    }
}

fn inject(
    actual: DisposableEpochTransitionFault,
    point: DisposableEpochTransitionFault,
) -> Result<(), DisposableEpochTransitionError> {
    if actual == point {
        Err(DisposableEpochTransitionError::Injected)
    } else {
        Ok(())
    }
}

async fn set_bounded_session(
    tx: &mut Transaction<'_>,
    quoted_schema: &str,
) -> Result<(), DisposableEpochTransitionError> {
    let mut rows = tx
        .query(
            "SELECT (EXTRACT(EPOCH FROM pg_catalog.current_setting('lock_timeout')::interval) * 1000)::bigint, \
             (EXTRACT(EPOCH FROM pg_catalog.current_setting('statement_timeout')::interval) * 1000)::bigint",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(DisposableEpochTransitionError::Identity)?;
    let lock: i64 = row.get(0)?;
    let statement: i64 = row.get(1)?;
    let bounded = |current: i64, maximum: i64| {
        if current <= 0 {
            maximum
        } else {
            current.min(maximum)
        }
    };
    tx.execute(
        "SELECT pg_catalog.set_config('lock_timeout', ?, true), \
         pg_catalog.set_config('statement_timeout', ?, true), \
         pg_catalog.set_config('search_path', ?, true)",
        crate::db_params![
            format!("{}ms", bounded(lock, 3_000)),
            format!("{}ms", bounded(statement, 30_000)),
            format!("{quoted_schema}, pg_temp")
        ],
    )
    .await?;
    Ok(())
}

async fn check_roles(
    tx: &mut Transaction<'_>,
    config: &DisposableEpochTransitionConfig,
    schema: &str,
    schema_oid: i64,
) -> Result<(), DisposableEpochTransitionError> {
    let mut rows = tx
        .query(
            r#"SELECT session_user::text = m.rolname AND current_user::text = m.rolname
                  AND m.rolcanlogin AND NOT m.rolsuper AND NOT m.rolcreaterole
                  AND NOT m.rolcreatedb AND NOT m.rolreplication AND NOT m.rolbypassrls
                  AND NOT o.rolcanlogin AND NOT o.rolsuper AND NOT o.rolcreaterole
                  AND NOT o.rolcreatedb AND NOT o.rolreplication AND NOT o.rolbypassrls
                  AND n.oid::bigint = ? AND n.nspowner = o.oid
                  AND m.oid <> d.datdba AND o.oid <> d.datdba
                  AND NOT pg_catalog.has_database_privilege(m.oid, d.oid, 'CREATE,TEMP')
                  AND NOT pg_catalog.has_database_privilege(o.oid, d.oid, 'CREATE,TEMP')
                  AND NOT pg_catalog.has_schema_privilege(m.oid, n.oid, 'CREATE')
                  AND EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members x
                              WHERE x.member = m.oid AND x.roleid = o.oid
                                AND x.set_option AND NOT x.inherit_option AND NOT x.admin_option)
                  AND (SELECT count(*) FROM pg_catalog.pg_auth_members x WHERE x.member = m.oid) = 1
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members x
                                  WHERE x.member IN (o.oid, r.oid)
                                     OR (x.member = m.oid AND x.roleid <> o.oid)
                                     OR x.roleid IN (m.oid, r.oid)
                                     OR (x.roleid = o.oid AND x.member <> m.oid))
                  AND pg_catalog.pg_has_role(m.oid, o.oid, 'SET')
                  AND NOT pg_catalog.pg_has_role(m.oid, o.oid, 'USAGE')
                  AND pg_catalog.pg_my_temp_schema() = 0
                  AND pg_catalog.current_setting('session_replication_role') = 'origin'
                  AND NOT pg_catalog.pg_is_in_recovery()
               FROM pg_catalog.pg_roles m
               JOIN pg_catalog.pg_roles o ON o.rolname = ?
               JOIN pg_catalog.pg_roles r ON r.rolname = ?
               JOIN pg_catalog.pg_namespace n ON n.nspname = ?
               JOIN pg_catalog.pg_database d ON d.datname = pg_catalog.current_database()
               WHERE m.rolname = ?"#,
            crate::db_params![
                schema_oid,
                config.owner.owner.as_str(),
                config.owner.runtime.as_str(),
                schema,
                config.migrator.as_str()
            ],
        )
        .await?;
    let valid = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if valid {
        Ok(())
    } else {
        Err(DisposableEpochTransitionError::Roles)
    }
}

async fn check_current_role(
    tx: &mut Transaction<'_>,
    config: &DisposableEpochTransitionConfig,
) -> Result<(), DisposableEpochTransitionError> {
    let mut rows = tx
        .query(
            "SELECT session_user::text = ? AND current_user::text = ?",
            (config.migrator.as_str(), config.owner.owner.as_str()),
        )
        .await?;
    let valid = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if valid {
        Ok(())
    } else {
        Err(DisposableEpochTransitionError::Roles)
    }
}

async fn check_marker(
    tx: &mut Transaction<'_>,
    config: &DisposableEpochTransitionConfig,
    schema: &str,
    quoted_schema: &str,
) -> Result<(), DisposableEpochTransitionError> {
    let mut rows = tx
        .query(
            r#"SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND c.relowner = o.oid
              AND NOT c.relispartition AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity
              AND NOT c.relhasrules
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid)
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                              WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
              AND (SELECT count(*) FROM pg_catalog.pg_attribute a
                   WHERE a.attrelid = c.oid AND a.attnum > 0) = 2
              AND (SELECT count(*) FROM pg_catalog.pg_attribute a
                   WHERE a.attrelid = c.oid AND NOT a.attisdropped AND a.attnotnull AND NOT a.atthasdef
                     AND a.attgenerated = '' AND a.attidentity = ''
                     AND a.atttypid = 'pg_catalog.text'::regtype
                     AND ((a.attnum = 1 AND a.attname = 'version')
                       OR (a.attnum = 2 AND a.attname = 'checksum'))) = 2
              AND (SELECT count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = c.oid) = 1
              AND (SELECT count(*) FROM pg_catalog.pg_index i WHERE i.indrelid = c.oid) = 1
              AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
                          JOIN pg_catalog.pg_index i ON i.indexrelid = x.conindid
                          WHERE x.conrelid = c.oid AND x.contype = 'p' AND x.convalidated
                            AND NOT x.condeferrable AND x.conkey = ARRAY[1]::smallint[]
                            AND i.indisvalid AND i.indisready AND i.indisunique AND i.indisprimary
                            AND i.indnkeyatts = 1 AND i.indnatts = 1
                            AND i.indexprs IS NULL AND i.indpred IS NULL)
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                                  coalesce(c.relacl, pg_catalog.acldefault('r', c.relowner))
                              ) a WHERE a.grantee <> c.relowner)
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute col
                              CROSS JOIN LATERAL pg_catalog.aclexplode(col.attacl) a
                              WHERE col.attrelid = c.oid AND col.attnum > 0
                                AND a.grantee <> c.relowner)
           FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_catalog.pg_roles o ON o.rolname = ?
           WHERE n.nspname = ? AND c.relname = ?"#,
            crate::db_params![
                config.owner.owner.as_str(),
                schema,
                MARKER_TABLE
            ],
        )
        .await?;
    let valid = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !valid {
        return Err(DisposableEpochTransitionError::Marker);
    }
    let mut rows = tx
        .query(
            &format!("SELECT count(*) FROM {quoted_schema}.{MARKER_TABLE}"),
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(DisposableEpochTransitionError::Marker)?;
    if row.get::<i64>(0)? != 0 {
        return Err(DisposableEpochTransitionError::Marker);
    }
    Ok(())
}

async fn verify_marker(
    tx: &mut Transaction<'_>,
    quoted_schema: &str,
    checksum: &str,
) -> Result<(), DisposableEpochTransitionError> {
    let mut rows = tx.query(
        &format!("SELECT count(*) = 1 AND bool_and(version = ? AND checksum = ?) FROM {quoted_schema}.{MARKER_TABLE}"),
        (MARKER_VERSION, checksum),
    ).await?;
    let valid = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if valid {
        Ok(())
    } else {
        Err(DisposableEpochTransitionError::Marker)
    }
}
