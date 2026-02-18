//! OpenTelemetry instrumentation for Waddle Server.
//!
//! This module provides unified observability across HTTP and XMPP components,
//! including traces, metrics, and logs via OpenTelemetry.
//!
//! See [ADR-0014](../../../docs/adrs/0014-opentelemetry.md) for design decisions.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    metrics::{PeriodicReader, SdkMeterProvider},
    trace::SdkTracerProvider,
    Resource,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// The global tracer provider, stored for shutdown.
static TRACER_PROVIDER: std::sync::OnceLock<SdkTracerProvider> = std::sync::OnceLock::new();

/// The global meter provider, stored for shutdown.
static METER_PROVIDER: std::sync::OnceLock<SdkMeterProvider> = std::sync::OnceLock::new();

/// Build the OpenTelemetry resource with service information.
fn build_resource() -> Resource {
    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "waddle-server".to_string());
    let service_version = std::env::var("OTEL_SERVICE_VERSION")
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());

    Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", service_name),
            KeyValue::new("service.version", service_version),
        ])
        .build()
}

fn default_filter() -> EnvFilter {
    // Keep historical defaults to avoid changing verbosity unexpectedly.
    EnvFilter::new("info,waddle_server=debug,waddle_xmpp=debug")
}

fn build_log_filter() -> EnvFilter {
    if let Ok(filter) = std::env::var("RUST_LOG") {
        return EnvFilter::try_new(filter).unwrap_or_else(|_| default_filter());
    }

    if let Ok(level_or_filter) = std::env::var("WADDLE_LOG_LEVEL") {
        let level_or_filter = level_or_filter.trim();
        if !level_or_filter.is_empty() {
            let filter = if level_or_filter.contains('=') || level_or_filter.contains(',') {
                level_or_filter.to_string()
            } else {
                format!(
                    "{level},waddle_server={level},waddle_xmpp={level}",
                    level = level_or_filter
                )
            };
            return EnvFilter::try_new(filter).unwrap_or_else(|_| default_filter());
        }
    }

    default_filter()
}

/// Initialize OpenTelemetry tracing with OTLP export.
///
/// This sets up:
/// - OTLP exporter for traces (to Jaeger, Grafana Tempo, etc.)
/// - OTLP exporter for metrics
/// - tracing-subscriber with OpenTelemetry integration
/// - Console output for local development
///
/// # Configuration
///
/// Environment variables:
/// - `OTEL_EXPORTER_OTLP_ENDPOINT`: OTLP endpoint (default: http://localhost:4317)
/// - `OTEL_SERVICE_NAME`: Service name (default: waddle-server)
/// - `OTEL_SERVICE_VERSION`: Service version (default: crate version)
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

    let resource = build_resource();

    // Build OTLP trace exporter
    let trace_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&otlp_endpoint)
        .build()?;

    // Build tracer provider with batch processor
    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(trace_exporter)
        .with_resource(resource.clone())
        .build();

    // Store the provider for shutdown
    let _ = TRACER_PROVIDER.set(tracer_provider.clone());

    // Get a tracer from the provider
    let tracer = tracer_provider.tracer("waddle-server");

    // Build OTLP metrics exporter
    let metrics_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(&otlp_endpoint)
        .build()?;

    // Build meter provider
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(metrics_exporter).build())
        .with_resource(resource)
        .build();

    // Store the meter provider for shutdown
    let _ = METER_PROVIDER.set(meter_provider.clone());

    // Set global meter provider
    opentelemetry::global::set_meter_provider(meter_provider);

    let filter = build_log_filter();

    // Structured JSON logs for production and local observability pipelines.
    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true);

    // Build the OpenTelemetry tracing layer
    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Combine layers and set as global subscriber
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(telemetry_layer)
        .init();

    tracing::info!(
        endpoint = %otlp_endpoint,
        "OpenTelemetry initialized with OTLP export"
    );

    Ok(())
}

/// Initialize telemetry for local development (without OTLP export).
///
/// This is useful for development when you don't have an OTLP collector running.
/// It provides console output with colored logs.
pub fn init_local() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = build_log_filter();

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();

    tracing::info!("Local telemetry initialized with JSON logging (no OTLP export)");

    Ok(())
}

/// Shutdown telemetry, flushing any pending spans and metrics.
///
/// Call this before application exit to ensure all telemetry data is sent.
pub fn shutdown() {
    tracing::info!("Shutting down telemetry...");

    // Shutdown tracer provider
    if let Some(provider) = TRACER_PROVIDER.get() {
        if let Err(e) = provider.shutdown() {
            tracing::error!(error = %e, "Error shutting down tracer provider");
        }
    }

    // Shutdown meter provider
    if let Some(provider) = METER_PROVIDER.get() {
        if let Err(e) = provider.shutdown() {
            tracing::error!(error = %e, "Error shutting down meter provider");
        }
    }

    tracing::info!("Telemetry shutdown complete");
}

// ============================================================================
// Metrics
// ============================================================================

/// XMPP metrics for observability.
///
/// These metrics follow the naming conventions from ADR-0014.
#[allow(dead_code)]
pub mod metrics {
    use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
    use std::sync::OnceLock;

    static METER: OnceLock<Meter> = OnceLock::new();

    fn meter() -> &'static Meter {
        METER.get_or_init(|| opentelemetry::global::meter("waddle-server"))
    }

    /// Counter for XMPP stanzas processed.
    pub fn stanzas_processed() -> Counter<u64> {
        meter()
            .u64_counter("xmpp.stanzas.processed")
            .with_description("Total XMPP stanzas processed")
            .with_unit("stanza")
            .build()
    }

    /// Counter for authentication attempts.
    pub fn auth_attempts() -> Counter<u64> {
        meter()
            .u64_counter("xmpp.auth.attempts")
            .with_description("Total authentication attempts")
            .with_unit("attempt")
            .build()
    }

    /// Counter for MUC messages.
    pub fn muc_messages() -> Counter<u64> {
        meter()
            .u64_counter("xmpp.muc.messages")
            .with_description("Total MUC messages sent")
            .with_unit("message")
            .build()
    }

    /// Gauge for active XMPP connections.
    pub fn connections_active() -> Gauge<i64> {
        meter()
            .i64_gauge("xmpp.connections.active")
            .with_description("Current number of active XMPP connections")
            .with_unit("connection")
            .build()
    }

    /// Gauge for active MUC rooms.
    pub fn muc_rooms_active() -> Gauge<i64> {
        meter()
            .i64_gauge("xmpp.muc.rooms.active")
            .with_description("Current number of active MUC rooms")
            .with_unit("room")
            .build()
    }

    /// Gauge for MUC occupants.
    pub fn muc_occupants() -> Gauge<i64> {
        meter()
            .i64_gauge("xmpp.muc.occupants")
            .with_description("Current number of MUC occupants")
            .with_unit("user")
            .build()
    }

    /// Histogram for stanza processing latency.
    pub fn stanza_latency() -> Histogram<f64> {
        meter()
            .f64_histogram("xmpp.stanza.latency")
            .with_description("XMPP stanza processing latency")
            .with_unit("ms")
            .build()
    }

    /// Histogram for HTTP request duration.
    pub fn http_request_duration() -> Histogram<f64> {
        meter()
            .f64_histogram("http.request.duration")
            .with_description("HTTP request processing duration")
            .with_unit("ms")
            .build()
    }

    /// Histogram for database query duration.
    pub fn db_query_duration() -> Histogram<f64> {
        meter()
            .f64_histogram("db.query.duration")
            .with_description("Database query execution time")
            .with_unit("ms")
            .build()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_build_resource() {
        // Test that resource building doesn't panic
        let _resource = super::build_resource();
    }

    #[test]
    fn test_init_local() {
        // Note: Can only initialize once per process
        // This test just verifies the function compiles
        // let _ = super::init_local();
    }
}
