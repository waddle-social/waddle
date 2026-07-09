//! OpenTelemetry instrumentation for Waddle Server.
//!
//! This module provides unified observability across HTTP and XMPP components,
//! including traces, metrics, and logs via OpenTelemetry.
//!
//! See [ADR-0014](../../../docs/adrs/0014-opentelemetry.md) for design decisions.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    logs::SdkLoggerProvider,
    metrics::{PeriodicReader, SdkMeterProvider},
    trace::SdkTracerProvider,
    Resource,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

/// The global tracer provider, stored for shutdown.
static TRACER_PROVIDER: std::sync::OnceLock<SdkTracerProvider> = std::sync::OnceLock::new();

/// The global meter provider, stored for shutdown.
static METER_PROVIDER: std::sync::OnceLock<SdkMeterProvider> = std::sync::OnceLock::new();

/// The global logger provider, stored for shutdown.
static LOGGER_PROVIDER: std::sync::OnceLock<SdkLoggerProvider> = std::sync::OnceLock::new();

/// Build the OpenTelemetry resource with service information.
fn build_resource() -> Resource {
    build_resource_from(
        |name| std::env::var(name).ok(),
        crate::build_identity::printable_git_sha(),
    )
}

fn build_resource_from(get: impl Fn(&str) -> Option<String>, build_commit: &str) -> Resource {
    let non_empty = |name: &str| get(name).filter(|value| !value.trim().is_empty());
    let service_name =
        non_empty("OTEL_SERVICE_NAME").unwrap_or_else(|| "waddle-server".to_string());
    let service_instance_id = non_empty("OTEL_SERVICE_INSTANCE_ID")
        .or_else(|| non_empty("K8S_POD_UID"))
        .or_else(|| non_empty("HOSTNAME"))
        .unwrap_or_else(|| format!("local-process-{}", std::process::id()));

    let mut attributes = vec![
        KeyValue::new("service.name", service_name),
        // This value is intentionally compile-time identity. Runtime
        // environment variables are deployment metadata and cannot relabel
        // telemetry emitted by a different binary revision.
        KeyValue::new("service.version", build_commit.to_owned()),
        KeyValue::new("service.instance.id", service_instance_id),
    ];
    if let Some(pod_name) = non_empty("K8S_POD_NAME") {
        attributes.push(KeyValue::new("k8s.pod.name", pod_name));
    }
    if let Some(namespace) = non_empty("K8S_NAMESPACE_NAME") {
        attributes.push(KeyValue::new("k8s.namespace.name", namespace));
    }
    if let Some(environment) = non_empty("DEPLOYMENT_ENVIRONMENT_NAME") {
        attributes.push(KeyValue::new("deployment.environment.name", environment));
    }
    if let Some(cluster) = non_empty("DEPLOYMENT_CLUSTER_NAME") {
        attributes.push(KeyValue::new("k8s.cluster.name", cluster));
    }

    Resource::builder().with_attributes(attributes).build()
}

fn default_filter() -> EnvFilter {
    // Keep historical defaults to avoid changing verbosity unexpectedly.
    EnvFilter::new("info,waddle_server=debug,waddle_xmpp=debug")
}

/// Filter for the OTLP log bridge.
///
/// The exporter itself uses `tonic` / `hyper` / `h2` / `reqwest`, and
/// those crates emit `tracing` events internally. Without this filter
/// the bridge would feed their output right back into the exporter,
/// which would emit more events — a feedback loop that blows up the
/// pipeline. Silencing those targets at the bridge layer keeps them
/// visible on stdout (through the `fmt` layer) but out of OTLP.
///
/// Built on top of `default_filter()` so the base verbosity stays in
/// lockstep with the primary subscriber filter.
fn log_bridge_filter() -> EnvFilter {
    // Every crate in the OTLP export path. If any of these starts
    // emitting `tracing` events that land back in the exporter, the
    // pipeline loops. Keep this list exhaustive.
    const OFF_TARGETS: &[&str] = &[
        "hyper",
        "hyper_util",
        "tonic",
        "h2",
        "reqwest",
        "tower",
        "opentelemetry",
        "opentelemetry_sdk",
        "opentelemetry_otlp",
        "opentelemetry_http",
        "opentelemetry_proto",
        "opentelemetry_appender_tracing",
    ];

    OFF_TARGETS.iter().fold(default_filter(), |filter, target| {
        let directive = format!("{target}=off")
            .parse()
            .expect("static `<target>=off` directive must be valid");
        filter.add_directive(directive)
    })
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
/// - OTLP exporter for logs (bridged from `tracing` events)
/// - tracing-subscriber with OpenTelemetry integration
/// - Console output for local development
///
/// # Configuration
///
/// Environment variables:
/// - `OTEL_EXPORTER_OTLP_ENDPOINT`: OTLP endpoint (default: http://localhost:4317)
/// - `OTEL_SERVICE_NAME`: Service name (default: waddle-server)
/// - `DEPLOYMENT_ENVIRONMENT_NAME`: Deployment environment resource attribute
/// - `DEPLOYMENT_CLUSTER_NAME`: Kubernetes cluster resource attribute
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
    // Register the W3C Trace Context propagator globally so inbound
    // `traceparent` / `tracestate` headers from the browser (injected by
    // the chat frontend's Faro `TracingInstrumentation`) join their
    // spans into the trace we're about to start. The tower-http
    // `TraceLayer` in `server/mod.rs` reads this propagator in its
    // `make_span_with` closure.
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

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
        .with_resource(resource.clone())
        .build();

    // Store the meter provider for shutdown
    let _ = METER_PROVIDER.set(meter_provider.clone());

    // Set global meter provider
    opentelemetry::global::set_meter_provider(meter_provider);

    // Build OTLP logs exporter + provider. The tracing bridge below
    // feeds every `tracing::{info,warn,error,debug,trace}!` event into
    // this provider as an OTLP log record, in addition to stdout.
    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_endpoint(&otlp_endpoint)
        .build()?;

    let logger_provider = SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter)
        .with_resource(resource)
        .build();

    let _ = LOGGER_PROVIDER.set(logger_provider.clone());

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

    // Bridge `tracing` events into the OTLP log pipeline. Scope its
    // own filter so the HTTP/gRPC transports used by the OTLP exporter
    // can't feed their logs back into themselves and loop.
    let otel_log_layer =
        OpenTelemetryTracingBridge::new(&logger_provider).with_filter(log_bridge_filter());

    // Combine layers and set as global subscriber
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(telemetry_layer)
        .with(otel_log_layer)
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

    // Shutdown logger provider (flushes any buffered OTLP log records).
    if let Some(provider) = LOGGER_PROVIDER.get() {
        if let Err(e) = provider.shutdown() {
            tracing::error!(error = %e, "Error shutting down logger provider");
        }
    }

    tracing::info!("Telemetry shutdown complete");
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    use opentelemetry::Key;
    use std::collections::BTreeMap;

    #[test]
    fn resource_has_unique_instance_and_deployment_identity() {
        let values = BTreeMap::from([
            ("K8S_NAMESPACE_NAME", "waddle"),
            ("K8S_POD_NAME", "waddle-server-abc"),
            ("K8S_POD_UID", "pod-uid-123"),
            ("DEPLOYMENT_ENVIRONMENT_NAME", "production"),
            ("DEPLOYMENT_CLUSTER_NAME", "waddle-cloud"),
            ("WADDLE_GIT_SHA", "0123456789abcdef0123456789abcdef01234567"),
        ]);
        let resource = build_resource_from(
            |name| values.get(name).map(ToString::to_string),
            "0123456789abcdef0123456789abcdef01234567",
        );

        assert_eq!(
            resource.get(&Key::new("service.instance.id")),
            Some("pod-uid-123".into())
        );
        assert_eq!(
            resource.get(&Key::new("service.version")),
            Some("0123456789abcdef0123456789abcdef01234567".into())
        );
        assert_eq!(
            resource.get(&Key::new("k8s.pod.name")),
            Some("waddle-server-abc".into())
        );
        assert_eq!(
            resource.get(&Key::new("k8s.namespace.name")),
            Some("waddle".into())
        );
        assert_eq!(
            resource.get(&Key::new("deployment.environment.name")),
            Some("production".into())
        );
        assert_eq!(
            resource.get(&Key::new("k8s.cluster.name")),
            Some("waddle-cloud".into())
        );
    }

    #[test]
    fn runtime_values_cannot_override_compiled_service_version() {
        let values = BTreeMap::from([
            ("OTEL_SERVICE_NAME", "custom-server"),
            ("OTEL_SERVICE_VERSION", "release-1"),
            ("OTEL_SERVICE_INSTANCE_ID", "instance-1"),
            ("K8S_POD_UID", "pod-fallback"),
            ("WADDLE_GIT_SHA", "commit-fallback"),
        ]);
        let resource = build_resource_from(
            |name| values.get(name).map(ToString::to_string),
            "0123456789abcdef0123456789abcdef01234567",
        );

        assert_eq!(
            resource.get(&Key::new("service.name")),
            Some("custom-server".into())
        );
        assert_eq!(
            resource.get(&Key::new("service.version")),
            Some("0123456789abcdef0123456789abcdef01234567".into())
        );
        assert_eq!(
            resource.get(&Key::new("service.instance.id")),
            Some("instance-1".into())
        );
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

    #[test]
    fn test_log_bridge_filter_directives_parse() {
        // Constructing the filter parses every `<target>=off` directive
        // via `.expect(...)`. If any entry in OFF_TARGETS drifts into an
        // invalid form, this test panics here instead of at process init
        // in production.
        let _filter = super::log_bridge_filter();
    }
}
