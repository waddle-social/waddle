use anyhow::Result;
use tracing::info;

mod auth;
mod config;
mod db;
mod messages;
mod permissions;
mod server;
mod telemetry;
mod vcard;

pub use config::{ServerConfig, ServerMode};

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

    // Initialize database
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

    let migration_runner = db::MigrationRunner::global();
    migration_runner
        .run(db_pool.global())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?;

    info!("Database initialized and migrations complete");

    // Start the server
    server::start(db_pool, server_config, inherited).await?;

    telemetry::shutdown();

    Ok(())
}
