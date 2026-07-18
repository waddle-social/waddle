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

/// Record a durable push candidate in the legacy, reliability, and typed
/// pipeline families.
pub fn increment_push_candidate_created() {
    crate::prometheus::increment_push_candidate_created();
    crate::counter_add!(
        "xmpp.push.candidate_created",
        "{notification}",
        "XEP-0357 notification candidates inserted into the durable pipeline.",
        1,
    );
    super::push_pipeline::increment_candidate_created();
}

/// Record duplicate candidate coalescing in the legacy, reliability, and
/// typed pipeline families.
pub fn increment_push_candidate_coalesced() {
    crate::prometheus::increment_push_candidate_coalesced();
    crate::counter_add!(
        "xmpp.push.candidate_coalesced",
        "{notification}",
        "Duplicate XEP-0357 notification candidates coalesced at insertion.",
        1,
    );
    super::push_pipeline::increment_coalesced();
}

/// Record Push Service acceptance in the legacy, reliability, and typed
/// pipeline families.
pub fn increment_push_outbox_published() {
    crate::prometheus::increment_push_outbox_published();
    crate::counter_add!(
        "xmpp.push.outbox_published",
        "{notification}",
        "XEP-0357 notification outbox jobs accepted by the Push Service.",
        1,
    );
    super::push_pipeline::increment_published();
}
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
    super::push_pipeline::increment_retry_scheduled();
}

/// Record terminal outbox dead-lettering in the legacy, reliability, and
/// typed pipeline families.
pub fn increment_push_outbox_dead_lettered() {
    crate::prometheus::increment_push_outbox_dead_lettered();
    crate::counter_add!(
        "xmpp.push.outbox_dead_lettered",
        "{notification}",
        "XEP-0357 notification outbox jobs terminally dead-lettered.",
        1,
    );
    super::push_pipeline::increment_dead_lettered();
}

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
    super::push_pipeline::increment_suppressed();
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
        assert_eq!(
            guard.counter_sum("waddle.push.pipeline", &[("stage", "suppressed")]),
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

    /// One dual-emit contract case: drive `emit`, then expect the OTel
    /// counter `otel` and the legacy text line `legacy` to both read
    /// `expected`.
    struct DualEmitCase {
        otel: &'static str,
        legacy: &'static str,
        expected: u64,
        emit: fn(),
    }

    /// #1330 acceptance: metric-reader-seam coverage for EVERY migrated
    /// counter, not a per-family sample. Each helper is driven once and
    /// both halves of the dual emit are asserted — the exported OTel
    /// sample under the new `xmpp.*` name and the legacy `waddle_*`
    /// text line the recording-rule aliases will later map from.
    #[tokio::test]
    async fn every_migrated_counter_dual_emits() {
        let cases: &[DualEmitCase] = &[
            // Stream-management family.
            DualEmitCase {
                otel: "xmpp.sm.unacked_evicted",
                legacy: "waddle_sm_unacked_evicted_total",
                expected: 1,
                emit: increment_sm_unacked_evicted,
            },
            DualEmitCase {
                otel: "xmpp.sm.promotion_storage_failed",
                legacy: "waddle_sm_promotion_storage_failed_total",
                expected: 3,
                emit: || add_sm_promotion_storage_failed(3),
            },
            DualEmitCase {
                otel: "xmpp.sm.promotion_not_promotable",
                legacy: "waddle_sm_promotion_not_promotable_total",
                expected: 1,
                emit: increment_sm_promotion_not_promotable,
            },
            DualEmitCase {
                otel: "xmpp.sm.promotion_blocklist_failed",
                legacy: "waddle_sm_promotion_blocklist_failed_total",
                expected: 1,
                emit: increment_sm_promotion_blocklist_failed,
            },
            DualEmitCase {
                otel: "xmpp.sm.promotion_dead_lettered",
                legacy: "waddle_sm_promotion_dead_lettered_total",
                expected: 1,
                emit: increment_sm_promotion_dead_lettered,
            },
            DualEmitCase {
                otel: "xmpp.sm.drain_timeout",
                legacy: "waddle_sm_drain_timeout_total",
                expected: 1,
                emit: increment_sm_drain_timeout,
            },
            DualEmitCase {
                otel: "xmpp.sm.resume_window_clamped",
                legacy: "waddle_sm_resume_window_clamped_total",
                expected: 1,
                emit: increment_sm_resume_window_clamped,
            },
            DualEmitCase {
                otel: "xmpp.sm.send_window_pauses",
                legacy: "waddle_sm_send_window_pauses_total",
                expected: 1,
                emit: increment_sm_send_window_pause,
            },
            DualEmitCase {
                otel: "xmpp.sm.send_window_pause_timeouts",
                legacy: "waddle_sm_send_window_pause_timeouts_total",
                expected: 1,
                emit: increment_sm_send_window_pause_timeout,
            },
            DualEmitCase {
                otel: "xmpp.sm.detached_unacked_evicted",
                legacy: "waddle_sm_detached_unacked_evicted_total",
                expected: 1,
                emit: increment_sm_detached_unacked_evicted,
            },
            // Push family (reason-carrying helpers asserted separately below).
            DualEmitCase {
                otel: "xmpp.push.candidate_created",
                legacy: "waddle_push_candidate_created_total",
                expected: 1,
                emit: increment_push_candidate_created,
            },
            DualEmitCase {
                otel: "xmpp.push.candidate_coalesced",
                legacy: "waddle_push_candidate_coalesced_total",
                expected: 1,
                emit: increment_push_candidate_coalesced,
            },
            DualEmitCase {
                otel: "xmpp.push.outbox_published",
                legacy: "waddle_push_outbox_published_total",
                expected: 1,
                emit: increment_push_outbox_published,
            },
            DualEmitCase {
                otel: "xmpp.push.outbox_dead_lettered",
                legacy: "waddle_push_outbox_dead_lettered_total",
                expected: 1,
                emit: increment_push_outbox_dead_lettered,
            },
            // Pending-delivery family.
            DualEmitCase {
                otel: "xmpp.pending_delivery.quota_exceeded",
                legacy: "waddle_pending_delivery_quota_exceeded_total",
                expected: 1,
                emit: increment_pending_delivery_quota_exceeded,
            },
            DualEmitCase {
                otel: "xmpp.pending_delivery.orphan_claims_released",
                legacy: "waddle_pending_delivery_orphan_claims_released_total",
                expected: 4,
                emit: || add_pending_delivery_orphan_claims_released(4),
            },
            DualEmitCase {
                otel: "xmpp.pending_delivery.aged_out",
                legacy: "waddle_pending_delivery_aged_out_total",
                expected: 2,
                emit: || add_pending_delivery_aged_out(2),
            },
            DualEmitCase {
                otel: "xmpp.pending_delivery.unresolved_poison_pill",
                legacy: "waddle_pending_delivery_unresolved_poison_pill_total",
                expected: 1,
                emit: increment_pending_delivery_unresolved_poison_pill,
            },
            DualEmitCase {
                otel: "xmpp.pending_delivery.archive_lookup_transient_failure",
                legacy: "waddle_pending_delivery_archive_lookup_transient_failure_total",
                expected: 1,
                emit: increment_pending_delivery_archive_lookup_transient_failure,
            },
            DualEmitCase {
                otel: "xmpp.pending.flush_batches",
                legacy: "waddle_pending_flush_batches_total",
                expected: 2,
                emit: || add_pending_flush_batches(2),
            },
            DualEmitCase {
                otel: "xmpp.pending.flush_rows_pushed",
                legacy: "waddle_pending_flush_rows_pushed_total",
                expected: 5,
                emit: || add_pending_flush_rows_pushed(5),
            },
            // Broadcast family.
            DualEmitCase {
                otel: "xmpp.broadcast.delivered",
                legacy: "waddle_broadcast_delivered_total",
                expected: 1,
                emit: increment_broadcast_delivered,
            },
            DualEmitCase {
                otel: "xmpp.broadcast.not_connected",
                legacy: "waddle_broadcast_not_connected_total",
                expected: 1,
                emit: increment_broadcast_not_connected,
            },
            DualEmitCase {
                otel: "xmpp.broadcast.dropped_full",
                legacy: "waddle_broadcast_dropped_full_total",
                expected: 1,
                emit: increment_broadcast_dropped_full,
            },
            DualEmitCase {
                otel: "xmpp.broadcast.dropped_closed",
                legacy: "waddle_broadcast_dropped_closed_total",
                expected: 1,
                emit: increment_broadcast_dropped_closed,
            },
            // Delivery-loss family.
            DualEmitCase {
                otel: "xmpp.delivery.terminal_error_drop",
                legacy: "waddle_delivery_terminal_error_drop_total",
                expected: 1,
                emit: increment_delivery_terminal_error_drop,
            },
            DualEmitCase {
                otel: "xmpp.delivery.retry_exhausted_drop",
                legacy: "waddle_delivery_retry_exhausted_drop_total",
                expected: 1,
                emit: increment_delivery_retry_exhausted_drop,
            },
            DualEmitCase {
                otel: "xmpp.resolver.affiliation_sync_capacity_drop",
                legacy: "waddle_resolver_affiliation_sync_capacity_drop_total",
                expected: 1,
                emit: increment_resolver_affiliation_sync_capacity_drop,
            },
            DualEmitCase {
                otel: "xmpp.user_actor.reaped",
                legacy: "waddle_user_actor_reaped_total",
                expected: 1,
                emit: increment_user_actor_reaped,
            },
            // DND.
            DualEmitCase {
                otel: "xmpp.dnd.projection_read_errored",
                legacy: "waddle_dnd_projection_read_errored_total",
                expected: 1,
                emit: increment_dnd_projection_read_errored,
            },
        ];

        let guard = setup().await;
        for case in cases {
            (case.emit)();
        }
        increment_push_outbox_retry_scheduled(super::PushRetryReason::Unknown);
        increment_push_suppressed(PushSuppressReason::WaddleDnd);

        let rendered = crate::prometheus::render_metrics();
        for case in cases {
            assert_eq!(
                guard.counter_sum(case.otel, &[]),
                Some(case.expected),
                "OTel sample missing or wrong for {}",
                case.otel
            );
            assert!(
                rendered.contains(&format!("{} {}\n", case.legacy, case.expected)),
                "legacy text line missing or wrong for {}",
                case.legacy
            );
        }
        // The two reason-carrying helpers keep their label shape on both
        // halves.
        assert_eq!(
            guard.counter_sum("xmpp.push.outbox_retry_scheduled", &[("reason", "unknown")]),
            Some(1)
        );
        assert!(
            rendered.contains("waddle_push_outbox_retry_scheduled_total{reason=\"unknown\"} 1\n")
        );
        assert_eq!(
            guard.counter_sum("xmpp.push.suppressed", &[("reason", "waddle_dnd")]),
            Some(1)
        );
        assert!(rendered.contains("waddle_push_suppressed_total{reason=\"waddle_dnd\"} 1\n"));
        for stage in ["suppressed", "retry_scheduled", "dead_lettered"] {
            assert_eq!(
                guard.counter_sum("waddle.push.pipeline", &[("stage", stage)]),
                Some(1),
                "typed pipeline sample missing or wrong for {stage}"
            );
        }
    }
}
