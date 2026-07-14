//! Export contract for the stanza-handler wedge-backstop metric.
//!
//! A narrow source-contract regression pins the timeout arm to the production
//! `record_stanza_handler_timeout` helper. The exporter regression then pins
//! what that helper hands to the OpenTelemetry SDK before the OTLP collector
//! translates the instrument into a Prometheus series.

use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::{
    data::{AggregatedMetrics, MetricData},
    InMemoryMetricExporter, SdkMeterProvider,
};

const INSTRUMENT_NAME: &str = "xmpp.stanza.handler.timeout";
const FRAME_BACKSTOP_SOURCE: &str =
    include_str!("../src/server/routes/websocket/frame_backstop.rs");

#[test]
fn frame_backstop_timeout_arm_records_the_production_metric_helper() {
    let timeout_arm = FRAME_BACKSTOP_SOURCE
        .split("fn on_timeout(self) -> StanzaTimeout {")
        .nth(1)
        .and_then(|source| source.split("self.span.record").next())
        .expect("StanzaBackstop::on_timeout source before span recording");

    let executable_calls = timeout_arm
        .lines()
        .filter(|line| {
            line.trim() == "metrics::record_stanza_handler_timeout(self.kind, &self.payload_ns);"
        })
        .count();
    assert_eq!(executable_calls, 1, "timeout arm must record exactly once");
}

#[test]
fn stanza_handler_timeout_exports_the_canonical_counter_and_attributes() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());

    waddle_xmpp::metrics::record_stanza_handler_timeout("iq", "urn:test:wedged");
    waddle_xmpp::metrics::record_stanza_handler_timeout("message", "");
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

    assert_eq!(
        sum.data_points().count(),
        2,
        "the two reachable label sets should produce distinct series"
    );
    for (kind, payload_ns) in [("iq", "urn:test:wedged"), ("message", "")] {
        let point = sum
            .data_points()
            .find(|point| {
                point
                    .attributes()
                    .any(|attribute| attribute == &KeyValue::new("kind", kind))
                    && point
                        .attributes()
                        .any(|attribute| attribute == &KeyValue::new("payload_ns", payload_ns))
            })
            .expect("reachable stanza timeout label set should be exported");
        assert_eq!(point.value(), 1);
        assert_eq!(
            point.attributes().count(),
            2,
            "timeout attribute schema must stay limited to the documented axes"
        );
    }
}
