//! Reliability counters dual-emitted to the frozen legacy text renderer and
//! their OpenTelemetry successors.
//!
//! OTel names deliberately start with `xmpp`, not `waddle`: after Prometheus
//! export, `xmpp.<rest>` becomes `xmpp_<rest>_total`, leaving the live
//! `waddle_<rest>_total` scrape distinct until recording-rule aliases cut over.

use super::attributes::{Janitor, PushRetryReason, PushSuppressReason, SweepOutcome};

macro_rules! dual_increment {
    ($helper:ident, $legacy:path, $name:literal, $unit:literal, $description:literal) => {
        #[doc = concat!("Increment legacy reliability counter and OTel `", $name, "`.")]
        pub fn $helper() {
            $legacy();
            crate::counter_add!($name, $unit, $description, 1);
        }
    };
}

macro_rules! dual_add {
    ($helper:ident, $legacy:path, $name:literal, $unit:literal, $description:literal) => {
        #[doc = concat!("Add to the legacy reliability counter and OTel `", $name, "`.")]
        pub fn $helper(count: u64) {
            $legacy(count);
            crate::counter_add!($name, $unit, $description, count);
        }
    };
}

dual_increment!(
    increment_sm_unacked_evicted,
    crate::prometheus::increment_sm_unacked_evicted,
    "xmpp.sm.unacked_evicted",
    "{stanza}",
    "XEP-0198 unacked stanzas evicted from an attached session replay window."
);
dual_add!(
    add_sm_promotion_storage_failed,
    crate::prometheus::add_sm_promotion_storage_failed,
    "xmpp.sm.promotion_storage_failed",
    "{stanza}",
    "Unacked stanzas whose XEP-0160 promotion storage write failed."
);
dual_increment!(
    increment_sm_promotion_not_promotable,
    crate::prometheus::increment_sm_promotion_not_promotable,
    "xmpp.sm.promotion_not_promotable",
    "{stanza}",
    "Unacked stanzas that were not XEP-0160 promotion candidates."
);
dual_increment!(
    increment_sm_promotion_blocklist_failed,
    crate::prometheus::increment_sm_promotion_blocklist_failed,
    "xmpp.sm.promotion_blocklist_failed",
    "{session}",
    "XEP-0198 promotion sessions skipped because blocklist loading failed."
);
dual_increment!(
    increment_sm_promotion_dead_lettered,
    crate::prometheus::increment_sm_promotion_dead_lettered,
    "xmpp.sm.promotion_dead_lettered",
    "{session}",
    "XEP-0198 promotion sessions dead-lettered after exhausting retries."
);
dual_increment!(
    increment_sm_drain_timeout,
    crate::prometheus::increment_sm_drain_timeout,
    "xmpp.sm.drain_timeout",
    "{event}",
    "Graceful-shutdown XEP-0198 drains that exceeded their deadline."
);
dual_increment!(
    increment_sm_resume_window_clamped,
    crate::prometheus::increment_sm_resume_window_clamped,
    "xmpp.sm.resume_window_clamped",
    "{session}",
    "XEP-0198 sessions whose requested resume window was clamped."
);
dual_increment!(
    increment_sm_send_window_pause,
    crate::prometheus::increment_sm_send_window_pause,
    "xmpp.sm.send_window_pauses",
    "{event}",
    "XEP-0198 wire-write pauses engaged at the send-window high watermark."
);
dual_increment!(
    increment_sm_send_window_pause_timeout,
    crate::prometheus::increment_sm_send_window_pause_timeout,
    "xmpp.sm.send_window_pause_timeouts",
    "{event}",
    "XEP-0198 send-window pauses that exceeded their acknowledgement deadline."
);
dual_increment!(
    increment_sm_detached_unacked_evicted,
    crate::prometheus::increment_sm_detached_unacked_evicted,
    "xmpp.sm.detached_unacked_evicted",
    "{stanza}",
    "Unacked stanzas evicted from a detached XEP-0198 session replay window."
);

dual_increment!(
    increment_push_candidate_created,
    crate::prometheus::increment_push_candidate_created,
    "xmpp.push.candidate_created",
    "{notification}",
    "XEP-0357 notification candidates inserted into the durable pipeline."
);
dual_increment!(
    increment_push_candidate_coalesced,
    crate::prometheus::increment_push_candidate_coalesced,
    "xmpp.push.candidate_coalesced",
    "{notification}",
    "Duplicate XEP-0357 notification candidates coalesced at insertion."
);
dual_increment!(
    increment_push_outbox_published,
    crate::prometheus::increment_push_outbox_published,
    "xmpp.push.outbox_published",
    "{notification}",
    "XEP-0357 notification outbox jobs accepted by the Push Service."
);
/// Dual-emit for outbox retry scheduling. Hand-written (not
/// `dual_increment!`) because the OTel successor must carry the same
/// `reason` label shape the legacy text family renders
/// (`waddle_push_outbox_retry_scheduled_total{reason="unknown"}`) —
/// otherwise PromQL filtering or grouping by `reason` would stop
/// matching at the alias cutover.
pub fn increment_push_outbox_retry_scheduled(reason: PushRetryReason) {
    crate::prometheus::increment_push_outbox_retry_scheduled();
    crate::counter_add!(
        "xmpp.push.outbox_retry_scheduled",
        "{notification}",
        "XEP-0357 notification outbox jobs scheduled for retry.",
        1,
        reason,
    );
}
dual_increment!(
    increment_push_outbox_dead_lettered,
    crate::prometheus::increment_push_outbox_dead_lettered,
    "xmpp.push.outbox_dead_lettered",
    "{notification}",
    "XEP-0357 notification outbox jobs terminally dead-lettered."
);

/// Record one typed push suppression in both telemetry systems.
pub fn increment_push_suppressed(reason: PushSuppressReason) {
    crate::prometheus::increment_push_suppressed(reason);
    crate::counter_add!(
        "xmpp.push.suppressed",
        "{notification}",
        "XEP-0357 notification candidates suppressed by a bounded policy reason.",
        1,
        reason,
    );
}

// No dual helper for the legacy unknown-reason catch-all: the sealed
// `PushSuppressReason` enum makes an unmapped reason a compile error,
// so the catch-all is structurally unreachable. Its frozen text
// counter stays rendered (permanently 0) until the contract PR
// deletes the family.

dual_increment!(
    increment_pending_delivery_quota_exceeded,
    crate::prometheus::increment_pending_delivery_quota_exceeded,
    "xmpp.pending_delivery.quota_exceeded",
    "{message}",
    "Offline messages rejected because the recipient pending-delivery quota was full."
);
dual_add!(
    add_pending_delivery_orphan_claims_released,
    crate::prometheus::add_pending_delivery_orphan_claims_released,
    "xmpp.pending_delivery.orphan_claims_released",
    "{claim}",
    "Orphaned pending-delivery claims released by the claim janitor."
);
dual_add!(
    add_pending_delivery_aged_out,
    crate::prometheus::add_pending_delivery_aged_out,
    "xmpp.pending_delivery.aged_out",
    "{row}",
    "Pending-delivery rows removed after exceeding the configured maximum age."
);
dual_increment!(
    increment_pending_delivery_unresolved_poison_pill,
    crate::prometheus::increment_pending_delivery_unresolved_poison_pill,
    "xmpp.pending_delivery.unresolved_poison_pill",
    "{row}",
    "Pending-delivery rows dropped because their archived payload could not be resolved."
);
dual_increment!(
    increment_pending_delivery_archive_lookup_transient_failure,
    crate::prometheus::increment_pending_delivery_archive_lookup_transient_failure,
    "xmpp.pending_delivery.archive_lookup_transient_failure",
    "{event}",
    "Pending-delivery flushes aborted by a transient archive lookup failure."
);
dual_add!(
    add_pending_flush_batches,
    crate::prometheus::add_pending_flush_batches,
    "xmpp.pending.flush_batches",
    "{event}",
    "Pending-delivery batches drained by offline-message flushes."
);
dual_add!(
    add_pending_flush_rows_pushed,
    crate::prometheus::add_pending_flush_rows_pushed,
    "xmpp.pending.flush_rows_pushed",
    "{row}",
    "Pending-delivery rows pushed to recovering resources."
);

dual_increment!(
    increment_broadcast_delivered,
    crate::prometheus::increment_broadcast_delivered,
    "xmpp.broadcast.delivered",
    "{stanza}",
    "Non-blocking broadcast attempts enqueued to a recipient."
);
dual_increment!(
    increment_broadcast_not_connected,
    crate::prometheus::increment_broadcast_not_connected,
    "xmpp.broadcast.not_connected",
    "{stanza}",
    "Non-blocking broadcast attempts with no connected recipient."
);
dual_increment!(
    increment_broadcast_dropped_full,
    crate::prometheus::increment_broadcast_dropped_full,
    "xmpp.broadcast.dropped_full",
    "{stanza}",
    "Non-blocking broadcast attempts dropped because the recipient channel was full."
);
dual_increment!(
    increment_broadcast_dropped_closed,
    crate::prometheus::increment_broadcast_dropped_closed,
    "xmpp.broadcast.dropped_closed",
    "{stanza}",
    "Non-blocking broadcast attempts dropped because the recipient channel was closed."
);

dual_increment!(
    increment_delivery_terminal_error_drop,
    crate::prometheus::increment_delivery_terminal_error_drop,
    "xmpp.delivery.terminal_error_drop",
    "{stanza}",
    "Actor-path deliveries dropped after an enqueue-uncertain terminal error."
);
dual_increment!(
    increment_delivery_retry_exhausted_drop,
    crate::prometheus::increment_delivery_retry_exhausted_drop,
    "xmpp.delivery.retry_exhausted_drop",
    "{stanza}",
    "Deliveries dropped after bounded full-channel retries were exhausted."
);
dual_increment!(
    increment_resolver_affiliation_sync_capacity_drop,
    crate::prometheus::increment_resolver_affiliation_sync_capacity_drop,
    "xmpp.resolver.affiliation_sync_capacity_drop",
    "{event}",
    "Resolver-affiliation synchronization jobs dropped at scheduler capacity."
);
dual_increment!(
    increment_user_actor_reaped,
    crate::prometheus::increment_user_actor_reaped,
    "xmpp.user_actor.reaped",
    "{session}",
    "Empty user actors removed by the periodic reaper."
);

dual_increment!(
    increment_dnd_projection_read_errored,
    crate::prometheus::increment_dnd_projection_read_errored,
    "xmpp.dnd.projection_read_errored",
    "{event}",
    "DND projection reads that failed open to inactive."
);

/// Record one periodic janitor sweep outcome.
pub fn record_janitor_sweep(janitor: Janitor, outcome: SweepOutcome) {
    crate::counter_add!(
        "waddle.janitor.sweeps",
        "{sweep}",
        "Periodic janitor sweep ticks by janitor and outcome.",
        1,
        janitor,
        outcome,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::attributes::MetricAttribute;

    async fn setup() -> crate::telemetry::test_support::MetricsTestGuard {
        let guard = crate::telemetry::test_support::acquire().await;
        crate::prometheus::reset_metrics_for_test();
        guard
    }

    #[tokio::test]
    async fn sm_helper_dual_emits() {
        let guard = setup().await;
        add_sm_promotion_storage_failed(3);
        assert_eq!(
            guard.counter_sum("xmpp.sm.promotion_storage_failed", &[]),
            Some(3)
        );
        assert!(crate::prometheus::render_metrics()
            .contains("waddle_sm_promotion_storage_failed_total 3"));
    }

    #[tokio::test]
    async fn push_helper_dual_emits_with_typed_reason() {
        let guard = setup().await;
        increment_push_suppressed(PushSuppressReason::Xep0492Never);
        assert_eq!(
            guard.counter_sum("xmpp.push.suppressed", &[("reason", "xep0492_never")]),
            Some(1)
        );
        assert!(crate::prometheus::render_metrics()
            .contains("waddle_push_suppressed_total{reason=\"xep0492_never\"} 1"));
    }

    #[tokio::test]
    async fn pending_delivery_helper_dual_emits() {
        let guard = setup().await;
        add_pending_delivery_aged_out(4);
        assert_eq!(
            guard.counter_sum("xmpp.pending_delivery.aged_out", &[]),
            Some(4)
        );
        assert!(crate::prometheus::render_metrics()
            .contains("waddle_pending_delivery_aged_out_total 4"));
    }

    #[tokio::test]
    async fn broadcast_helper_dual_emits() {
        let guard = setup().await;
        increment_broadcast_dropped_full();
        assert_eq!(
            guard.counter_sum("xmpp.broadcast.dropped_full", &[]),
            Some(1)
        );
        assert!(
            crate::prometheus::render_metrics().contains("waddle_broadcast_dropped_full_total 1")
        );
    }

    #[tokio::test]
    async fn delivery_loss_helper_dual_emits() {
        let guard = setup().await;
        increment_delivery_retry_exhausted_drop();
        assert_eq!(
            guard.counter_sum("xmpp.delivery.retry_exhausted_drop", &[]),
            Some(1)
        );
        assert!(crate::prometheus::render_metrics()
            .contains("waddle_delivery_retry_exhausted_drop_total 1"));
    }

    #[tokio::test]
    async fn dnd_helper_dual_emits() {
        let guard = setup().await;
        increment_dnd_projection_read_errored();
        assert_eq!(
            guard.counter_sum("xmpp.dnd.projection_read_errored", &[]),
            Some(1)
        );
        assert!(crate::prometheus::render_metrics()
            .contains("waddle_dnd_projection_read_errored_total 1"));
    }

    #[tokio::test]
    async fn janitor_completed_heartbeat_records() {
        let guard = setup().await;
        record_janitor_sweep(Janitor::RoomDormancy, SweepOutcome::Completed);
        assert_eq!(
            guard.counter_sum(
                "waddle.janitor.sweeps",
                &[("janitor", "room_dormancy"), ("outcome", "completed")]
            ),
            Some(1)
        );
    }

    #[tokio::test]
    async fn janitor_failed_heartbeat_records() {
        let guard = setup().await;
        record_janitor_sweep(Janitor::AuthState, SweepOutcome::Failed);
        assert_eq!(
            guard.counter_sum(
                "waddle.janitor.sweeps",
                &[("janitor", "auth_state"), ("outcome", "failed")]
            ),
            Some(1)
        );
    }

    #[test]
    fn push_suppress_reason_exactly_covers_legacy_reason_list() {
        let enum_values: Vec<_> = PushSuppressReason::ALL
            .iter()
            .map(MetricAttribute::value)
            .collect();
        assert_eq!(enum_values, crate::prometheus::PUSH_SUPPRESSED_REASONS);
    }
}
