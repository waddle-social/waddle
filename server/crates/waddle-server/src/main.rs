use anyhow::{Context, Result};
use tracing::info;
use waddle_server::{config, config::ServerConfig, db, server, telemetry};

#[tokio::main]
async fn main() -> Result<()> {
    // Install the ring crypto provider for rustls (required for XMPP TLS)
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Initialize telemetry
    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        telemetry::init().map_err(|e| anyhow::anyhow!("Failed to init telemetry: {}", e))?;
    } else {
        telemetry::init_local()
            .map_err(|e| anyhow::anyhow!("Failed to init local telemetry: {}", e))?;
    }

    info!("Waddle Server starting...");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("License: AGPL-3.0");

    // Check for inherited listeners (Ecdysis restart from parent process)
    let inherited = waddle_ecdysis::ListenerSet::from_env();
    if inherited.is_some() {
        info!("Inherited listeners from parent process (Ecdysis graceful restart)");
    }

    // Load configuration
    let server_config = ServerConfig::from_env()
        .map_err(|e| anyhow::anyhow!("Failed to load server configuration: {}", e))?;
    server_config.log_config();

    // Initialize database (driver + DSN contract)
    let db_runtime = config::DatabaseRuntimeConfig::from_env()
        .map_err(|e| anyhow::anyhow!("Failed to load database runtime config: {}", e))?;
    info!(
        driver = ?db_runtime.driver,
        "Using configured database runtime"
    );
    // The dedicated control-plane pool (ADR-0017 element 4/12) hosts only
    // node/claim liveness statements (the keypair-slot lease heartbeat, and —
    // from Phase 3 Slice 1 — the claims CAS), which no code path issues
    // unless clustering is actually running. Provisioning it on every
    // Postgres deployment regardless of `clustering.enabled` would open and
    // connect-validate a second pool nothing uses, holding idle connections
    // against the database for a feature never requested, and a transient
    // failure on that second connect would fail startup for that same unused
    // feature. So it is gated on BOTH the configured driver being Postgres
    // AND clustering being enabled for this run.
    let mut db_config = db::DatabaseConfig::new(db_runtime.driver, db_runtime.database_url);
    db_config.pool_size = db_runtime.pool_size;
    if db_runtime.driver == db::DatabaseDriver::Postgres && server_config.clustering.enabled {
        db_config = db_config.with_control_plane_pool(db_runtime.control_plane_pool_size);
    }

    let pool_config = db::PoolConfig;
    let db_pool = db::DatabasePool::new(db_config, pool_config)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to initialize database: {}", e))?;

    let migration_runner = db::MigrationRunner::single();
    migration_runner
        .run(db_pool.global())
        .await
        .context("Failed to run migrations")?;

    info!("Database initialized and migrations complete");

    // Start the server
    let metrics_flush = server::start(db_pool, server_config, inherited).await?;

    telemetry::shutdown(metrics_flush);

    Ok(())
}
