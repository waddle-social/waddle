//! Implementation behind the parent module facade.

use super::backend::{PoolAdmission, PoolLane};
use super::lineage;
use super::migrations;
use super::physical_attestation::{
    AttestedPgPool, AttestedPostgresWriterCapability, PhysicalAttestationError,
};
use super::Error;
use super::FromStr;
#[cfg(test)]
use super::Path;
use super::PgPool;
use super::SqliteConnectOptions;
use super::SqlitePool;
use super::{connect_backend, DatabaseBackend};
use super::{debug, info, instrument};
use super::{ConnectionGuard, DatabaseDriver, Transaction};
use super::{CONTROL_PLANE_POOL_ACQUIRE_TIMEOUT, MAIN_POOL_ACQUIRE_TIMEOUT};
use std::time::Duration;

/// Database-specific errors.
#[derive(Error, Debug)]
pub enum DatabaseError {
    #[error("Failed to connect to database: {0}")]
    ConnectionFailed(String),

    #[error("Database query failed: {0}")]
    QueryFailed(String),

    /// Fail-closed startup refusal for an append-only migration ledger
    /// violation, rather than an environmental database failure.
    #[error(transparent)]
    MigrationLedger(#[from] migrations::MigrationLedgerError),

    /// Fail-closed readiness refusal for a database whose durable lineage
    /// attestation is missing, malformed, or does not describe this database.
    #[error(transparent)]
    Lineage(#[from] lineage::LineageError),

    /// Fail-closed refusal when a writer capability is detached from
    /// its physically attested PostgreSQL pool or connection.
    #[error(transparent)]
    PhysicalAttestation(#[from] PhysicalAttestationError),

    /// A migration statement batch (or its ledger record insert) failed while
    /// applying. Carries which migration so a crash-looping pod's log names it.
    #[error("migration v{version} ({description}) failed to apply")]
    MigrationApply {
        version: i64,
        description: String,
        #[source]
        source: Box<DatabaseError>,
    },

    #[error("Internal database error: {0}")]
    Internal(#[from] sqlx::Error),

    /// `DatabaseConfig::control_plane_pool` was set on a non-Postgres
    /// config. The clustering control plane (ADR-0017 element 4/12) has no
    /// SQLite equivalent — clustering itself requires Postgres — so a
    /// control-plane pool on any other driver is a caller bug, not an
    /// environmental failure.
    #[error(
        "control-plane pool configured for non-Postgres driver: the clustering control plane \
         has no SQLite equivalent (ADR-0017 Phase 3)"
    )]
    ControlPlanePoolRequiresPostgres,

    /// [`Database::control_plane_guard`] was called on a `Database` opened
    /// without a control-plane pool (`DatabaseConfig::control_plane_pool` was
    /// `None`).
    #[error(
        "control-plane pool not configured for this database (set \
         DatabaseConfig::control_plane_pool)"
    )]
    ControlPlanePoolUnavailable,

    /// `DatabaseConfig::pool_size` or `ControlPlanePoolConfig::size` was 0.
    /// A zero-sized pool can never serve a connection: left unchecked, the
    /// first query against it would instead hang for the pool's acquire
    /// timeout before failing — this rejects the config at construction
    /// time instead.
    #[error("{which} pool size must be at least 1 (got 0)")]
    PoolSizeZero { which: &'static str },

    #[error("combined main and ingress pool size exceeds the supported range")]
    PoolSizeOverflow,

    #[error(
        "attested PostgreSQL pool has capacity {actual}, but its lane budgets require {expected}"
    )]
    AttestedPoolLaneCapacityMismatch { expected: u32, actual: u32 },
}

/// Default main-pool connection cap, preserving the behavior that was
/// hardcoded at both adapters' `.max_connections(10)` before this field
/// existed (ADR-0017 element 12).
pub const DEFAULT_POOL_SIZE: u32 = 10;

/// Sizing for the ADR-0017 Phase 3 control-plane pool (element 4/12): a
/// second, independently-sized pool so node/claim liveness statements
/// (keypair-slot lease heartbeat, and — from Slice 1 — the claims CAS) never
/// queue behind fenced bulk writes, backstop fencing SELECTs, claims-read
/// storms, or janitor batches on the main pool. Constructed via the same
/// [`DatabaseAdapter`]/`connect_backend` machinery as the main pool — no
/// separate connection-management type.
///
/// Postgres-only: the clustering control plane has no SQLite equivalent
/// (clustering itself requires Postgres), so `DatabaseConfig::control_plane_pool`
/// must be `None` whenever `driver` is [`DatabaseDriver::Sqlite`] —
/// [`Database::from_config`] rejects the combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlPlanePoolConfig {
    pub size: u32,
}

/// Default control-plane pool size (small — this pool only ever carries
/// single-statement CAS/heartbeat traffic, never bulk writes).
pub const DEFAULT_CONTROL_PLANE_POOL_SIZE: u32 = 4;

/// Configuration for database connections.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub driver: DatabaseDriver,
    pub database_url: String,
    /// Main/shared pool connection cap (ADR-0017 element 12). Default
    /// [`DEFAULT_POOL_SIZE`], preserving today's hardcoded behavior.
    pub pool_size: u32,
    /// Dedicated control-plane pool, if this `Database` should provision
    /// one. `None` (the default) means no control-plane pool is opened —
    /// only the production global database sets this, and only when the
    /// driver is Postgres AND clustering is enabled (see `main.rs`); no code
    /// path issues control-plane statements otherwise.
    pub control_plane_pool: Option<ControlPlanePoolConfig>,
    /// Main-pool acquire timeout. Default [`MAIN_POOL_ACQUIRE_TIMEOUT`]
    /// (sqlx's 30 s); production sets it from `WADDLE_DB_ACQUIRE_TIMEOUT_SECS`.
    /// The control-plane pool keeps its own fixed bound.
    pub acquire_timeout: Duration,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            driver: DatabaseDriver::Sqlite,
            database_url: "sqlite::memory:".to_string(),
            pool_size: DEFAULT_POOL_SIZE,
            control_plane_pool: None,
            acquire_timeout: MAIN_POOL_ACQUIRE_TIMEOUT,
        }
    }
}

impl DatabaseConfig {
    pub fn new(driver: DatabaseDriver, database_url: impl Into<String>) -> Self {
        Self {
            driver,
            database_url: database_url.into(),
            pool_size: DEFAULT_POOL_SIZE,
            control_plane_pool: None,
            acquire_timeout: MAIN_POOL_ACQUIRE_TIMEOUT,
        }
    }

    /// Provision a dedicated control-plane pool alongside the main pool.
    /// Postgres-only — see [`ControlPlanePoolConfig`]; [`Database::from_config`]
    /// rejects this combined with a non-Postgres `driver`.
    pub fn with_control_plane_pool(mut self, size: u32) -> Self {
        self.control_plane_pool = Some(ControlPlanePoolConfig { size });
        self
    }
}

/// Storage lifetime of a SQLite pool supplied by another store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqlitePoolStorage {
    /// Data exists only for the lifetime of the in-memory database.
    InMemory,
    /// Data is backed by a durable SQLite database.
    Persistent,
}

/// Logical database handle.
#[derive(Clone, kameo::Reply)]
pub struct Database {
    pub(in crate::db) backend: DatabaseBackend,
    /// Admission for this logical lane over `backend`.
    pub(in crate::db) admission: PoolAdmission,
    /// Optional reserved ingress lane over the same physical pool. Only the
    /// main handle stores this; `ingress_lane` returns a clone whose active
    /// admission is this separate bounded semaphore.
    pub(in crate::db) ingress_admission: Option<PoolAdmission>,
    /// The dedicated control-plane pool (ADR-0017 element 4/12), present
    /// only when [`DatabaseConfig::control_plane_pool`] was set. Hosts only
    /// node/claim liveness statements — see [`Database::control_plane_guard`].
    pub(in crate::db) control_plane_backend: Option<DatabaseBackend>,
    pub(in crate::db) control_plane_admission: Option<PoolAdmission>,
    pub(in crate::db) name: String,
    pub(in crate::db) driver: DatabaseDriver,
    /// The DSN this handle was opened against (ADR-0017 Phase 3 Slice 4
    /// FIX 4): lets a caller compare "does this other resolved URL point
    /// at the same database as this handle" — e.g.
    /// `sm_persistence::open_for_cluster_mode`'s co-location check, which
    /// must refuse to start a Postgres-fenced `SmPersistenceStorage`
    /// against a database URL different from the clustering global
    /// database's own. Deliberately **not** exposed via `Debug` (this
    /// struct has none, on purpose — see the control-plane-pool test
    /// below) and [`Database::database_url`] is the only accessor, so a
    /// caller must go out of its way to read it, rather than have it leak
    /// into an incidental log/format call. Callers that log or embed this
    /// value in an error message MUST redact credentials first (DSNs
    /// commonly carry a password in their userinfo component).
    pub(in crate::db) database_url: String,
    /// Present only when the exact hook-bearing pool and its unforgeable
    /// writer capability were consumed together.
    pub(in crate::db) attested_pg_writer: Option<AttestedPostgresWriterCapability>,
}

impl Database {
    /// The SQLite pool behind this handle, so colocated stores (MAM, inbox)
    /// share the ingress database instead of opening a second one.
    pub(crate) fn sqlite_pool(&self) -> Option<&SqlitePool> {
        match &self.backend {
            DatabaseBackend::Sqlite(pool) => Some(pool),
            DatabaseBackend::Postgres(_) => None,
        }
    }

    /// A startup gate owns this physical session and closes it on completion.
    /// Detaching returns the pool slot immediately, so even a one-slot main
    /// pool can execute the schema work while this session holds its gate.
    pub(crate) async fn detached_postgres_startup_connection(
        &self,
    ) -> Result<sqlx::PgConnection, DatabaseError> {
        if self.ingress_admission.is_some() {
            return Err(DatabaseError::QueryFailed(
                "detached startup connections are unavailable after reserved pool lanes are created"
                    .to_owned(),
            ));
        }
        match &self.backend {
            DatabaseBackend::Postgres(pool) => Ok(pool.acquire().await?.detach()),
            DatabaseBackend::Sqlite(_) => Err(DatabaseError::QueryFailed(
                "PostgreSQL startup session requested for SQLite".to_owned(),
            )),
        }
    }

    /// Wrap a physical external SQLx pool so shared database helpers can run
    /// against that exact pool without opening a second connection set.
    pub(crate) fn from_sqlite_pool(
        name: &str,
        pool: SqlitePool,
        storage: SqlitePoolStorage,
    ) -> Self {
        Self {
            admission: PoolAdmission::unbounded(
                pool.options().get_acquire_timeout(),
                PoolLane::External,
            ),
            ingress_admission: None,
            backend: DatabaseBackend::Sqlite(pool),
            control_plane_backend: None,
            control_plane_admission: None,
            name: name.to_string(),
            driver: DatabaseDriver::Sqlite,
            database_url: if storage == SqlitePoolStorage::InMemory {
                "sqlite::memory:".to_string()
            } else {
                "sqlite:external-pool".to_string()
            },
            attested_pg_writer: None,
        }
    }

    /// Wrap a physical external PostgreSQL pool without retaining its DSN.
    pub(crate) fn from_postgres_pool(name: &str, pool: PgPool) -> Self {
        Self {
            admission: PoolAdmission::unbounded(
                pool.options().get_acquire_timeout(),
                PoolLane::External,
            ),
            ingress_admission: None,
            backend: DatabaseBackend::Postgres(pool),
            control_plane_backend: None,
            control_plane_admission: None,
            name: name.to_string(),
            driver: DatabaseDriver::Postgres,
            database_url: "postgres:external-pool".to_string(),
            attested_pg_writer: None,
        }
    }

    /// Wrap the exact physical-attestation pool and retain its dark writer
    /// capability. This constructor accepts the private pool/capability
    /// wrapper as one value, so it cannot bind a token to another pool.
    #[cfg(feature = "test-support")]
    pub(crate) fn from_attested_postgres_pool(name: &str, pool: AttestedPgPool) -> Self {
        let main_budget = pool.max_connections();
        Self::from_attested_postgres_pool_with_budgets(name, pool, main_budget, None)
            .expect("the attested pool's default main budget matches its own capacity")
    }

    pub(crate) async fn from_attested_postgres_pool_with_ingress(
        name: &str,
        pool: AttestedPgPool,
        main_budget: u32,
        ingress_budget: u32,
    ) -> Result<Self, DatabaseError> {
        Self::from_attested_postgres_pool_with_budgets(
            name,
            pool,
            main_budget,
            Some(ingress_budget),
        )
    }

    fn from_attested_postgres_pool_with_budgets(
        name: &str,
        pool: AttestedPgPool,
        main_budget: u32,
        ingress_budget: Option<u32>,
    ) -> Result<Self, DatabaseError> {
        if main_budget == 0 {
            return Err(DatabaseError::PoolSizeZero { which: "main" });
        }
        if ingress_budget == Some(0) {
            return Err(DatabaseError::PoolSizeZero { which: "ingress" });
        }
        let expected = match ingress_budget {
            Some(ingress) => main_budget
                .checked_add(ingress)
                .ok_or(DatabaseError::PoolSizeOverflow)?,
            None => main_budget,
        };
        let (pool, attested_pg_writer, database_url) = pool.into_parts()?;
        let actual = pool.options().get_max_connections();
        if actual != expected {
            return Err(DatabaseError::AttestedPoolLaneCapacityMismatch { expected, actual });
        }
        let acquire_timeout = pool.options().get_acquire_timeout();
        Ok(Self {
            admission: PoolAdmission::new(main_budget, acquire_timeout, PoolLane::Main),
            ingress_admission: ingress_budget
                .map(|size| PoolAdmission::new(size, acquire_timeout, PoolLane::Ingress)),
            backend: DatabaseBackend::Postgres(pool),
            control_plane_backend: None,
            control_plane_admission: None,
            name: name.to_string(),
            driver: DatabaseDriver::Postgres,
            database_url,
            attested_pg_writer: Some(attested_pg_writer),
        })
    }

    pub(crate) fn attested_pg_writer_capability(&self) -> Option<AttestedPostgresWriterCapability> {
        self.attested_pg_writer.clone()
    }

    pub(crate) fn verify_attested_pg_writer_capability(
        &self,
        capability: &AttestedPostgresWriterCapability,
    ) -> Result<(), PhysicalAttestationError> {
        match &self.attested_pg_writer {
            Some(owned) if owned.belongs_to(capability) => Ok(()),
            _ => Err(PhysicalAttestationError::WrongPool),
        }
    }

    pub async fn in_memory(name: &str) -> Result<Self, DatabaseError> {
        Self::from_config(name, &DatabaseConfig::default()).await
    }

    #[cfg(test)]
    #[instrument(skip_all, fields(path = %path.as_ref().display()))]
    pub async fn open_local(name: &str, path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DatabaseError::ConnectionFailed(format!(
                    "Failed to create database directory: {}",
                    e
                ))
            })?;
        }
        let database_url = format!("sqlite://{}", path.to_string_lossy());
        Self::from_config(
            name,
            &DatabaseConfig::new(DatabaseDriver::Sqlite, database_url),
        )
        .await
    }

    #[instrument(skip_all, fields(name = %name), err)]
    pub async fn from_config(name: &str, config: &DatabaseConfig) -> Result<Self, DatabaseError> {
        if config.control_plane_pool.is_some() && config.driver != DatabaseDriver::Postgres {
            return Err(DatabaseError::ControlPlanePoolRequiresPostgres);
        }
        if config.pool_size == 0 {
            return Err(DatabaseError::PoolSizeZero { which: "main" });
        }
        if let Some(ControlPlanePoolConfig { size }) = config.control_plane_pool {
            if size == 0 {
                return Err(DatabaseError::PoolSizeZero {
                    which: "control-plane",
                });
            }
        }
        debug!(driver = ?config.driver, "Opening database");
        let backend = connect_backend(
            config.driver,
            &config.database_url,
            config.pool_size,
            config.acquire_timeout,
        )
        .await?;

        let control_plane_backend = match config.control_plane_pool {
            Some(ControlPlanePoolConfig { size }) => Some(
                connect_backend(
                    config.driver,
                    &config.database_url,
                    size,
                    CONTROL_PLANE_POOL_ACQUIRE_TIMEOUT,
                )
                .await?,
            ),
            None => None,
        };
        let control_plane_admission = config.control_plane_pool.map(|pool| {
            PoolAdmission::new(
                pool.size,
                CONTROL_PLANE_POOL_ACQUIRE_TIMEOUT,
                PoolLane::ControlPlane,
            )
        });

        info!(
            name = %name,
            driver = ?config.driver,
            pool_size = config.pool_size,
            control_plane_pool_size = config.control_plane_pool.map(|p| p.size),
            "Opened database"
        );

        Ok(Self {
            backend,
            admission: PoolAdmission::unbounded(config.acquire_timeout, PoolLane::Main),
            ingress_admission: None,
            control_plane_backend,
            control_plane_admission,
            name: name.to_string(),
            driver: config.driver,
            database_url: config.database_url.clone(),
            attested_pg_writer: None,
        })
    }

    /// Clone this logical database onto its reserved ingress admission lane.
    /// Both handles retain the same private physical backend and attestation
    /// binding, while admission permits remain disjoint.
    pub(crate) fn ingress_lane(&self) -> Self {
        let Some(admission) = &self.ingress_admission else {
            return self.clone();
        };
        let mut ingress = self.clone();
        ingress.admission = admission.clone();
        ingress
    }

    pub(crate) fn is_ingress_lane(&self) -> bool {
        self.admission.lane() == PoolLane::Ingress
    }

    pub(crate) fn ingress_pool_size(&self) -> Option<u32> {
        self.ingress_admission.as_ref()?.limit()
    }

    pub async fn guard(&self) -> Result<ConnectionGuard, DatabaseError> {
        Ok(ConnectionGuard::new(
            self.backend.clone(),
            self.admission.clone(),
        ))
    }

    /// A `ConnectionGuard` against the dedicated control-plane pool (ADR-0017
    /// element 4/12), for node/claim liveness statements only — the keypair
    /// -slot lease heartbeat, and (from Slice 1) the claims CAS. Never for
    /// fenced write transactions or their fencing `SELECT ... FOR SHARE`,
    /// which must share the *same* connection as the write they guard and so
    /// always run on the main pool via [`Database::begin`].
    ///
    /// Errs with [`DatabaseError::ControlPlanePoolUnavailable`] if this
    /// `Database` was opened without a control-plane pool.
    pub async fn control_plane_guard(&self) -> Result<ConnectionGuard, DatabaseError> {
        match &self.control_plane_backend {
            Some(backend) => Ok(ConnectionGuard::new(
                backend.clone(),
                self.control_plane_admission
                    .clone()
                    .expect("control plane backend and admission are constructed together"),
            )),
            None => Err(DatabaseError::ControlPlanePoolUnavailable),
        }
    }

    /// Begin a database transaction.
    ///
    /// Multiple statements executed against the returned [`Transaction`]
    /// share a single connection and are committed atomically by
    /// [`Transaction::commit`] (or rolled back on drop). Use this when you
    /// need an invariant — like XEP-0237's "ver identifies the roster
    /// state" — to hold across multiple writes that must commit together
    /// or not at all.
    pub async fn begin(&self) -> Result<Transaction<'_>, DatabaseError> {
        let acquisition_deadline = self.admission.acquisition_deadline();
        let permit = self.admission.acquire_until(acquisition_deadline).await?;
        Transaction::begin(&self.backend, permit, acquisition_deadline).await
    }

    /// Begin a transaction that acquires the database write lock
    /// immediately. Use this for transactions that perform writes after
    /// a read, to avoid SQLite deadlocks when two connections both
    /// start as readers and then try to upgrade to writers.
    pub async fn begin_immediate(&self) -> Result<Transaction<'_>, DatabaseError> {
        let acquisition_deadline = self.admission.acquisition_deadline();
        let permit = self.admission.acquire_until(acquisition_deadline).await?;
        Transaction::begin_immediate(&self.backend, permit, acquisition_deadline).await
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn driver(&self) -> DatabaseDriver {
        self.driver
    }

    /// Whether this handle is backed by SQLite's process-local in-memory
    /// database. This is deliberately derived from the opened handle rather
    /// than configuration-source presence: an XMPP-specific URL may be unset
    /// while still resolving to a durable `WADDLE_DATABASE_URL` database.
    pub fn is_in_memory_sqlite(&self) -> bool {
        self.driver == DatabaseDriver::Sqlite && sqlite_url_is_in_memory(&self.database_url)
    }

    pub fn has_control_plane_pool(&self) -> bool {
        self.control_plane_backend.is_some()
    }

    /// The DSN this handle was opened against (ADR-0017 Phase 3 Slice 4
    /// FIX 4). See the field's own doc comment for the redaction caveat —
    /// this MAY carry credentials in its userinfo component; callers that
    /// log or embed it in a user-facing error MUST redact first.
    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    /// Export the alertable DB health-check failure counter. The probe
    /// spans that used to carry this signal are suppressed as trace
    /// noise (#1438). Shared with the pool so its ask-failure branch
    /// (actor unreachable — [`Database::health_check`] never runs)
    /// counts through the same instrument.
    pub(crate) fn record_health_check_failure() {
        waddle_xmpp::counter_add!(
            "waddle.db.health_check.failed",
            "1",
            "Database health-check failures (failed probe query, unavailable \
             connection, or unreachable DB actor), across the readiness and \
             detailed-health paths.",
            1,
        );
    }

    #[instrument(skip_all, fields(name = %self.name), err)]
    pub async fn health_check(&self) -> Result<bool, DatabaseError> {
        // The deepest common point of every database health probe — the
        // pool's actor ask (readiness, detailed health) lands here;
        // liveness (`/health`) never probes the database. The probe-driven
        // `health_check` spans are suppressed as trace noise (#1438), so
        // the counter is the alertable failure signal for all of them.
        let conn = match self.guard().await {
            Ok(conn) => conn,
            Err(e) => {
                crate::telemetry::mark_span_error("database health check guard failed");
                Self::record_health_check_failure();
                return Err(e);
            }
        };
        match conn.query("SELECT 1", ()).await {
            Ok(_) => Ok(true),
            Err(e) => {
                crate::telemetry::mark_span_error("database health check failed");
                tracing::warn!(error = %e, "Database health check failed");
                Self::record_health_check_failure();
                Ok(false)
            }
        }
    }

    #[cfg(test)]
    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn execute(&self, sql: &str) -> Result<u64, DatabaseError> {
        let conn = self.guard().await?;
        conn.execute(sql, ()).await
    }
}

pub(crate) fn sqlite_url_is_in_memory(database_url: &str) -> bool {
    let trimmed = database_url.trim().to_ascii_lowercase();
    let base = trimmed
        .split_once('?')
        .map_or(trimmed.as_str(), |(base, _)| base);
    if matches!(
        base,
        ":memory:" | "sqlite::memory:" | "sqlite://{memory}:" | "sqlite:///{memory}:"
    ) || base == "file::memory:"
        || base.ends_with("://file::memory:")
        || base.ends_with(":file::memory:")
        || super::sqlite_url_query_requests_memory(&trimmed)
    {
        return true;
    }
    SqliteConnectOptions::from_str(database_url)
        .map(|options| {
            let filename = options
                .get_filename()
                .to_string_lossy()
                .to_ascii_lowercase();
            filename == ":memory:"
                || filename == "file::memory:"
                // sqlx names shared in-memory databases exactly
                // `sqlx-in-memory-<digits>` with no directory component; a
                // durable file that merely resembles the marker
                // (`sqlx-in-memory-prod.db`, `/var/.../sqlx-in-memory-x`)
                // must stay durable.
                || (!filename.contains('/')
                    && filename
                        .strip_prefix("sqlx-in-memory-")
                        .is_some_and(|rest| {
                            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
                        }))
        })
        .unwrap_or(false)
}

pub(in crate::db) fn sqlite_url_query_requests_memory(database_url: &str) -> bool {
    let Some((_, query)) = database_url.split_once('?') else {
        return false;
    };
    query.split('&').any(|param| {
        param
            .split_once('=')
            .is_some_and(|(key, value)| key == "mode" && value == "memory")
    })
}
