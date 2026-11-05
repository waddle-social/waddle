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
const IDLE_TIMEOUT_LIMIT_MS: i64 = 10_000;

/// Caller-owned identity for one transition attempt. Allocate and retain it
/// before starting the cancellable operation; never reuse it for another attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisposableEpochTransitionAttempt(uuid::Uuid);

impl DisposableEpochTransitionAttempt {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub fn as_uuid(self) -> uuid::Uuid {
        self.0
    }
}

impl Default for DisposableEpochTransitionAttempt {
    fn default() -> Self {
        Self::new()
    }
}

/// A dark observation, never an authorization to replay, roll back, or activate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisposableEpochTransitionObservation {
    ObservedCommitted,
    Unresolved,
}

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
    #[error("disposable epoch transition or observation timed out")]
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
/// and record the attempt and candidate identifier/checksum in a pre-provisioned lab table.
///
/// Only fixture names beginning with `migration_transition_lab_` are accepted.
/// `_migrations` and the protocol epoch remain unchanged. A success is a
/// received COMMIT acknowledgement, not deployment or writer admission.
/// The caller must allocate and retain `attempt` before calling this cancellable
/// operation. An error or cancellation after COMMIT dispatch is indeterminate;
/// never retry without fencing and independently reconciling the target. The
/// fault enum is intentionally closed and compiled only with the `test-support` feature.
pub async fn run_disposable_epoch_transition_lab(
    migrator_options: &PgConnectOptions,
    runtime_options: &PgConnectOptions,
    config: &DisposableEpochTransitionConfig,
    attempt: DisposableEpochTransitionAttempt,
    fault: DisposableEpochTransitionFault,
) -> Result<(), DisposableEpochTransitionError> {
    match tokio::time::timeout(
        OPERATION_DEADLINE,
        run_with_connections(migrator_options, runtime_options, config, attempt, fault),
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
    attempt: DisposableEpochTransitionAttempt,
    fault: DisposableEpochTransitionFault,
) -> Result<(), DisposableEpochTransitionError> {
    check_fixture_options(migrator_options, config)?;
    if runtime_options.get_username() != config.owner.runtime.as_str() {
        return Err(DisposableEpochTransitionError::Roles);
    }

    let nonce = format!("waddle-transition-lab-{}", uuid::Uuid::new_v4().simple());

    let mut runtime = tokio::time::timeout(
        CONNECT_DEADLINE,
        PgConnection::connect_with(&runtime_options.clone().application_name(&nonce)),
    )
    .await
    .map_err(|_| DisposableEpochTransitionError::SetupTimeout)??;
    tokio::time::timeout(CONNECT_DEADLINE, bound_backend_idle_timeouts(&mut runtime))
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
        bounded_migrator_pool_options().connect_with(migrator_options.clone()),
    )
    .await
    .map_err(|_| DisposableEpochTransitionError::SetupTimeout)??;
    let db = Database::from_postgres_pool("disposable owner transition lab", pool.clone());
    let result =
        run_in_transaction(&db, &mut runtime_tx, config, pid, &nonce, attempt, fault).await;
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
    attempt: DisposableEpochTransitionAttempt,
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
    lock_marker(&mut tx, &quoted).await?;
    check_marker_shape(&mut tx, config, schema).await?;
    check_marker_empty(&mut tx, &quoted).await?;
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
        &format!("INSERT INTO {quoted}.{MARKER_TABLE} (version, checksum, attempt_id) VALUES (?, ?, ?::pg_catalog.uuid)"),
        crate::db_params![MARKER_VERSION, checksum.clone(), attempt.0.to_string()],
    )
    .await?;
    inject(fault, DisposableEpochTransitionFault::AfterMarker)?;
    if inspect_catalog_in_transaction(&mut tx, &config.owner, schema, &quoted).await?
        != DarkEpochAclStage::RelationGuardEpochZero
    {
        return Err(DisposableEpochTransitionError::UnexpectedStage);
    }
    if !marker_matches(&mut tx, &quoted, &checksum, attempt).await? {
        return Err(DisposableEpochTransitionError::Marker);
    }
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

/// Read back one attempt on a fresh migrator LOGIN, assuming the NOLOGIN owner.
///
/// This is a bounded dark observation, not a recovery or admission protocol.
/// The expected lineage and physical identity must come from an independent
/// source. A direct authoritative primary and backend (not a transaction pooler),
/// old-backend fencing, and quiescent
/// privileged DDL, roles, ACLs, ledger, marker, lineage, manifest, and epoch
/// writes are external prerequisites; none is established here. Idle timeouts
/// are installed before BEGIN, but a connection lost during the preceding
/// setup may still leave an idle backend. An absent or mismatched receipt is
/// unresolved even if the old transaction later commits.
/// Neither result authorizes replay, rollback, or activation.
pub async fn observe_disposable_epoch_transition_lab(
    migrator_options: &PgConnectOptions,
    config: &DisposableEpochTransitionConfig,
    attempt: DisposableEpochTransitionAttempt,
) -> Result<DisposableEpochTransitionObservation, DisposableEpochTransitionError> {
    tokio::time::timeout(OPERATION_DEADLINE, async {
        check_fixture_options(migrator_options, config)?;
        let pool = tokio::time::timeout(
            CONNECT_DEADLINE,
            bounded_migrator_pool_options().connect_with(migrator_options.clone()),
        )
        .await
        .map_err(|_| DisposableEpochTransitionError::SetupTimeout)??;
        let db =
            Database::from_postgres_pool("disposable owner transition observation", pool.clone());
        let result = observe_in_transaction(&db, config, attempt).await;
        drop(db);
        let _ = tokio::time::timeout(CONNECT_DEADLINE, pool.close()).await;
        result
    })
    .await
    .map_err(|_| DisposableEpochTransitionError::SetupTimeout)?
}

fn check_fixture_options(
    migrator_options: &PgConnectOptions,
    config: &DisposableEpochTransitionConfig,
) -> Result<(), DisposableEpochTransitionError> {
    let expected = config
        .owner
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DisposableEpochTransitionError::Identity)?;
    if migrator_options.get_username() != config.migrator.as_str()
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
    Ok(())
}

async fn observe_in_transaction(
    db: &Database,
    config: &DisposableEpochTransitionConfig,
    attempt: DisposableEpochTransitionAttempt,
) -> Result<DisposableEpochTransitionObservation, DisposableEpochTransitionError> {
    let expected = config
        .owner
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DisposableEpochTransitionError::Identity)?;
    let schema = &expected.schema.name.0;
    let quoted = format!("\"{}\"", schema.replace('\"', "\"\""));
    let quoted_owner = format!("\"{}\"", config.owner.owner.as_str().replace('\"', "\"\""));
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED, READ ONLY",
        (),
    )
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
    // Same lock order as the writer; AS must precede marker and catalog reads.
    tx.execute(
        &format!("LOCK TABLE ONLY {quoted}.ingress_protocol_epoch IN ACCESS SHARE MODE"),
        (),
    )
    .await?;
    lock_marker(&mut tx, &quoted).await?;
    check_marker_shape(&mut tx, config, schema).await?;
    let checksum = hex::encode(Sha256::digest(RELATION_GUARD_CANDIDATE.as_bytes()));
    let exact_marker = marker_matches(&mut tx, &quoted, &checksum, attempt).await?;
    let stage = inspect_catalog_in_transaction(&mut tx, &config.owner, schema, &quoted).await?;
    check_current_role(&mut tx, config).await?;
    tx.execute("RESET ROLE", ()).await?;
    check_roles(&mut tx, config, schema, i64::from(expected.schema.oid)).await?;
    tx.rollback().await?;
    Ok(
        if exact_marker && stage == DarkEpochAclStage::RelationGuardEpochZero {
            DisposableEpochTransitionObservation::ObservedCommitted
        } else {
            DisposableEpochTransitionObservation::Unresolved
        },
    )
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

// Install server-side session limits on each fresh backend before BEGIN. A
// client can disappear without PostgreSQL noticing its TCP connection closed.
fn bounded_migrator_pool_options() -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| {
            Box::pin(async move { bound_backend_idle_timeouts(connection).await })
        })
}

async fn bound_backend_idle_timeouts(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    let (idle, idle_in_transaction): (i64, i64) = sqlx::query_as(
        "SELECT (EXTRACT(EPOCH FROM pg_catalog.current_setting('idle_session_timeout')::interval) * 1000)::bigint, \
         (EXTRACT(EPOCH FROM pg_catalog.current_setting('idle_in_transaction_session_timeout')::interval) * 1000)::bigint",
    )
    .fetch_one(&mut *connection)
    .await?;
    let bounded = |current: i64| {
        if current <= 0 {
            IDLE_TIMEOUT_LIMIT_MS
        } else {
            current.min(IDLE_TIMEOUT_LIMIT_MS)
        }
    };
    sqlx::query(
        "SELECT pg_catalog.set_config('idle_session_timeout', $1, false), \
         pg_catalog.set_config('idle_in_transaction_session_timeout', $2, false)",
    )
    .bind(format!("{}ms", bounded(idle)))
    .bind(format!("{}ms", bounded(idle_in_transaction)))
    .execute(&mut *connection)
    .await?;
    Ok(())
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

async fn lock_marker(
    tx: &mut Transaction<'_>,
    quoted_schema: &str,
) -> Result<(), DisposableEpochTransitionError> {
    tx.execute(
        &format!("LOCK TABLE ONLY {quoted_schema}.{MARKER_TABLE} IN ACCESS SHARE MODE"),
        (),
    )
    .await?;
    Ok(())
}

async fn check_marker_shape(
    tx: &mut Transaction<'_>,
    config: &DisposableEpochTransitionConfig,
    schema: &str,
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
                   WHERE a.attrelid = c.oid AND a.attnum > 0) = 3
              AND (SELECT count(*) FROM pg_catalog.pg_attribute a
                   WHERE a.attrelid = c.oid AND NOT a.attisdropped AND a.attnotnull AND NOT a.atthasdef
                     AND a.attgenerated = '' AND a.attidentity = ''
                     AND ((a.atttypid = 'pg_catalog.text'::regtype
                           AND ((a.attnum = 1 AND a.attname = 'version')
                             OR (a.attnum = 2 AND a.attname = 'checksum')))
                       OR (a.atttypid = 'pg_catalog.uuid'::regtype
                           AND a.attnum = 3 AND a.attname = 'attempt_id'))) = 3
              AND (SELECT count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = c.oid) = 1
              AND (SELECT count(*) FROM pg_catalog.pg_index i WHERE i.indrelid = c.oid) = 1
              AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
                          JOIN pg_catalog.pg_index i ON i.indexrelid = x.conindid
                          WHERE x.conrelid = c.oid AND x.contype = 'p' AND x.convalidated
                            AND NOT x.condeferrable AND x.conkey = ARRAY[1]::smallint[]
                            AND i.indisvalid AND i.indisready AND i.indisunique AND i.indisprimary
                            AND i.indnkeyatts = 1 AND i.indnatts = 1
                            AND i.indexprs IS NULL AND i.indpred IS NULL)
              AND EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                              coalesce(c.relacl, pg_catalog.acldefault('r', c.relowner))
                          ) a WHERE a.grantee = runtime.oid AND a.privilege_type = 'SELECT'
                                   AND NOT a.is_grantable)
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                                  coalesce(c.relacl, pg_catalog.acldefault('r', c.relowner))
                              ) a WHERE a.grantee NOT IN (o.oid, runtime.oid)
                                   OR (a.grantee = runtime.oid
                                       AND (a.privilege_type <> 'SELECT' OR a.is_grantable)))
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute col
                              CROSS JOIN LATERAL pg_catalog.aclexplode(col.attacl) a
                              WHERE col.attrelid = c.oid
                                AND a.grantee <> c.relowner)
           FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_catalog.pg_roles o ON o.rolname = ?
           JOIN pg_catalog.pg_roles runtime ON runtime.rolname = ?
           WHERE n.nspname = ? AND c.relname = ?"#,
            crate::db_params![config.owner.owner.as_str(), config.owner.runtime.as_str(), schema, MARKER_TABLE],
        )
        .await?;
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

async fn check_marker_empty(
    tx: &mut Transaction<'_>,
    quoted_schema: &str,
) -> Result<(), DisposableEpochTransitionError> {
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

async fn marker_matches(
    tx: &mut Transaction<'_>,
    quoted_schema: &str,
    checksum: &str,
    attempt: DisposableEpochTransitionAttempt,
) -> Result<bool, DisposableEpochTransitionError> {
    // Text equality inherits the column collation, which may be case- or
    // accent-insensitive. Compare the UTF-8 bytes of the receipt instead.
    let mut rows = tx
        .query(
            &format!(
                "SELECT count(*) = 1 AND bool_and(\
            pg_catalog.convert_to(version, 'UTF8') = pg_catalog.convert_to(?, 'UTF8') AND \
            pg_catalog.convert_to(checksum, 'UTF8') = pg_catalog.convert_to(?, 'UTF8') AND \
            attempt_id = ?::pg_catalog.uuid) FROM {quoted_schema}.{MARKER_TABLE}"
            ),
            crate::db_params![MARKER_VERSION, checksum, attempt.0.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<bool>(0)?),
        None => Err(DisposableEpochTransitionError::Marker),
    }
}
