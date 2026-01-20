use anyhow::Result;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

mod server;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber for logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Waddle Server starting...");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("License: AGPL-3.0");

    // Start the HTTP server
    server::start().await?;

    Ok(())
}
