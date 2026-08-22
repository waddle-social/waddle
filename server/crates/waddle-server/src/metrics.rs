use std::sync::OnceLock;

use opentelemetry::metrics::{Counter, Gauge, Meter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalDepartureRetryOutcome {
    Completed,
    Requeued,
    ActorGone,
    Acknowledged,
    AckBarrier,
    InFlightBarrier,
    NotOccupant,
    Superseded,
    Abandoned,
    Retired,
    AwaitingReap,
    Stuck,
    Overflow,
}

impl LocalDepartureRetryOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Requeued => "requeued",
            Self::ActorGone => "actor_gone",
            Self::Acknowledged => "acknowledged",
            Self::AckBarrier => "ack_barrier",
            Self::InFlightBarrier => "in_flight_barrier",
            Self::NotOccupant => "not_occupant",
            Self::Superseded => "superseded",
            Self::Abandoned => "abandoned",
            Self::Retired => "retired",
            Self::AwaitingReap => "awaiting_reap",
            Self::Stuck => "stuck",
            Self::Overflow => "overflow",
        }
    }
}

fn meter() -> &'static Meter {
    static METER: OnceLock<Meter> = OnceLock::new();
    METER.get_or_init(|| opentelemetry::global::meter("waddle-server"))
}

pub fn record_local_departure_retry(outcome: LocalDepartureRetryOutcome) {
    static COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
    COUNTER
        .get_or_init(|| {
            meter()
                .u64_counter("waddle.muc.local_departure_retry")
                .with_description("Local MUC departure retry outcomes.")
                .build()
        })
        .add(
            1,
            &[opentelemetry::KeyValue::new("outcome", outcome.as_str())],
        );
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

#[cfg(test)]
mod tests {
    use super::LocalDepartureRetryOutcome as Outcome;

    #[test]
    fn local_departure_retry_labels_are_byte_stable() {
        assert_eq!(
            [
                Outcome::Completed,
                Outcome::Requeued,
                Outcome::ActorGone,
                Outcome::Acknowledged,
                Outcome::AckBarrier,
                Outcome::InFlightBarrier,
                Outcome::NotOccupant,
                Outcome::Superseded,
                Outcome::Abandoned,
                Outcome::Retired,
                Outcome::AwaitingReap,
                Outcome::Stuck,
                Outcome::Overflow,
            ]
            .map(Outcome::as_str),
            [
                "completed",
                "requeued",
                "actor_gone",
                "acknowledged",
                "ack_barrier",
                "in_flight_barrier",
                "not_occupant",
                "superseded",
                "abandoned",
                "retired",
                "awaiting_reap",
                "stuck",
                "overflow",
            ]
        );
    }
}
