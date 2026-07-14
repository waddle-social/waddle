//! Export contract for the stanza-handler wedge-backstop metric.
//!
//! The frame-backstop tests prove that an elapsed IQ/message/presence dispatch
//! calls `record_stanza_handler_timeout`. This test pins what that production
//! helper hands to the OpenTelemetry SDK before the OTLP collector translates
//! the instrument into a Prometheus series.

use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::{
    data::{AggregatedMetrics, MetricData},
    InMemoryMetricExporter, SdkMeterProvider,
};

const INSTRUMENT_NAME: &str = "xmpp.stanza.handler.timeout";

#[test]
fn stanza_handler_timeout_exports_the_canonical_counter_and_attributes() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());

    waddle_xmpp::metrics::record_stanza_handler_timeout("message", "urn:test:wedged");
    provider
        .force_flush()
        .expect("timeout metric should flush to the configured exporter");

    let resource_metrics = exporter
        .get_finished_metrics()
        .expect("timeout metric should be available from the exporter");
    let metric = resource_metrics
        .iter()
        .flat_map(|resource| resource.scope_metrics())
        .flat_map(|scope| scope.metrics())
        .find(|metric| metric.name() == INSTRUMENT_NAME)
        .expect("canonical stanza-handler timeout instrument should be exported");

    assert_eq!(
        metric.description(),
        "Inbound stanza handlers that exceeded the per-connection wedge backstop"
    );
    assert_eq!(metric.unit(), "stanza");

    let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
        panic!("stanza-handler timeout must export as a u64 sum");
    };
    assert!(sum.is_monotonic(), "timeout counter must be monotonic");

    let points: Vec<_> = sum.data_points().collect();
    assert_eq!(points.len(), 1, "one label set should produce one series");
    assert_eq!(points[0].value(), 1);

    let attributes: Vec<_> = points[0].attributes().cloned().collect();
    assert_eq!(
        attributes.len(),
        2,
        "timeout cardinality must stay limited to the two documented axes"
    );
    assert!(attributes.contains(&KeyValue::new("kind", "message")));
    assert!(attributes.contains(&KeyValue::new("payload_ns", "urn:test:wedged")));
}
