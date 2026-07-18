//! Database module for Waddle Server.
//!
//! Core infrastructure now uses SQLx adapters and a single logical database.

pub mod actor;
mod backend;
pub mod blocking;
mod migrations;
mod pool;
pub mod roster;
mod schema;
mod value;

#[cfg(test)]
use std::path::Path;
use thiserror::Error;
use tracing::{debug, info, instrument};

use backend::{connect_backend, DatabaseBackend};
pub use backend::{ConnectionGuard, DatabaseDriver, Transaction};
pub use migrations::MigrationRunner;
pub use pool::{DatabasePool, PoolConfig, PoolHealth};
pub use schema::{i64_sql_type, widen_postgres_i64_column_to_bigint};
pub use value::{row_value, DbDecode, IntoParams, Row, Rows, Value, ValueExt};

#[macro_export]
macro_rules! db_params {
    () => {
        Vec::<$crate::db::Value>::new()
    };
    ($($value:expr),+ $(,)?) => {
        vec![$($crate::db::Value::from($value)),+]
    };
}

/// Database-specific errors.
#[derive(Error, Debug)]
pub enum DatabaseError {
    #[error("Failed to connect to database: {0}")]
    ConnectionFailed(String),

    #[error("Database query failed: {0}")]
    QueryFailed(String),

    #[error("Migration failed: {0}")]
    MigrationFailed(String),

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
    /// first query against it would instead hang for sqlx's ~30s
    /// `acquire_timeout` before failing — this rejects the config at
    /// construction time instead.
    #[error("{which} pool size must be at least 1 (got 0)")]
    PoolSizeZero { which: &'static str },
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
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            driver: DatabaseDriver::Sqlite,
            database_url: "sqlite::memory:".to_string(),
            pool_size: DEFAULT_POOL_SIZE,
            control_plane_pool: None,
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

/// Logical database handle.
#[derive(Clone, kameo::Reply)]
pub struct Database {
    backend: DatabaseBackend,
    /// The dedicated control-plane pool (ADR-0017 element 4/12), present
    /// only when [`DatabaseConfig::control_plane_pool`] was set. Hosts only
    /// node/claim liveness statements — see [`Database::control_plane_guard`].
    control_plane_backend: Option<DatabaseBackend>,
    name: String,
    driver: DatabaseDriver,
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
    database_url: String,
}

impl Database {
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

    #[instrument(skip_all, fields(name = %name))]
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
        let backend =
            connect_backend(config.driver, &config.database_url, config.pool_size).await?;

        let control_plane_backend = match config.control_plane_pool {
            Some(ControlPlanePoolConfig { size }) => {
                Some(connect_backend(config.driver, &config.database_url, size).await?)
            }
            None => None,
        };

        info!(
            name = %name,
            driver = ?config.driver,
            pool_size = config.pool_size,
            control_plane_pool_size = config.control_plane_pool.map(|p| p.size),
            "Opened database"
        );

        Ok(Self {
            backend,
            control_plane_backend,
            name: name.to_string(),
            driver: config.driver,
            database_url: config.database_url.clone(),
        })
    }

    pub async fn guard(&self) -> Result<ConnectionGuard, DatabaseError> {
        Ok(ConnectionGuard::new(self.backend.clone()))
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
            Some(backend) => Ok(ConnectionGuard::new(backend.clone())),
            None => Err(DatabaseError::ControlPlanePoolUnavailable),
        }
    }

    /// Begin a transaction on the dedicated control-plane pool.
    ///
    /// Use this only when claim/node liveness work needs more than one SQL
    /// statement to share a connection and transaction. Fenced application
    /// writes still belong on the main pool via [`Database::begin`].
    pub async fn begin_control_plane(&self) -> Result<Transaction<'_>, DatabaseError> {
        match &self.control_plane_backend {
            Some(backend) => Transaction::begin(backend).await,
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
        Transaction::begin(&self.backend).await
    }

    /// Begin a transaction that acquires the database write lock
    /// immediately. Use this for transactions that perform writes after
    /// a read, to avoid SQLite deadlocks when two connections both
    /// start as readers and then try to upgrade to writers.
    pub async fn begin_immediate(&self) -> Result<Transaction<'_>, DatabaseError> {
        Transaction::begin_immediate(&self.backend).await
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn driver(&self) -> DatabaseDriver {
        self.driver
    }

    /// The DSN this handle was opened against (ADR-0017 Phase 3 Slice 4
    /// FIX 4). See the field's own doc comment for the redaction caveat —
    /// this MAY carry credentials in its userinfo component; callers that
    /// log or embed it in a user-facing error MUST redact first.
    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn health_check(&self) -> Result<bool, DatabaseError> {
        let conn = self.guard().await?;
        match conn.query("SELECT 1", ()).await {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::warn!(error = %e, "Database health check failed");
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

/// Strip a DSN's userinfo (`user[:password]@`) component, if any, for safe
/// inclusion in a log line or user-facing error message (ADR-0017 Phase 3
/// Slice 4 FIX 4). `postgres://user:pass@host/db` becomes
/// `postgres://***@host/db`; a URL with no userinfo, or one that fails to
/// parse as `scheme://...`, is returned unchanged.
///
/// A small string-scan rather than pulling in a URL-parsing crate: the only
/// shape this needs to recognize is the `scheme://[userinfo@]rest` prefix
/// every DSN this codebase handles (`postgres://`, `postgresql://`,
/// `sqlite://`) already uses.
///
/// `#[cfg(feature = "clustering")]`: this repo's dead-code hard rule prefers
/// narrowing visibility over suppressing an unused-code warning — the sole
/// call site is `sm_persistence::open_for_cluster_mode`'s FIX 4 co-location
/// check, itself only reachable in a `clustering`-feature build.
#[cfg(feature = "clustering")]
pub(crate) fn redact_database_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let (scheme, rest) = url.split_at(scheme_end + 3);
    let Some(at_pos) = rest.find('@') else {
        return url.to_string();
    };
    // Only redact if everything before the `@` is plausibly userinfo (no
    // `/` in it) — a bare `@` appearing later in a path/query segment must
    // not be mistaken for credentials.
    let userinfo_candidate = &rest[..at_pos];
    if userinfo_candidate.contains('/') {
        return url.to_string();
    }
    format!("{scheme}***@{}", &rest[at_pos + 1..])
}

#[cfg(all(test, feature = "clustering"))]
mod redact_database_url_tests {
    use super::*;

    #[test]
    fn redact_database_url_strips_credentials() {
        assert_eq!(
            redact_database_url("postgres://user:hunter2@localhost:5432/waddle"),
            "postgres://***@localhost:5432/waddle"
        );
        assert_eq!(
            redact_database_url("postgres://user@localhost/waddle"),
            "postgres://***@localhost/waddle"
        );
    }

    #[test]
    fn redact_database_url_is_a_no_op_without_userinfo() {
        assert_eq!(
            redact_database_url("postgres://localhost:5432/waddle"),
            "postgres://localhost:5432/waddle"
        );
        assert_eq!(redact_database_url("sqlite::memory:"), "sqlite::memory:");
        assert_eq!(redact_database_url("not-a-url-at-all"), "not-a-url-at-all");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_database() {
        let db = Database::in_memory("test").await.unwrap();
        assert_eq!(db.name(), "test");
    }

    #[test]
    fn database_config_defaults_preserve_historical_pool_size() {
        // Byte-for-byte-identical guarantee: before `pool_size` existed both
        // adapters hardcoded `.max_connections(10)` — the default must not
        // silently change that for existing deployments.
        let config = DatabaseConfig::default();
        assert_eq!(config.pool_size, DEFAULT_POOL_SIZE);
        assert_eq!(DEFAULT_POOL_SIZE, 10);
        assert!(config.control_plane_pool.is_none());
    }

    #[test]
    fn database_config_new_defaults_have_no_control_plane_pool() {
        let config = DatabaseConfig::new(DatabaseDriver::Postgres, "postgres://example");
        assert_eq!(config.pool_size, DEFAULT_POOL_SIZE);
        assert!(config.control_plane_pool.is_none());
    }

    #[test]
    fn with_control_plane_pool_sets_the_configured_size() {
        let config = DatabaseConfig::new(DatabaseDriver::Postgres, "postgres://example")
            .with_control_plane_pool(4);
        assert_eq!(
            config.control_plane_pool,
            Some(ControlPlanePoolConfig { size: 4 })
        );
    }

    #[tokio::test]
    async fn control_plane_pool_requires_postgres() {
        // `Database` intentionally does not implement `Debug` (it would leak
        // a live SQLx pool handle into `{:?}` output), so this asserts via
        // `match` rather than `expect_err`/`unwrap_err` (both require `T:
        // Debug` on the `Ok` type).
        let config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:")
            .with_control_plane_pool(4);
        match Database::from_config("bad-control-plane", &config).await {
            Err(DatabaseError::ControlPlanePoolRequiresPostgres) => {}
            Err(other) => panic!("expected ControlPlanePoolRequiresPostgres, got {other}"),
            Ok(_) => panic!("SQLite + control-plane pool must be rejected"),
        }
    }

    #[tokio::test]
    async fn from_config_rejects_zero_main_pool_size() {
        let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
        config.pool_size = 0;
        match Database::from_config("zero-main-pool", &config).await {
            Err(DatabaseError::PoolSizeZero { which: "main" }) => {}
            Err(other) => panic!("expected PoolSizeZero(main), got {other}"),
            Ok(_) => panic!("zero main pool size must be rejected"),
        }
    }

    #[tokio::test]
    async fn from_config_rejects_zero_control_plane_pool_size() {
        let config = DatabaseConfig::new(DatabaseDriver::Postgres, "postgres://example")
            .with_control_plane_pool(0);
        match Database::from_config("zero-control-plane-pool", &config).await {
            Err(DatabaseError::PoolSizeZero {
                which: "control-plane",
            }) => {}
            Err(other) => panic!("expected PoolSizeZero(control-plane), got {other}"),
            Ok(_) => panic!("zero control-plane pool size must be rejected"),
        }
    }

    #[tokio::test]
    async fn control_plane_guard_errors_when_not_configured() {
        let db = Database::in_memory("test").await.unwrap();
        match db.control_plane_guard().await {
            Err(DatabaseError::ControlPlanePoolUnavailable) => {}
            Err(other) => panic!("expected ControlPlanePoolUnavailable, got {other}"),
            Ok(_) => panic!("no control-plane pool was configured"),
        }
    }

    #[tokio::test]
    async fn begin_control_plane_errors_when_not_configured() {
        let db = Database::in_memory("test").await.unwrap();
        match db.begin_control_plane().await {
            Err(DatabaseError::ControlPlanePoolUnavailable) => {}
            Err(other) => panic!("expected ControlPlanePoolUnavailable, got {other}"),
            Ok(_) => panic!("no control-plane pool was configured"),
        };
    }

    // Postgres-gated: the control-plane CAS has no SQLite equivalent, so this
    // is skipped unless `WADDLE_TEST_POSTGRES_URL` points at a Postgres —
    // mirroring `clustering::lease`'s test convention.
    #[tokio::test]
    async fn control_plane_pool_is_distinct_from_main_pool() {
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            return;
        };
        let config = DatabaseConfig::new(DatabaseDriver::Postgres, url)
            .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE);
        let db = Database::from_config("control-plane-pool-test", &config)
            .await
            .expect("open test postgres");

        let main_max = match &db.backend {
            DatabaseBackend::Postgres(pool) => pool.options().get_max_connections(),
            DatabaseBackend::Sqlite(_) => panic!("expected a Postgres main backend"),
        };
        let control_max = match db
            .control_plane_backend
            .as_ref()
            .expect("control-plane pool was configured")
        {
            DatabaseBackend::Postgres(pool) => pool.options().get_max_connections(),
            DatabaseBackend::Sqlite(_) => panic!("expected a Postgres control-plane backend"),
        };

        assert_eq!(main_max, DEFAULT_POOL_SIZE);
        assert_eq!(control_max, DEFAULT_CONTROL_PLANE_POOL_SIZE);
        assert_ne!(
            main_max, control_max,
            "control-plane pool must be a distinct PgPool from the main pool"
        );
    }

    #[tokio::test]
    async fn test_health_check() {
        let db = Database::in_memory("test").await.unwrap();
        let healthy = db.health_check().await.unwrap();
        assert!(healthy);
    }

    #[tokio::test]
    async fn test_execute_query() {
        let db = Database::in_memory("test").await.unwrap();

        db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .unwrap();
        db.execute("INSERT INTO test (name) VALUES ('hello')")
            .await
            .unwrap();

        let conn = db.guard().await.unwrap();
        let mut rows = conn.query("SELECT * FROM test", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let name: String = row.get(1).unwrap();
        assert_eq!(name, "hello");
    }
}
