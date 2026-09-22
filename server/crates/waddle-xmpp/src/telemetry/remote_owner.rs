//! Bounded-cardinality progress and backlog samples for remote owner mirrors.
use super::attributes::RemoteOwnerMirrorOutcome;
use std::time::Duration;

const PENDING_AGE_BUCKETS: [f64; 10] = [
    1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 900.0, 1800.0, 3600.0,
];

/// One attempted mirror, including live mirrors checked for committed expiry.
pub fn record_mirror_attempt(outcome: RemoteOwnerMirrorOutcome) {
    add_mirror_attempt(1, outcome);
}

// Startup failure-series seeding and real progress share this emitting site.
pub(super) fn add_mirror_attempt(count: u64, outcome: RemoteOwnerMirrorOutcome) {
    crate::counter_add!(
        "xmpp.remote_owner_mirror.attempts",
        "{mirror}",
        "Remote owner mirror reconciliation attempts by bounded outcome.",
        count,
        outcome,
    );
}

/// Samples taken at the end of a page. Pending means locally known owed
/// retirements, not all live mirrors. Age starts when retirement is detected;
/// undetected expiry and process restarts are not included in that age.
pub fn record_mirror_backlog(inventory: usize, pending: usize, oldest: Duration) {
    crate::histogram_record!(
        "xmpp.remote_owner_mirror.inventory",
        "{mirror}",
        "Remote owner mirror inventory sampled after each reconciliation page.",
        inventory as f64,
    );
    crate::histogram_record!(
        "xmpp.remote_owner_mirror.pending",
        "{mirror}",
        "Known owed remote owner retirements sampled after each reconciliation page.",
        pending as f64,
    );
    crate::histogram_record!(
        "xmpp.remote_owner_mirror.oldest_pending_age",
        "s",
        "Age since local detection of the oldest owed mirror retirement; zero with no pending work.",
        buckets: PENDING_AGE_BUCKETS,
        oldest.as_secs_f64(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::attributes::{Janitor, SweepOutcome};

    #[tokio::test]
    async fn mirror_failure_counter_starts_at_zero_without_seeding_heartbeat() {
        let guard = crate::telemetry::test_support::acquire().await;
        crate::telemetry::reliability::register_reliability_counters();
        assert_eq!(
            guard.counter_sum(
                "xmpp.remote_owner_mirror.attempts",
                &[("outcome", "failed")]
            ),
            Some(0)
        );
        assert_eq!(
            guard.counter_sum(
                "waddle.janitor.sweeps",
                &[("janitor", "remote_owner_mirror")]
            ),
            None
        );
        record_mirror_attempt(RemoteOwnerMirrorOutcome::Failed);
        assert_eq!(
            guard.counter_sum(
                "xmpp.remote_owner_mirror.attempts",
                &[("outcome", "failed")]
            ),
            Some(1)
        );
    }

    #[tokio::test]
    async fn mirror_progress_and_backlog_are_exported() {
        let guard = crate::telemetry::test_support::acquire().await;
        for outcome in [
            RemoteOwnerMirrorOutcome::Live,
            RemoteOwnerMirrorOutcome::Retired,
            RemoteOwnerMirrorOutcome::Superseded,
            RemoteOwnerMirrorOutcome::Deferred,
            RemoteOwnerMirrorOutcome::Failed,
        ] {
            record_mirror_attempt(outcome);
        }
        for outcome in ["live", "retired", "superseded", "deferred", "failed"] {
            assert_eq!(
                guard.counter_sum("xmpp.remote_owner_mirror.attempts", &[("outcome", outcome)]),
                Some(1)
            );
        }
        record_mirror_backlog(1024, 64, Duration::from_secs(12));
        record_mirror_backlog(0, 0, Duration::ZERO);
        for metric in [
            "xmpp.remote_owner_mirror.inventory",
            "xmpp.remote_owner_mirror.pending",
            "xmpp.remote_owner_mirror.oldest_pending_age",
        ] {
            assert_eq!(guard.histogram_count(metric, &[]), Some(2));
        }
        let mut sums = std::collections::BTreeMap::new();
        for resource in guard.exported() {
            for scope in resource.scope_metrics() {
                for metric in scope.metrics() {
                    if !metric.name().starts_with("xmpp.remote_owner_mirror.") {
                        continue;
                    }
                    if let opentelemetry_sdk::metrics::data::AggregatedMetrics::F64(
                        opentelemetry_sdk::metrics::data::MetricData::Histogram(histogram),
                    ) = metric.data()
                    {
                        for point in histogram.data_points() {
                            assert_eq!(
                                point.attributes().count(),
                                0,
                                "backlog samples carry no entity labels"
                            );
                            *sums.entry(metric.name().to_string()).or_insert(0.0) += point.sum();
                        }
                    }
                }
            }
        }
        assert_eq!(sums["xmpp.remote_owner_mirror.inventory"], 1024.0);
        assert_eq!(sums["xmpp.remote_owner_mirror.pending"], 64.0);
        assert_eq!(sums["xmpp.remote_owner_mirror.oldest_pending_age"], 12.0);
        assert_eq!(
            guard.histogram_bounds("xmpp.remote_owner_mirror.oldest_pending_age"),
            Some(PENDING_AGE_BUCKETS.to_vec())
        );
        crate::telemetry::reliability::record_janitor_sweep(
            Janitor::RemoteOwnerMirror,
            SweepOutcome::Deferred,
        );
        assert_eq!(
            guard.counter_sum(
                "waddle.janitor.sweeps",
                &[("janitor", "remote_owner_mirror"), ("outcome", "deferred")]
            ),
            Some(1)
        );
    }
}
