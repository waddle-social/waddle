use anyhow::Result;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber for logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Waddle CLI starting...");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("License: AGPL-3.0");

    // TODO: Initialize TUI components
    // - Load configuration from XDG directories
    // - Initialize Ratatui terminal
    // - Connect to XMPP server
    // - Set up event loop
    // - Render UI (sidebar, message view, input)

    info!("CLI initialization complete - ready for TUI implementation");

    Ok(())
}
