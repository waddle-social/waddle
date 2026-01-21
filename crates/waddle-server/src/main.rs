use anyhow::Result;
use tracing::info;

mod auth;
mod db;
mod messages;
mod permissions;
mod server;
mod telemetry;

#[tokio::main]
async fn main() -> Result<()> {
    // Install the ring crypto provider for rustls (required for XMPP TLS)
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Initialize telemetry (OpenTelemetry + tracing)
    // Use OTEL_EXPORTER_OTLP_ENDPOINT env var to configure OTLP endpoint
    // Falls back to local-only logging if OTLP is not available
    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        telemetry::init().map_err(|e| anyhow::anyhow!("Failed to init telemetry: {}", e))?;
    } else {
        telemetry::init_local().map_err(|e| anyhow::anyhow!("Failed to init local telemetry: {}", e))?;
    }

    info!("Waddle Server starting...");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("License: AGPL-3.0");

    // Initialize database pool
    let db_config = if let Ok(db_path) = std::env::var("WADDLE_DB_PATH") {
        info!("Using file-based database at: {}", db_path);
        db::DatabaseConfig::development(&db_path)
    } else {
        info!("Using in-memory database (development mode)");
        db::DatabaseConfig::default()
    };

    let pool_config = db::PoolConfig::default();
    let db_pool = db::DatabasePool::new(db_config, pool_config)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to initialize database: {}", e))?;

    // Run global database migrations
    let migration_runner = db::MigrationRunner::global();
    migration_runner
        .run(db_pool.global())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?;

    info!("Database initialized and migrations complete");

    // Start the HTTP server
    server::start(db_pool).await?;

    // Shutdown telemetry on exit
    telemetry::shutdown();

    Ok(())
}
