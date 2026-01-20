use anyhow::Result;
use tracing::info;

mod server;
mod telemetry;

#[tokio::main]
async fn main() -> Result<()> {
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

    // Start the HTTP server
    server::start().await?;

    // Shutdown telemetry on exit
    telemetry::shutdown();

    Ok(())
}
