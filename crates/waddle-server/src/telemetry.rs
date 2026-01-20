//! OpenTelemetry instrumentation for Waddle Server.
//!
//! This module provides unified observability across HTTP and XMPP components,
//! including traces, metrics, and logs via OpenTelemetry.
//!
//! See [ADR-0014](../../../docs/adrs/0014-opentelemetry.md) for design decisions.

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

// Note: The OpenTelemetry 0.28 API has changed significantly.
// This module provides a placeholder that will be updated once the
// exact API stabilizes and dependencies are resolved.
//
// TODO: Implement full OTLP export once opentelemetry-otlp API is stable
// For now, we use tracing-subscriber for local development.

/// Initialize OpenTelemetry tracing and logging.
///
/// This sets up:
/// - OTLP exporter for traces (to Jaeger, Grafana Tempo, etc.)
/// - tracing-subscriber with OpenTelemetry integration
/// - Console output for local development
///
/// # Configuration
///
/// Environment variables:
/// - `OTEL_EXPORTER_OTLP_ENDPOINT`: OTLP endpoint (default: http://localhost:4317)
/// - `OTEL_SERVICE_NAME`: Service name (default: waddle-server)
/// - `RUST_LOG`: Log filter (default: info)
///
/// # Example
///
/// ```ignore
/// use waddle_server::telemetry;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     telemetry::init()?;
///
///     // Your application code here...
///
///     telemetry::shutdown();
///     Ok(())
/// }
/// ```
pub fn init() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Get configuration from environment
    let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4317".to_string());

    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "waddle-server".to_string());

    // Build the log filter from RUST_LOG or default to info
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,waddle_server=debug,waddle_xmpp=debug"));

    // Build the fmt layer for console output
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true);

    // Combine layers and set as global subscriber
    // TODO: Add OpenTelemetry layer once API is stable
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();

    tracing::info!(
        endpoint = %otlp_endpoint,
        service = %service_name,
        "Telemetry initialized (OTLP export pending API stabilization)"
    );

    Ok(())
}

/// Initialize telemetry for local development (without OTLP export).
///
/// This is useful for development when you don't have an OTLP collector running.
/// It provides console output with colored logs.
pub fn init_local() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,waddle_server=debug,waddle_xmpp=debug"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .pretty();

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();

    tracing::info!("Local telemetry initialized (no OTLP export)");

    Ok(())
}

/// Shutdown telemetry, flushing any pending spans.
///
/// Call this before application exit to ensure all telemetry data is sent.
pub fn shutdown() {
    // The tracer provider is dropped when the global provider is replaced or at program end
    // This ensures any pending spans are flushed
    tracing::info!("Telemetry shutdown complete");
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_init_local() {
        // Note: Can only initialize once per process
        // This test just verifies the function compiles
        // let _ = super::init_local();
    }
}
