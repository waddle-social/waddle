//! Global owner bootstrap and read-only runtime attachment boundaries.
//!
//! Owner startup migrates and provisions under a credential that owns the
//! schema. The restricted mode attaches a physically attested pool under the
//! runtime role and verifies lineage, ledger and global ACLs without DDL.

use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use thiserror::Error;
use tracing::info;
use waddle_xmpp::mam::{MamSchemaBootstrapError, SqlxMamStorage};
use waddle_xmpp::XmppError;

use crate::config::LineageConfig;
use crate::notification_outbox::{NotificationOutboxError, NotificationOutboxStore};
use crate::push_service::DatabasePushServiceStore;

use super::lineage::{self, AttestedLineage, LineageAction, LineageError, LineageUuid};
use super::physical_attestation::{attested_pool, PhysicalAttestationConfig};
use super::{
    Database, DatabaseConfig, DatabaseDriver, DatabaseError, DatabasePool, MigrationRunner,
    Transaction,
};

#[derive(Debug, Error)]
pub enum OwnerBootstrapError {
    #[error("failed to bootstrap database lineage table")]
    LineageTable(#[source] DatabaseError),
    #[error("failed to enroll global database lineage")]
    Enrollment(#[source] DatabaseError),
    #[error("failed to apply global database lineage adoption")]
    Adoption(#[source] DatabaseError),
    #[error(
        "refusing to start clustering against a database with no lineage row: \
         enroll it first (WADDLE_DB_LINEAGE_ACTION=enroll on one rollout)"
    )]
    MissingClusteredLineage,
    #[error(
        "refusing to run migrations: the global database's lineage does not \
         describe this deployment (mis-pointed DSN, unadopted restore, or \
         missing WADDLE_DEPLOYMENT_UUID)"
    )]
    PreMigrationLineage(#[source] DatabaseError),
    #[error("failed to run migrations")]
    Migrations(#[source] DatabaseError),
}

#[derive(Debug, Error)]
pub enum SharedOwnerBootstrapError {
    #[error("shared owner startup requires PostgreSQL")]
    PostgresRequired,
    #[error("shared owner pool capacity exceeds the supported range")]
    CapacityExceeded,
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Global(#[from] OwnerBootstrapError),
    #[error("legacy MAM owner preparation failed and may have changed schema")]
    Mam(#[source] MamSchemaBootstrapError),
    #[error("legacy Push Service owner preparation failed")]
    Push(#[source] XmppError),
    #[error("legacy notification outbox owner preparation failed")]
    NotificationOutbox(#[source] NotificationOutboxError),
}

/// Bootstrap the ordinary owner path, then seal its one physical PostgreSQL
/// pool into main and ingress admission classes before any handle escapes.
/// MAM uses the current legacy initializer, on an owned connection detached
/// from this pool. This path makes no restricted-role or attestation claim.
pub async fn bootstrap_shared_owner_postgres(
    mut config: DatabaseConfig,
    lineage: &LineageConfig,
    clustering_enabled: bool,
    push_secret_key: &[u8],
    main_budget: u32,
    ingress_budget: u32,
) -> Result<(DatabasePool, Option<LineageUuid>, NotificationOutboxStore), SharedOwnerBootstrapError>
{
    if config.driver != DatabaseDriver::Postgres {
        return Err(SharedOwnerBootstrapError::PostgresRequired);
    }
    if main_budget == 0 {
        return Err(DatabaseError::PoolSizeZero { which: "main" }.into());
    }
    if ingress_budget == 0 {
        return Err(DatabaseError::PoolSizeZero { which: "ingress" }.into());
    }
    let physical = main_budget
        .checked_add(ingress_budget)
        .ok_or(DatabaseError::PoolSizeOverflow)?;
    let total = physical
        .checked_add(config.control_plane_pool.map_or(0, |pool| pool.size))
        .ok_or(DatabaseError::PoolSizeOverflow)?;
    if total > MAX_VERIFIED_RUNTIME_POOL_CONNECTIONS {
        return Err(SharedOwnerBootstrapError::CapacityExceeded);
    }
    config.pool_size = physical;
    let database = Database::from_config("global", &config).await?;
    let global_adopt_matched =
        bootstrap_global_owner(&database, lineage, clustering_enabled).await?;

    let session = database.detached_postgres_startup_connection().await?;
    let session = SqlxMamStorage::bootstrap_legacy_postgres_schema_on_owned_connection(session)
        .await
        .map_err(SharedOwnerBootstrapError::Mam)?;
    drop(session);

    // Outbox ancestry adoption reads Push Service publish jobs. Preserve the
    // legacy startup order before outbox takes its detached schema-gate
    // session. Neither temporary store may escape with unlaned admission.
    let push = DatabasePushServiceStore::new_with_secret_key(database.clone(), push_secret_key)
        .await
        .map_err(SharedOwnerBootstrapError::Push)?;
    drop(push);
    let prepared_outbox = NotificationOutboxStore::prepare_legacy_owner_shared(&database)
        .await
        .map_err(SharedOwnerBootstrapError::NotificationOutbox)?;

    let database = database.seal_owner_postgres_lanes(main_budget, ingress_budget)?;
    let outbox =
        NotificationOutboxStore::attach_prepared_shared_owner(database.clone(), prepared_outbox)
            .await
            .map_err(SharedOwnerBootstrapError::NotificationOutbox)?;
    Ok((
        DatabasePool::from_database(database),
        global_adopt_matched,
        outbox,
    ))
}

#[derive(Debug, Error)]
pub enum StoreOwnerBootstrapError {
    #[error("failed to bootstrap store database lineage table")]
    LineageTable(#[source] DatabaseError),
    #[error("failed to enroll store database lineage")]
    Enrollment(#[source] DatabaseError),
    #[error("failed to apply store database lineage adoption")]
    Adoption(#[source] DatabaseError),
}

#[derive(Debug, Error)]
pub enum RuntimeAttachmentError {
    #[error("runtime attachment cannot process a lineage enrollment or adoption action")]
    OperatorActionConfigured,
    #[error("runtime attachment requires an operator-supplied PostgreSQL identity")]
    MissingExpectedPostgresIdentity,
    #[error("runtime attachment identity differs from the expected committed installation")]
    ExpectedIdentityMismatch,
    #[error("restricted global runtime role or privileges are unsafe at {component}")]
    UnsafeGlobalRuntime { component: &'static str },
    #[error("could not verify the PostgreSQL {setting} bound for runtime attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[cfg(feature = "test-support")]
    #[error("MAM runtime role or privileges do not match the restricted attachment")]
    UnsafeMamRuntime,
    #[cfg(feature = "test-support")]
    #[error("MAM schema does not match the owner-installed contract")]
    MamSchema,
    #[error(
        "verified runtime pool requests {requested} connections, exceeding the resource safety limit of {maximum}"
    )]
    PoolCapacityExceeded { requested: u32, maximum: u32 },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// Resource safety bound for this dark constructor, not a PostgreSQL limit.
pub const MAX_VERIFIED_RUNTIME_POOL_CONNECTIONS: u32 = 1024;

/// Separate admission limits over the physically attested main pool.
/// Main and ingress share one backend pool but cannot borrow admission from
/// each other. Their combined size plus any control-plane pool is bounded by
/// [`MAX_VERIFIED_RUNTIME_POOL_CONNECTIONS`].
#[derive(Debug, Clone, Copy)]
pub struct RuntimePoolLimits {
    pub main_connections: u32,
    pub ingress_connections: u32,
    pub acquire_timeout: Duration,
}

/// Capacity for a separately attested control-plane pool. Its admission
/// capacity is independent from the main and ingress lanes.
#[derive(Debug, Clone, Copy)]
pub struct ControlPlanePoolLimits {
    pub connections: u32,
    pub acquire_timeout: Duration,
}

/// An attested global database whose lineage and migration ledger were
/// verified before this handle was created. The ledger verification describes
/// that transaction's snapshot.
pub struct VerifiedGlobalAttachment {
    _database: Database,
}

impl VerifiedGlobalAttachment {
    /// Consume this verified attachment into the server pool without reopening
    /// a DSN or detaching its physical-attestation and admission capabilities.
    /// Call within a running Tokio runtime: the pool owns a database actor.
    pub fn into_pool(self) -> super::DatabasePool {
        super::DatabasePool::from_database(self._database)
    }
}

#[cfg(feature = "test-support")]
impl VerifiedGlobalAttachment {
    pub(crate) fn database_for_test(&self) -> &Database {
        &self._database
    }
}

/// Construct one physically attested PostgreSQL pool with independent main
/// and ingress admission, then verify its physical capability, qualified
/// lineage and complete migration ledger in the same read-only transaction.
///
/// `expected` is a separate operator-supplied installation identity; it must
/// not be obtained from this pool or its capability. This function compares
/// that input with the attestor configuration, but does not establish the
/// input's provenance, audit all runtime grants, drain existing sessions or
/// fence a PostgreSQL primary. The endpoint must retain a server session
/// across checkout and transaction start. No schema repair, owner bootstrap
/// or store initialization is performed.
pub async fn attach_verified_global_runtime(
    options: PgConnectOptions,
    attestation: PhysicalAttestationConfig,
    expected: &AttestedLineage,
    limits: RuntimePoolLimits,
) -> Result<VerifiedGlobalAttachment, RuntimeAttachmentError> {
    attach_verified_global_runtime_inner(options, None, attestation, expected, limits).await
}

/// Attach a global runtime database with a separate physically attested
/// control-plane pool. Both pools use the same expected physical identity,
/// lineage, and runtime login, but retain independent connection and admission
/// budgets. The control-plane pool is checked before this private attachment
/// is returned; a failed checkout cannot fall back to an ordinary pool or
/// reopen a DSN.
///
/// This constructor performs no owner DDL and does not audit all runtime
/// grants, drain existing sessions, or fence a PostgreSQL primary.
pub async fn attach_verified_global_runtime_with_control_plane(
    options: PgConnectOptions,
    control_plane_options: PgConnectOptions,
    attestation: PhysicalAttestationConfig,
    expected: &AttestedLineage,
    limits: RuntimePoolLimits,
    control_plane_limits: ControlPlanePoolLimits,
) -> Result<VerifiedGlobalAttachment, RuntimeAttachmentError> {
    attach_verified_global_runtime_inner(
        options,
        Some((control_plane_options, control_plane_limits)),
        attestation,
        expected,
        limits,
    )
    .await
}

async fn attach_verified_global_runtime_inner(
    options: PgConnectOptions,
    control_plane: Option<(PgConnectOptions, ControlPlanePoolLimits)>,
    attestation: PhysicalAttestationConfig,
    expected: &AttestedLineage,
    limits: RuntimePoolLimits,
) -> Result<VerifiedGlobalAttachment, RuntimeAttachmentError> {
    let total = limits
        .main_connections
        .checked_add(limits.ingress_connections)
        .ok_or(DatabaseError::PoolSizeOverflow)?;
    if limits.main_connections == 0 {
        return Err(DatabaseError::PoolSizeZero { which: "main" }.into());
    }
    if limits.ingress_connections == 0 {
        return Err(DatabaseError::PoolSizeZero { which: "ingress" }.into());
    }
    if let Some((_, control_plane_limits)) = &control_plane {
        if control_plane_limits.connections == 0 {
            return Err(DatabaseError::PoolSizeZero {
                which: "control-plane",
            }
            .into());
        }
    }
    let total = match &control_plane {
        Some((_, limits)) => total
            .checked_add(limits.connections)
            .ok_or(DatabaseError::PoolSizeOverflow)?,
        None => total,
    };
    if total > MAX_VERIFIED_RUNTIME_POOL_CONNECTIONS {
        return Err(RuntimeAttachmentError::PoolCapacityExceeded {
            requested: total,
            maximum: MAX_VERIFIED_RUNTIME_POOL_CONNECTIONS,
        });
    }
    if expected.postgres_identity.is_none() {
        return Err(RuntimeAttachmentError::MissingExpectedPostgresIdentity);
    }
    if attestation.expected_lineage() != expected {
        return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
    }

    let main_pool_size = limits
        .main_connections
        .checked_add(limits.ingress_connections)
        .ok_or(DatabaseError::PoolSizeOverflow)?;
    let has_control_plane = control_plane.is_some();
    let pool = attested_pool(
        options,
        attestation.clone(),
        main_pool_size,
        limits.acquire_timeout,
    )
    .map_err(DatabaseError::from)?;
    let control_plane_pool = control_plane
        .map(|(options, control_plane_limits)| {
            attested_pool(
                options,
                attestation,
                control_plane_limits.connections,
                control_plane_limits.acquire_timeout,
            )
            .map(|pool| (pool, control_plane_limits.connections))
            .map_err(DatabaseError::from)
        })
        .transpose()?;
    let database = Database::from_attested_postgres_pool_with_ingress_and_control_plane(
        "global",
        pool,
        control_plane_pool,
        limits.main_connections,
        limits.ingress_connections,
    )
    .await?;
    let config = LineageConfig {
        deployment_uuid: Some(expected.deployment_uuid),
        action: None,
    };
    verify_global_runtime(&database, &config, expected).await?;
    if has_control_plane {
        let mut rows = database
            .control_plane_guard()
            .await?
            .query("SELECT pg_catalog.current_database()", ())
            .await?;
        if rows.next().await?.is_none() {
            return Err(DatabaseError::QueryFailed(
                "control-plane attachment returned no database identity".to_owned(),
            )
            .into());
        }
    }
    Ok(VerifiedGlobalAttachment {
        _database: database,
    })
}

/// The existing owner startup path, made explicit so a later credential split
/// can move it before restricted runtime attachment. This still runs under the
/// current startup credential and is not a role transition.
pub async fn bootstrap_global_owner(
    db: &Database,
    config: &LineageConfig,
    clustering_enabled: bool,
) -> Result<Option<LineageUuid>, OwnerBootstrapError> {
    lineage::ensure_table(db)
        .await
        .map_err(OwnerBootstrapError::LineageTable)?;

    let global_adopt_matched = match config.action.as_ref() {
        Some(LineageAction::Enroll) => {
            info!(store = "global", "enrolling database lineage");
            lineage::enroll(db, config)
                .await
                .map_err(OwnerBootstrapError::Enrollment)?;
            None
        }
        Some(LineageAction::Adopt(expected)) => {
            match lineage::adopt_if_matched(db, config, expected)
                .await
                .map_err(OwnerBootstrapError::Adoption)?
            {
                lineage::AdoptOutcome::Adopted { matched, .. } => {
                    info!(store = "global", matched = %matched, "adopted database lineage");
                    Some(matched)
                }
                lineage::AdoptOutcome::NotMatched => None,
            }
        }
        None => None,
    };

    match lineage::verify(db, config).await {
        Ok(_) => {}
        Err(DatabaseError::Lineage(LineageError::MissingRow)) => {
            if clustering_enabled && !matches!(config.action.as_ref(), Some(LineageAction::Enroll))
            {
                return Err(OwnerBootstrapError::MissingClusteredLineage);
            }
            info!("global database has no lineage row yet; proceeding to migrations un-enrolled");
        }
        Err(error) => return Err(OwnerBootstrapError::PreMigrationLineage(error)),
    }

    MigrationRunner::single()
        .run(db)
        .await
        .map_err(OwnerBootstrapError::Migrations)?;
    Ok(global_adopt_matched)
}

/// Preserve the existing per-store lineage owner action. Store schema
/// constructors currently precede this call, so this is not yet a complete
/// owner bootstrap or a runtime attachment boundary.
pub async fn bootstrap_store_owner(
    db: &Database,
    config: &LineageConfig,
) -> Result<Option<LineageUuid>, StoreOwnerBootstrapError> {
    lineage::ensure_table(db)
        .await
        .map_err(StoreOwnerBootstrapError::LineageTable)?;
    match config.action.as_ref() {
        Some(LineageAction::Enroll) => {
            lineage::enroll(db, config)
                .await
                .map_err(StoreOwnerBootstrapError::Enrollment)?;
            Ok(None)
        }
        Some(LineageAction::Adopt(expected)) => {
            match lineage::adopt_if_matched(db, config, expected)
                .await
                .map_err(StoreOwnerBootstrapError::Adoption)?
            {
                lineage::AdoptOutcome::Adopted { matched, .. } => Ok(Some(matched)),
                lineage::AdoptOutcome::NotMatched => Ok(None),
            }
        }
        None => Ok(None),
    }
}

/// Prove that this already-provisioned global database has an attested lineage
/// and the complete committed migration catalog. No schema repair, enrollment,
/// adoption or checksum backfill is attempted. `expected` must come from an
/// owner/operator handoff independent of the runtime DSN; that production
/// handoff is a later gate. PostgreSQL enforces read-only execution for the
/// whole verification transaction.
pub async fn verify_global_runtime(
    db: &Database,
    config: &LineageConfig,
    expected: &AttestedLineage,
) -> Result<AttestedLineage, RuntimeAttachmentError> {
    let (mut tx, driver, lineage_table, ledger_table) =
        begin_runtime_lineage_check(db, config, expected, true).await?;
    let attested =
        verify_runtime_lineage(db, &mut tx, driver, config, expected, &lineage_table).await?;
    if &attested != expected {
        return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
    }
    MigrationRunner::single()
        .verify_complete_in_transaction(&mut tx, driver, &ledger_table)
        .await?;
    if db.attested_pg_writer_capability().is_some() {
        let schema = &expected
            .postgres_identity
            .as_ref()
            .ok_or(RuntimeAttachmentError::MissingExpectedPostgresIdentity)?
            .schema
            .name
            .0;
        if let Some(component) = super::runtime_store_acl::verify_global(&mut tx, schema).await? {
            return Err(RuntimeAttachmentError::UnsafeGlobalRuntime { component });
        }
    }
    tx.commit().await?;
    Ok(attested)
}

/// Snapshot-only MAM attachment proof on the same admitted global database.
/// This performs no initialization and exposes no connection or pool. The
/// caller still has to fence old writers and privileged DDL/ACL changes.
#[cfg(feature = "test-support")]
pub(crate) async fn verify_mam_runtime_attachment(
    db: &Database,
    owner: &super::DarkMigrationOwnerConfig,
) -> Result<(), RuntimeAttachmentError> {
    if db.driver() != DatabaseDriver::Postgres || db.attested_pg_writer_capability().is_none() {
        return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
    }
    let expected = owner.expected_lineage();
    let expected_schema = &expected
        .postgres_identity
        .as_ref()
        .ok_or(RuntimeAttachmentError::MissingExpectedPostgresIdentity)?
        .schema
        .name
        .0;
    super::physical_attestation::validate_identifier(expected_schema, "MAM schema")
        .map_err(|_| RuntimeAttachmentError::ExpectedIdentityMismatch)?;
    let config = LineageConfig {
        deployment_uuid: Some(expected.deployment_uuid),
        action: None,
    };
    let (mut tx, driver, lineage_table, ledger_table) =
        begin_runtime_lineage_check(db, &config, expected, true).await?;
    let schema = &expected
        .postgres_identity
        .as_ref()
        .ok_or(RuntimeAttachmentError::MissingExpectedPostgresIdentity)?
        .schema
        .name
        .0;
    let mut rows = tx
        .query(
            "SELECT session_user::text = r.rolname AND current_user::text = r.rolname \
         AND r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreaterole \
         AND NOT r.rolcreatedb AND NOT r.rolreplication AND NOT r.rolbypassrls \
         AND NOT o.rolcanlogin AND NOT o.rolsuper AND NOT o.rolcreaterole \
         AND NOT o.rolcreatedb AND NOT o.rolreplication AND NOT o.rolbypassrls \
         AND n.nspowner = o.oid AND r.oid <> d.datdba AND o.oid <> d.datdba \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member = r.oid) \
         AND NOT pg_catalog.pg_has_role(r.oid, o.oid, 'SET') \
         AND NOT pg_catalog.has_database_privilege(r.oid, d.oid, 'CREATE,TEMP') \
         AND NOT pg_catalog.has_schema_privilege(r.oid, n.oid, 'CREATE') \
         AND NOT pg_catalog.has_parameter_privilege(r.oid, 'session_replication_role', 'SET') \
         AND pg_catalog.current_setting('session_replication_role') = 'origin' \
         FROM pg_catalog.pg_roles r JOIN pg_catalog.pg_roles o ON o.rolname = ? \
         JOIN pg_catalog.pg_namespace n ON n.nspname = ? \
         JOIN pg_catalog.pg_database d ON d.datname = pg_catalog.current_database() \
         WHERE r.rolname = ?",
            crate::db_params![
                owner.owner().as_str(),
                schema.as_str(),
                owner.runtime().as_str()
            ],
        )
        .await?;
    let role_ok = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !role_ok {
        return Err(RuntimeAttachmentError::UnsafeMamRuntime);
    }
    verify_mam_runtime_acl(&mut tx, schema, owner.runtime().as_str()).await?;
    let attested =
        verify_runtime_lineage(db, &mut tx, driver, &config, expected, &lineage_table).await?;
    if &attested != expected {
        return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
    }
    MigrationRunner::single()
        .verify_complete_in_transaction(&mut tx, driver, &ledger_table)
        .await?;
    let connection = tx
        .postgres_connection()
        .ok_or(RuntimeAttachmentError::MamSchema)?;
    waddle_xmpp::mam::SqlxMamStorage::verify_postgres_schema_on_connection(
        connection,
        schema,
        owner.owner().as_str(),
    )
    .await
    .map_err(|_| RuntimeAttachmentError::MamSchema)?;
    tx.rollback().await?;
    Ok(())
}

/// Check effective privileges and direct PUBLIC ACLs on the relations used by
/// this dark attachment. This is not an audit of unrelated roles or definer
/// functions. ACLs and role membership must remain quiescent after the check.
#[cfg(feature = "test-support")]
pub(crate) async fn verify_mam_runtime_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
    runtime: &str,
) -> Result<(), RuntimeAttachmentError> {
    let mut rows = tx.query(
        r#"WITH runtime AS (
             SELECT oid FROM pg_catalog.pg_roles WHERE rolname = ?
           ), relations AS (
             SELECT c.* FROM pg_catalog.pg_class c
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = ? AND c.relkind = 'r'
               AND c.relname IN ('mam_messages','mam_archive_sequences','_lineage','_migrations')
           )
           SELECT (SELECT count(*) FROM relations) = 4 AND COALESCE((
             SELECT pg_catalog.bool_and(
               c.relowner <> r.oid
               AND pg_catalog.has_table_privilege(r.oid,c.oid,'SELECT')
               AND NOT pg_catalog.has_table_privilege(r.oid,c.oid,'DELETE, TRUNCATE, REFERENCES, TRIGGER')
               AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid,'REFERENCES')
               AND NOT pg_catalog.has_table_privilege(r.oid,c.oid,
                 'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION, UPDATE WITH GRANT OPTION, DELETE WITH GRANT OPTION, TRUNCATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION, TRIGGER WITH GRANT OPTION')
               AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid,
                 'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION, UPDATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION')
               AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000
                 THEN NOT pg_catalog.has_table_privilege(r.oid,c.oid,'MAINTAIN') ELSE true END
               AND NOT EXISTS (
                 SELECT 1 FROM pg_catalog.aclexplode(
                   CASE WHEN pg_catalog.cardinality(c.relacl) > 0 THEN c.relacl
                        ELSE pg_catalog.acldefault('r', c.relowner) END
                 ) a WHERE a.grantee = 0
               )
               AND NOT EXISTS (
                 SELECT 1 FROM pg_catalog.pg_attribute a
                 CROSS JOIN LATERAL pg_catalog.aclexplode(
                   CASE WHEN pg_catalog.cardinality(a.attacl) > 0 THEN a.attacl
                        ELSE pg_catalog.acldefault('r', c.relowner) END
                 ) acl
                 WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                   AND acl.grantee = 0
               )
               AND CASE c.relname
                 WHEN 'mam_messages' THEN
                   pg_catalog.has_table_privilege(r.oid,c.oid,'INSERT')
                   AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid,'UPDATE')
                 WHEN 'mam_archive_sequences' THEN
                   pg_catalog.has_table_privilege(r.oid,c.oid,'INSERT')
                   AND NOT pg_catalog.has_table_privilege(r.oid,c.oid,'UPDATE')
                   AND pg_catalog.has_column_privilege(r.oid,c.oid,'next_seq','UPDATE')
                   AND NOT EXISTS (
                     SELECT 1 FROM pg_catalog.pg_attribute a
                     WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                       AND a.attname <> 'next_seq'
                       AND pg_catalog.has_column_privilege(r.oid,c.oid,a.attnum,'UPDATE')
                   )
                 ELSE NOT pg_catalog.has_table_privilege(r.oid,c.oid,'INSERT, UPDATE')
                   AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid,'INSERT, UPDATE')
                 END
             ) FROM relations c CROSS JOIN runtime r
           ), false)"#,
        crate::db_params![runtime, schema],
    ).await?;
    let safe = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !safe {
        return Err(RuntimeAttachmentError::UnsafeMamRuntime);
    }
    Ok(())
}

/// Verify only the committed lineage and physical identity of an already
/// provisioned durable store. The store's schema and grants are separate
/// owner/bootstrap gates; this is not wired into normal startup yet.
pub async fn verify_store_lineage_runtime(
    db: &Database,
    config: &LineageConfig,
    expected: &AttestedLineage,
) -> Result<AttestedLineage, RuntimeAttachmentError> {
    let (mut tx, driver, lineage_table, _) =
        begin_runtime_lineage_check(db, config, expected, false).await?;
    let attested =
        verify_runtime_lineage(db, &mut tx, driver, config, expected, &lineage_table).await?;
    if &attested != expected {
        return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
    }
    tx.commit().await?;
    Ok(attested)
}

async fn verify_runtime_lineage(
    db: &Database,
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    config: &LineageConfig,
    expected: &AttestedLineage,
    lineage_table: &str,
) -> Result<AttestedLineage, RuntimeAttachmentError> {
    if let Some(capability) = db.attested_pg_writer_capability() {
        db.verify_attested_pg_writer_capability(&capability)
            .map_err(DatabaseError::from)?;
        if &capability.attested_lineage_for_deployment(config.deployment_uuid)? != expected {
            return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
        }
        capability.attest_transaction(tx).await?;
        Ok(
            lineage::verify_qualified_with_attested_identity_in_transaction(
                tx,
                config,
                lineage_table,
                capability.identity(),
            )
            .await?,
        )
    } else {
        Ok(lineage::verify_qualified_in_transaction(tx, driver, config, lineage_table).await?)
    }
}

async fn begin_runtime_lineage_check<'a>(
    db: &'a Database,
    config: &LineageConfig,
    expected: &AttestedLineage,
    require_ledger: bool,
) -> Result<(Transaction<'a>, DatabaseDriver, String, String), RuntimeAttachmentError> {
    if config.action.is_some() {
        return Err(RuntimeAttachmentError::OperatorActionConfigured);
    }
    let driver = db.driver();
    if driver == DatabaseDriver::Postgres && expected.postgres_identity.is_none() {
        return Err(RuntimeAttachmentError::MissingExpectedPostgresIdentity);
    }
    if config.deployment_uuid != Some(expected.deployment_uuid) {
        return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
    }
    let mut tx = db.begin().await?;
    let (lineage_table, ledger_table) = if driver == DatabaseDriver::Postgres {
        // A raw pooled BEGIN can survive a prior borrower, and SQLx's BEGIN
        // then only warns about a nested transaction. Two different
        // pre-query isolation settings make an already-used inherited
        // transaction refuse this check instead of reusing its old snapshot.
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
            (),
        )
        .await?;
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
            (),
        )
        .await?;
        if db.attested_pg_writer_capability().is_some() {
            super::read_only_timeout::bound_postgres_waits(&mut tx)
                .await
                .map_err(|error| match error {
                    super::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                        RuntimeAttachmentError::InvalidTimeoutSetting { setting }
                    }
                    super::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                        RuntimeAttachmentError::Database(error)
                    }
                })?;
        }
        let schema = &expected
            .postgres_identity
            .as_ref()
            .ok_or(RuntimeAttachmentError::MissingExpectedPostgresIdentity)?
            .schema
            .name
            .0;
        let quoted_schema = format!("\"{}\"", schema.replace('"', "\"\""));
        // Lock before any SELECT fixes the repeatable-read snapshot. The
        // store-only verifier does not require a global migration ledger.
        tx.execute(
            &format!("LOCK TABLE {quoted_schema}._lineage IN ACCESS SHARE MODE"),
            (),
        )
        .await?;
        if require_ledger {
            tx.execute(
                &format!("LOCK TABLE {quoted_schema}._migrations IN ACCESS SHARE MODE"),
                (),
            )
            .await?;
        }
        {
            let mut rows = tx.query("SELECT pg_catalog.current_schema()", ()).await?;
            let row = rows
                .next()
                .await?
                .ok_or(RuntimeAttachmentError::ExpectedIdentityMismatch)?;
            let active: Option<String> = row.get(0)?;
            if active.as_deref() != Some(schema) {
                return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
            }
        }
        // A session TEMP table can shadow `_lineage` or `_migrations`.
        // Omit pg_catalog so PostgreSQL searches it implicitly first for
        // built-ins, while the trusted schema remains current_schema().
        // pg_temp is explicitly last for relation lookup.
        let safe_path = format!("{quoted_schema}, pg_temp");
        tx.execute(
            "SELECT pg_catalog.set_config('search_path', ?, true)",
            crate::db_params![safe_path],
        )
        .await?;
        (
            format!("{quoted_schema}._lineage"),
            format!("{quoted_schema}._migrations"),
        )
    } else {
        ("main._lineage".to_string(), "main._migrations".to_string())
    };
    Ok((tx, driver, lineage_table, ledger_table))
}
