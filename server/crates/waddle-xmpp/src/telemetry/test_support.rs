//! In-memory metric-reader test seam — the one approved metric test
//! seam from spec #1323.
//!
//! Tests assert **exported samples** (name, unit, attributes, value),
//! never instrument internals. The pipeline is a process-global
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

use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, Metric, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, InMemoryMetricExporterBuilder, PeriodicReader, SdkMeterProvider,
    Temporality,
};

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
