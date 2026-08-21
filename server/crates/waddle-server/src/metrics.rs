use std::sync::OnceLock;

use opentelemetry::metrics::{Counter, Gauge, Meter};

fn meter() -> &'static Meter {
    static METER: OnceLock<Meter> = OnceLock::new();
    METER.get_or_init(|| opentelemetry::global::meter("waddle-server"))
}

pub fn record_local_departure_retry(outcome: &'static str) {
    static COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
    COUNTER
        .get_or_init(|| {
            meter()
                .u64_counter("waddle.muc.local_departure_retry")
                .with_description("Local MUC departure retry outcomes.")
                .build()
        })
        .add(1, &[opentelemetry::KeyValue::new("outcome", outcome)]);
}

pub fn record_local_departure_pending(kind: &'static str, count: i64) {
    static GAUGE: OnceLock<Gauge<i64>> = OnceLock::new();
    GAUGE
        .get_or_init(|| {
            meter()
                .i64_gauge("waddle.muc.local_departure_pending")
                .with_description("Local MUC departures retained until their projection converges.")
                .with_unit("{departure}")
                .build()
        })
        .record(count, &[opentelemetry::KeyValue::new("kind", kind)]);
}
