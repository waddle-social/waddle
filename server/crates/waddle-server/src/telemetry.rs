//! OpenTelemetry instrumentation for Waddle Server.
//!
//! This module provides unified observability across HTTP and XMPP components,
//! including traces, metrics, and logs via OpenTelemetry.
//!
//! See [ADR-0014](../../../docs/adrs/0014-opentelemetry.md) for design decisions.

use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

mod span_hygiene;
mod span_noise;
#[cfg(test)]
mod tests {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry::Key;
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_sdk::logs::{
        BatchConfigBuilder as LogBatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider,
    };
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider, Temporality};
    use opentelemetry_sdk::trace::{
        BatchConfigBuilder as TraceBatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider,
        SpanExporter,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer as _;

    #[derive(Debug)]
    struct FailingSpanExporter {
        exports: Arc<AtomicUsize>,
    }

    impl SpanExporter for FailingSpanExporter {
        fn export(
            &self,
            _batch: Vec<opentelemetry_sdk::trace::SpanData>,
        ) -> impl std::future::Future<Output = opentelemetry_sdk::error::OTelSdkResult> + Send
        {
            self.exports.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(opentelemetry_sdk::error::OTelSdkError::Timeout(
                Duration::from_millis(1),
            )))
        }
    }

    #[derive(Debug)]
    struct FailingLogExporter {
        exports: Arc<AtomicUsize>,
    }

    impl opentelemetry_sdk::logs::LogExporter for FailingLogExporter {
        fn export(
            &self,
            _batch: opentelemetry_sdk::logs::LogBatch<'_>,
        ) -> impl std::future::Future<Output = opentelemetry_sdk::error::OTelSdkResult> + Send
        {
            self.exports.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(opentelemetry_sdk::error::OTelSdkError::Timeout(
                Duration::from_millis(1),
            )))
        }
    }

    #[derive(Debug)]
    struct FailingMetricExporter {
        exports: Arc<AtomicUsize>,
    }

    impl PushMetricExporter for FailingMetricExporter {
        fn export(
            &self,
            _metrics: &ResourceMetrics,
        ) -> impl std::future::Future<Output = opentelemetry_sdk::error::OTelSdkResult> + Send
        {
            self.exports.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(opentelemetry_sdk::error::OTelSdkError::Timeout(
                Duration::from_millis(1),
            )))
        }

        fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(
            &self,
            _timeout: Duration,
        ) -> opentelemetry_sdk::error::OTelSdkResult {
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }
    }

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
    fn instance_id_uses_hostname_verbatim() {
        // #1434: the identity must equal the pod name exactly so it
        // matches the Prometheus scrape path's `instance` label. No
        // pid/ordinal suffix.
        let id = super::resolve_service_instance_id(
            None,
            Some("waddle-server-abc-xyz".to_string()),
            7,
            "entropy",
        );
        assert_eq!(id, "waddle-server-abc-xyz");
    }

    #[test]
    fn instance_id_is_independent_of_pid() {
        // Two processes on the same pod resolve to the same identity;
        // a restart under a new pid keeps the pod's series stable.
        let host = Some("waddle-server-abc-xyz".to_string());
        assert_eq!(
            super::resolve_service_instance_id(None, host.clone(), 1, "entropy"),
            super::resolve_service_instance_id(None, host, 4242, "other-entropy"),
        );
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
            super::resolve_service_instance_id(None, Some(" pod-a \n".to_string()), 7, "entropy");
        assert_eq!(id, "pod-a");
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

    async fn telemetry_outage_child() {
        use waddle_xmpp::ownership::{
            ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, ObservedClaimStore,
        };

        let _metrics_guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        let trace_exports = Arc::new(AtomicUsize::new(0));
        let log_exports = Arc::new(AtomicUsize::new(0));
        let metric_exports = Arc::new(AtomicUsize::new(0));

        let trace_exporter = FailingSpanExporter {
            exports: trace_exports.clone(),
        };
        let trace_provider = SdkTracerProvider::builder()
            .with_span_processor(
                BatchSpanProcessor::builder(trace_exporter)
                    .with_batch_config(
                        TraceBatchConfigBuilder::default()
                            .with_scheduled_delay(Duration::from_millis(10))
                            .with_max_queue_size(32)
                            .with_max_export_batch_size(1)
                            .build(),
                    )
                    .build(),
            )
            .build();
        let tracer = trace_provider.tracer("telemetry-outage-test");

        let metric_exporter = FailingMetricExporter {
            exports: metric_exports.clone(),
        };
        let meter_provider = SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(metric_exporter)
                    .with_interval(Duration::from_millis(10))
                    .build(),
            )
            .build();
        opentelemetry::global::set_meter_provider(meter_provider.clone());

        let log_exporter = FailingLogExporter {
            exports: log_exports.clone(),
        };
        let logger_provider = SdkLoggerProvider::builder()
            .with_log_processor(
                BatchLogProcessor::builder(log_exporter)
                    .with_batch_config(
                        LogBatchConfigBuilder::default()
                            .with_scheduled_delay(Duration::from_millis(10))
                            .with_max_queue_size(32)
                            .with_max_export_batch_size(1)
                            .build(),
                    )
                    .build(),
            )
            .build();

        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::DEBUG)
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .with(
                OpenTelemetryTracingBridge::new(&logger_provider)
                    .with_filter(super::log_bridge_filter()),
            );
        let _subscriber = tracing::subscriber::set_default(subscriber);

        let store = ObservedClaimStore::new(InProcessClaimStore::new());
        let entity = Entity::new(EntityType::SmSession, "telemetry-outage-test");
        let owner = NodeIdentity::new("node-a", "epoch-a");

        let elapsed = {
            let operation_span = tracing::info_span!("claim_mutation_outage_test");
            let _entered = operation_span.enter();
            let started = Instant::now();
            let acquired =
                tokio::time::timeout(Duration::from_millis(250), store.acquire(&entity, &owner))
                    .await
                    .expect("claim mutation must complete within the bound")
                    .expect("claim mutation must succeed despite exporter outage");
            assert_eq!(
                acquired.0, 0,
                "first in-process claim epoch should stay unchanged"
            );
            started.elapsed()
        };

        assert!(
            elapsed < Duration::from_millis(250),
            "claim mutation took too long with exporters failing: {elapsed:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline
            && (trace_exports.load(Ordering::SeqCst) == 0
                || log_exports.load(Ordering::SeqCst) == 0
                || metric_exports.load(Ordering::SeqCst) == 0)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            trace_exports.load(Ordering::SeqCst) > 0,
            "trace batch processor never invoked the failing OTLP exporter"
        );
        assert!(
            log_exports.load(Ordering::SeqCst) > 0,
            "log batch processor never invoked the failing OTLP exporter"
        );
        assert!(
            metric_exports.load(Ordering::SeqCst) > 0,
            "metric periodic reader never invoked the failing OTLP exporter"
        );

        let _ = trace_provider.shutdown();
        let _ = logger_provider.shutdown();
        let _ = meter_provider.shutdown();
    }

    #[test]
    fn telemetry_outage_does_not_block_claim_mutation_logic() {
        const CHILD_ENV: &str = "WADDLE_TELEMETRY_OUTAGE_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime")
                .block_on(telemetry_outage_child());
            return;
        }

        let status =
            std::process::Command::new(std::env::current_exe().expect("current test binary"))
                .arg("--exact")
                .arg("telemetry::tests::telemetry_outage_does_not_block_claim_mutation_logic")
                .arg("--nocapture")
                .env(CHILD_ENV, "1")
                .status()
                .expect("spawn isolated telemetry outage child process");
        assert!(
            status.success(),
            "isolated telemetry outage child failed: {status}"
        );
    }
}

mod implementation;
#[cfg(test)]
use implementation::build_resource;
pub use implementation::flush_metrics_before_exit;
pub use implementation::init;
pub use implementation::init_local;
#[cfg(test)]
use implementation::is_known_sampler;
#[cfg(test)]
use implementation::log_bridge_filter;
pub(crate) use implementation::mark_span_error;
#[cfg(test)]
use implementation::resolve_service_instance_id;
#[cfg(test)]
use implementation::sampler_choice;
pub use implementation::shutdown;
pub use implementation::MetricsFlush;
#[cfg(test)]
use implementation::SamplerChoice;
