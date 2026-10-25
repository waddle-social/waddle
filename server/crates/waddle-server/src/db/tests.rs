use super::{
    ControlPlanePoolConfig, Database, DatabaseBackend, DatabaseConfig, DatabaseDriver,
    DatabaseError, DEFAULT_CONTROL_PLANE_POOL_SIZE, DEFAULT_POOL_SIZE,
};

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
    let config =
        DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:").with_control_plane_pool(4);
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
