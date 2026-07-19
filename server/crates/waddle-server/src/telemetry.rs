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
    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "waddle-server".to_string());
    let service_version = std::env::var("OTEL_SERVICE_VERSION")
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let service_instance_id = resolve_service_instance_id(
        std::env::var("OTEL_SERVICE_INSTANCE_ID").ok(),
        std::env::var("HOSTNAME").ok(),
        std::process::id(),
        uuid::Uuid::new_v4().simple().to_string().as_str(),
    );

    // `Resource::builder()` includes the SDK's `EnvResourceDetector`,
    // so standard `OTEL_RESOURCE_ATTRIBUTES` entries (helm sets
    // `deployment.environment` there) merge in without bespoke code.
    Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", service_name),
            KeyValue::new("service.version", service_version),
            KeyValue::new("service.instance.id", service_instance_id),
        ])
        .build()
}

/// Parsed trace-sampler configuration. Mirrors the OTel spec's
/// `OTEL_TRACES_SAMPLER` values; a typed intermediate so selection is
/// unit-testable (`Sampler::ParentBased` boxes a `dyn ShouldSample`
/// and cannot be inspected after construction).
#[derive(Debug, Clone, Copy, PartialEq)]
enum SamplerChoice {
    AlwaysOn,
    AlwaysOff,
    TraceIdRatio(f64),
    ParentBasedAlwaysOn,
    ParentBasedAlwaysOff,
    ParentBasedTraceIdRatio(f64),
}

/// Resolve the trace sampler from `OTEL_TRACES_SAMPLER` /
/// `OTEL_TRACES_SAMPLER_ARG` (helm `telemetry.tracesSampler*`).
/// Unset or unrecognized values fall back to the spec default,
/// `parentbased_traceidratio` with ratio 1.0, so trace volume has a
/// dial before it has a bill without changing today's behavior.
fn sampler_choice(name: Option<String>, arg: Option<String>) -> SamplerChoice {
    let ratio = nonblank(arg)
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|ratio| (0.0..=1.0).contains(ratio))
        .unwrap_or(1.0);

    // Sampler names are matched case-insensitively per the OTel
    // env-var spec. The ratio argument applies only when a ratio
    // sampler is explicitly selected; unset or unrecognized names get
    // the documented default with ratio 1.0 so a stray
    // OTEL_TRACES_SAMPLER_ARG can never silently shed traces.
    match nonblank(name).map(|n| n.to_ascii_lowercase()).as_deref() {
        Some("always_on") => SamplerChoice::AlwaysOn,
        Some("always_off") => SamplerChoice::AlwaysOff,
        Some("traceidratio") => SamplerChoice::TraceIdRatio(ratio),
        Some("parentbased_always_on") => SamplerChoice::ParentBasedAlwaysOn,
        Some("parentbased_always_off") => SamplerChoice::ParentBasedAlwaysOff,
        Some("parentbased_traceidratio") => SamplerChoice::ParentBasedTraceIdRatio(ratio),
        _ => SamplerChoice::ParentBasedTraceIdRatio(1.0),
    }
}

impl SamplerChoice {
    fn build(self) -> opentelemetry_sdk::trace::Sampler {
        use opentelemetry_sdk::trace::Sampler;
        match self {
            Self::AlwaysOn => Sampler::AlwaysOn,
            Self::AlwaysOff => Sampler::AlwaysOff,
            Self::TraceIdRatio(ratio) => Sampler::TraceIdRatioBased(ratio),
            Self::ParentBasedAlwaysOn => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
            Self::ParentBasedAlwaysOff => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
            Self::ParentBasedTraceIdRatio(ratio) => {
                Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)))
            }
        }
    }
}

/// The `OTEL_TRACES_SAMPLER` names `sampler_choice` maps to a
/// dedicated variant (everything else falls back to the default).
fn is_known_sampler(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "always_on"
            | "always_off"
            | "traceidratio"
            | "parentbased_always_on"
            | "parentbased_always_off"
            | "parentbased_traceidratio"
    )
}

fn sampler_from_env() -> opentelemetry_sdk::trace::Sampler {
    sampler_choice(
        std::env::var("OTEL_TRACES_SAMPLER").ok(),
        std::env::var("OTEL_TRACES_SAMPLER_ARG").ok(),
    )
    .build()
}

/// Resolve the `service.instance.id` resource attribute.
///
/// With multiple replicas, OTLP-pushed metrics that share an identical
/// resource collapse into one clobbered series downstream, so every
/// process needs a distinct, stable-for-its-lifetime identity.
/// Precedence: explicit `OTEL_SERVICE_INSTANCE_ID` override, then
/// `HOSTNAME` (the kubelet sets it to the pod name in every pod)
/// suffixed with the pid so several processes sharing a hostname —
/// bare metal, docker-compose — stay distinguishable, then a
/// pid+entropy fallback: pids alone repeat across PID namespaces, so
/// hostname-less containers need the per-process entropy to avoid
/// colliding. Blank values are skipped.
fn resolve_service_instance_id(
    explicit: Option<String>,
    hostname: Option<String>,
    pid: u32,
    fallback_entropy: &str,
) -> String {
    if let Some(id) = nonblank(explicit) {
        return id;
    }
    if let Some(host) = nonblank(hostname) {
        return format!("{host}-{pid}");
    }
    format!("waddle-server-{pid}-{fallback_entropy}")
}

fn nonblank(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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
/// - `OTEL_SERVICE_VERSION`: Service version (default: crate version)
/// - `OTEL_SERVICE_INSTANCE_ID`: Per-replica instance id (default:
///   `<HOSTNAME>-<pid>` — the pod name in Kubernetes — then a
///   pid+entropy fallback when no hostname is available)
/// - `OTEL_RESOURCE_ATTRIBUTES`: Standard comma-separated resource
///   attributes (helm sets `deployment.environment=<env>` here),
///   merged by the SDK's env detector
/// - `OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG`: Trace sampler
///   (default: `parentbased_traceidratio` with ratio 1.0)
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

    // Build tracer provider with batch processor. The sampler comes
    // from OTEL_TRACES_SAMPLER / OTEL_TRACES_SAMPLER_ARG (default
    // parentbased_traceidratio 1.0 — today's sample-everything
    // behavior, but now operator-tunable from helm values).
    let sampler = sampler_from_env();
    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(trace_exporter)
        .with_sampler(sampler)
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

    // Register the pod-wide observable gauges eagerly so a churn-free
    // pod exports 0 instead of no series (the waddle_connected_users
    // alias reads absent otherwise — Qodo review on PR #1426).
    waddle_xmpp::metrics::init_pod_gauges();

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

    // The sampler was resolved before the subscriber existed, so an
    // unrecognized name could only fall back silently; surface it now.
    if let Some(name) = nonblank(std::env::var("OTEL_TRACES_SAMPLER").ok()) {
        if !is_known_sampler(&name) {
            tracing::warn!(
                sampler = %name,
                "Unrecognized OTEL_TRACES_SAMPLER; using parentbased_traceidratio 1.0"
            );
        }
    }

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
mod tests {
    use opentelemetry::Key;

    #[test]
    fn test_build_resource() {
        // Test that resource building doesn't panic
        let _resource = super::build_resource();
    }

    #[test]
    fn build_resource_includes_nonempty_service_instance_id() {
        let resource = super::build_resource();
        let value = resource
            .get(&Key::from_static_str("service.instance.id"))
            .expect("resource must carry service.instance.id");
        assert!(
            !value.as_str().trim().is_empty(),
            "service.instance.id must be non-empty"
        );
    }

    #[test]
    fn instance_id_prefers_explicit_override() {
        let id = super::resolve_service_instance_id(
            Some("explicit-id".to_string()),
            Some("pod-name".to_string()),
            7,
            "entropy",
        );
        assert_eq!(id, "explicit-id");
    }

    #[test]
    fn instance_id_uses_hostname_with_pid_suffix() {
        // The pid suffix keeps several processes sharing a hostname
        // (bare metal, docker-compose) from publishing one identity.
        let id = super::resolve_service_instance_id(
            None,
            Some("waddle-server-abc-xyz".to_string()),
            7,
            "entropy",
        );
        assert_eq!(id, "waddle-server-abc-xyz-7");
    }

    #[test]
    fn instance_id_falls_back_to_pid_and_entropy() {
        // Pids repeat across PID namespaces, so the hostname-less
        // fallback must carry per-process entropy.
        let id = super::resolve_service_instance_id(
            Some("   ".to_string()),
            Some("\t".to_string()),
            7,
            "0a1b2c3d",
        );
        assert_eq!(id, "waddle-server-7-0a1b2c3d");
    }

    #[test]
    fn instance_id_trims_surrounding_whitespace() {
        let id =
            super::resolve_service_instance_id(None, Some(" pod-1 \n".to_string()), 7, "entropy");
        assert_eq!(id, "pod-1-7");
    }

    #[test]
    fn test_init_local() {
        // Note: Can only initialize once per process
        // This test just verifies the function compiles
        // let _ = super::init_local();
    }

    #[test]
    fn sampler_defaults_to_parentbased_ratio_one() {
        assert_eq!(
            super::sampler_choice(None, None),
            super::SamplerChoice::ParentBasedTraceIdRatio(1.0)
        );
    }

    #[test]
    fn sampler_honors_parentbased_traceidratio_arg() {
        assert_eq!(
            super::sampler_choice(
                Some("parentbased_traceidratio".to_string()),
                Some("0.25".to_string()),
            ),
            super::SamplerChoice::ParentBasedTraceIdRatio(0.25)
        );
    }

    #[test]
    fn sampler_supports_every_spec_variant() {
        use super::SamplerChoice;
        assert_eq!(
            super::sampler_choice(Some("always_on".to_string()), None),
            SamplerChoice::AlwaysOn
        );
        assert_eq!(
            super::sampler_choice(Some("always_off".to_string()), None),
            SamplerChoice::AlwaysOff
        );
        assert_eq!(
            super::sampler_choice(Some("traceidratio".to_string()), Some("0.5".to_string())),
            SamplerChoice::TraceIdRatio(0.5)
        );
        assert_eq!(
            super::sampler_choice(Some("parentbased_always_on".to_string()), None),
            SamplerChoice::ParentBasedAlwaysOn
        );
        assert_eq!(
            super::sampler_choice(Some("parentbased_always_off".to_string()), None),
            SamplerChoice::ParentBasedAlwaysOff
        );
    }

    #[test]
    fn sampler_names_match_case_insensitively() {
        assert_eq!(
            super::sampler_choice(Some("ALWAYS_OFF".to_string()), None),
            super::SamplerChoice::AlwaysOff
        );
        assert_eq!(
            super::sampler_choice(
                Some("ParentBased_TraceIdRatio".to_string()),
                Some("0.1".to_string())
            ),
            super::SamplerChoice::ParentBasedTraceIdRatio(0.1)
        );
        assert!(super::is_known_sampler("TRACEIDRATIO"));
    }

    #[test]
    fn sampler_arg_applies_only_to_explicit_ratio_samplers() {
        // A stray OTEL_TRACES_SAMPLER_ARG with no (or an unknown)
        // sampler name must never shed traces.
        assert_eq!(
            super::sampler_choice(None, Some("0.01".to_string())),
            super::SamplerChoice::ParentBasedTraceIdRatio(1.0)
        );
        assert_eq!(
            super::sampler_choice(Some("bogus".to_string()), Some("0.01".to_string())),
            super::SamplerChoice::ParentBasedTraceIdRatio(1.0)
        );
    }

    #[test]
    fn sampler_falls_back_on_garbage_input() {
        // Unknown sampler name and out-of-range/unparsable ratios must
        // not disable tracing; they fall back to the 1.0 default.
        for arg in [Some("7.5".to_string()), Some("nan".to_string()), None] {
            assert_eq!(
                super::sampler_choice(Some("bogus_sampler".to_string()), arg),
                super::SamplerChoice::ParentBasedTraceIdRatio(1.0)
            );
        }
    }

    #[test]
    fn every_sampler_choice_builds() {
        // Building must not panic for any variant; the ParentBased
        // internals are opaque past this point by SDK design.
        for choice in [
            super::SamplerChoice::AlwaysOn,
            super::SamplerChoice::AlwaysOff,
            super::SamplerChoice::TraceIdRatio(0.5),
            super::SamplerChoice::ParentBasedAlwaysOn,
            super::SamplerChoice::ParentBasedAlwaysOff,
            super::SamplerChoice::ParentBasedTraceIdRatio(1.0),
        ] {
            let _sampler = choice.build();
        }
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
