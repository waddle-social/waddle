//! In-memory OpenTelemetry metric and span test seams.
//!
//! Tests assert exported telemetry, never instrument internals. The metric
//! pipeline is a process-global
//! `SdkMeterProvider` backed by an [`InMemoryMetricExporter`] with
//! **delta temporality**: each [`MetricsTestGuard`] drains the
//! exporter on acquire, so a test observes only increments made while
//! it holds the guard.
//!
//! Serialization piggybacks on the existing
//! [`crate::prometheus::metrics_test_lock`] pattern so tests touching
//! the OTel pipeline, the legacy atomics, or both (dual-emit
//! migration tests) all contend on the same lock.
//!
//! The provider is installed globally exactly once per process and
//! never swapped, so instruments cached in macro `OnceLock`s always
//! bind to it — provided the guard is acquired before the first
//! increment in the process. Under `cargo nextest` (one process per
//! test) this holds trivially.

use std::sync::OnceLock;
use std::time::Duration;

use opentelemetry::trace::{Status, TracerProvider as _};
use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, Metric, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, InMemoryMetricExporterBuilder, PeriodicReader, SdkMeterProvider,
    Temporality,
};
use opentelemetry_sdk::trace::{
    InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SpanData,
};
use tracing_subscriber::prelude::*;

/// Install a thread-scoped tracing subscriber backed by an in-memory OTel
/// span exporter.
///
/// Keep the returned guard alive across the operation under test. Async tests
/// must use Tokio's current-thread flavor (`#[tokio::test]`'s default; pin it
/// with `flavor = "current_thread"`) because tracing's default subscriber
/// guard is thread-local — a multi-thread runtime is rejected here rather
/// than silently exporting nothing from worker threads.
pub fn acquire_spans() -> SpanTestGuard {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        assert_eq!(
            handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::CurrentThread,
            "acquire_spans() requires Tokio's current-thread runtime: the \
             subscriber installed by tracing::subscriber::set_default is \
             thread-local, so spans recorded on worker threads would bypass \
             the in-memory exporter. Use #[tokio::test(flavor = \"current_thread\")].",
        );
    }
    let exporter = InMemorySpanExporterBuilder::new().build();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let recorded_fields = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry()
        .with(RecordedFieldObserver {
            fields: recorded_fields.clone(),
        })
        .with(
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer("waddle-span-test"))
                .with_error_events_to_status(true)
                .with_error_records_to_exceptions(true),
        );
    let dispatch_guard = tracing::subscriber::set_default(subscriber);

    SpanTestGuard {
        exporter,
        provider,
        recorded_fields,
        _dispatch_guard: dispatch_guard,
    }
}

/// Observes every field value a span receives — at creation and through
/// later `Span::record` calls — WITHOUT waiting for the span to close.
///
/// Why closure can't be waited on (#1479): kameo parents actor spans
/// (`actor.handle_message`, `actor.lifecycle`) under the caller's current
/// span, so an actor spawned — or an abandoned/straggling ask handled —
/// inside an instrumented scope holds that scope's span open until the
/// actor task gets around to finishing, potentially for the actor's whole
/// life. A test that gates on the *exported* span therefore races actor
/// scheduling and can wait forever. Recording, by contrast, happens
/// synchronously on the asserting test's own call path.
struct RecordedFieldObserver {
    /// `(span name, field name, rendered value)` per record, in order.
    fields: std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
}

struct RecordedFieldVisitor<'a> {
    span_name: &'a str,
    fields: &'a mut Vec<(String, String, String)>,
}

impl tracing::field::Visit for RecordedFieldVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields.push((
            self.span_name.to_string(),
            field.name().to_string(),
            format!("{value:?}"),
        ));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields.push((
            self.span_name.to_string(),
            field.name().to_string(),
            value.to_string(),
        ));
    }
}

impl<S> tracing_subscriber::Layer<S> for RecordedFieldObserver
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if let Ok(mut fields) = self.fields.lock() {
            attrs.record(&mut RecordedFieldVisitor {
                span_name: attrs.metadata().name(),
                fields: &mut fields,
            });
        }
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        if let Ok(mut fields) = self.fields.lock() {
            values.record(&mut RecordedFieldVisitor {
                span_name: span.name(),
                fields: &mut fields,
            });
        }
    }
}

/// Holds a thread-scoped subscriber and exposes completed exported spans.
pub struct SpanTestGuard {
    exporter: InMemorySpanExporter,
    provider: SdkTracerProvider,
    recorded_fields: std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
    _dispatch_guard: tracing::subscriber::DefaultGuard,
}

impl SpanTestGuard {
    /// The last value recorded for `field` on any span named `span_name` —
    /// whether set at span creation or via a later `Span::record`.
    ///
    /// Use this (not [`Self::attribute_of`]) for spans whose scope awaited
    /// or messaged actors: those spans may be held open past the test's
    /// assertion point by kameo children (`actor.handle_message` /
    /// `actor.lifecycle` parent under the caller's span, and an actor
    /// spawned inside the scope pins it until the actor dies — #1479), so
    /// gating on the *exported* span races actor scheduling. Field records
    /// happen synchronously on the recording call path, so this observer
    /// sees them deterministically.
    pub fn recorded_field(&self, span_name: &str, field: &str) -> Option<String> {
        self.recorded_fields
            .lock()
            .ok()?
            .iter()
            .rev()
            .find(|(span, name, _)| span == span_name && name == field)
            .map(|(_, _, value)| value.clone())
    }

    /// Flush and return every completed span exported since acquisition.
    pub fn exported(&self) -> Vec<SpanData> {
        self.provider
            .force_flush()
            .expect("in-memory tracer provider must flush");
        self.exporter
            .get_finished_spans()
            .expect("in-memory exporter must yield finished spans")
    }

    /// Return the exported status of the named span, if present.
    pub fn status_of(&self, span_name: &str) -> Option<Status> {
        self.exported()
            .into_iter()
            .find(|span| span.name == span_name)
            .map(|span| span.status)
    }

    /// Whether any exported instance of the named span has error status.
    pub fn has_error_status(&self, span_name: &str) -> bool {
        self.exported()
            .into_iter()
            .any(|span| span.name == span_name && matches!(span.status, Status::Error { .. }))
    }

    /// Return a string-valued attribute from the named exported span.
    pub fn attribute_of(&self, span_name: &str, key: &str) -> Option<String> {
        self.exported()
            .into_iter()
            .find(|span| span.name == span_name)?
            .attributes
            .into_iter()
            .find(|attribute| attribute.key.as_str() == key)
            .map(|attribute| attribute.value.to_string())
    }
}

struct TestPipeline {
    exporter: InMemoryMetricExporter,
    provider: SdkMeterProvider,
}

fn pipeline() -> &'static TestPipeline {
    static PIPELINE: OnceLock<TestPipeline> = OnceLock::new();
    PIPELINE.get_or_init(|| {
        let exporter = InMemoryMetricExporterBuilder::new()
            .with_temporality(Temporality::Delta)
            .build();
        // The interval only exists to keep the background reader
        // quiet; collection happens through `force_flush`.
        let reader = PeriodicReader::builder(exporter.clone())
            .with_interval(Duration::from_secs(3600))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        opentelemetry::global::set_meter_provider(provider.clone());
        TestPipeline { exporter, provider }
    })
}

/// Serialize on the shared metrics lock, install the in-memory
/// pipeline (idempotent), and drain samples left over from earlier
/// tests in this process.
pub async fn acquire() -> MetricsTestGuard {
    let lock = crate::prometheus::metrics_test_lock().lock().await;
    let pipeline = pipeline();
    let _ = pipeline.provider.force_flush();
    pipeline.exporter.reset();
    MetricsTestGuard { _lock: lock }
}

/// Holds the metrics test lock and reads exported samples.
pub struct MetricsTestGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl MetricsTestGuard {
    /// Clone the process-wide provider for testing provider lifecycle helpers.
    pub fn provider(&self) -> SdkMeterProvider {
        pipeline().provider.clone()
    }

    /// Flush and return every batch exported since the guard was
    /// acquired (delta temporality: values are per-flush deltas).
    pub fn exported(&self) -> Vec<ResourceMetrics> {
        let pipeline = pipeline();
        pipeline
            .provider
            .force_flush()
            .expect("in-memory meter provider must flush");
        pipeline
            .exporter
            .get_finished_metrics()
            .expect("in-memory exporter must yield finished metrics")
    }

    /// Names of every metric exported since acquire.
    pub fn metric_names(&self) -> Vec<String> {
        self.exported()
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .map(|metric| metric.name().to_string())
            .collect()
    }

    /// The UCUM unit of the named metric, if it was exported.
    pub fn metric_unit(&self, name: &str) -> Option<String> {
        let mut unit = None;
        self.each_metric(name, |metric| {
            unit.get_or_insert_with(|| metric.unit().to_string());
        });
        unit
    }

    /// Sum of every `u64` counter data point for `name` whose
    /// attributes contain all `required` pairs. `None` when the
    /// instrument never exported (i.e. was never incremented).
    pub fn counter_sum(&self, name: &str, required: &[(&str, &str)]) -> Option<u64> {
        let mut total = None;
        self.each_metric(name, |metric| {
            let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
                return;
            };
            let entry = total.get_or_insert(0);
            for point in sum.data_points() {
                if attributes_contain(point.attributes(), required) {
                    *entry += point.value();
                }
            }
        });
        total
    }

    /// Total sample count of every `f64` histogram data point for
    /// `name` whose attributes contain all `required` pairs.
    pub fn histogram_count(&self, name: &str, required: &[(&str, &str)]) -> Option<u64> {
        let mut total = None;
        self.each_metric(name, |metric| {
            let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() else {
                return;
            };
            let entry = total.get_or_insert(0);
            for point in histogram.data_points() {
                if attributes_contain(point.attributes(), required) {
                    *entry += point.count();
                }
            }
        });
        total
    }

    /// Shape of the named `u64` counter: whether every exported batch is
    /// monotonic, and the attribute count of each exported data point.
    /// `None` when the instrument never exported as a u64 sum.
    pub fn counter_shape(&self, name: &str) -> Option<(bool, Vec<usize>)> {
        let mut shape: Option<(bool, Vec<usize>)> = None;
        self.each_metric(name, |metric| {
            let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
                return;
            };
            let (monotonic, attribute_counts) = shape.get_or_insert((true, Vec::new()));
            *monotonic &= sum.is_monotonic();
            attribute_counts.extend(sum.data_points().map(|point| point.attributes().count()));
        });
        shape
    }

    /// Visit every exported batch of the named metric.
    fn each_metric(&self, name: &str, mut visit: impl FnMut(&Metric)) {
        for resource in self.exported() {
            for scope in resource.scope_metrics() {
                for metric in scope.metrics() {
                    if metric.name() == name {
                        visit(metric);
                    }
                }
            }
        }
    }
}

fn attributes_contain<'a>(
    actual: impl Iterator<Item = &'a KeyValue>,
    required: &[(&str, &str)],
) -> bool {
    let actual: Vec<&KeyValue> = actual.collect();
    required.iter().all(|(key, value)| {
        actual
            .iter()
            .any(|kv| kv.key.as_str() == *key && kv.value.as_str() == *value)
    })
}
