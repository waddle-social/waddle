//! Reliability counters, emitted to the OTel meter family only.
//!
//! Contract phase of #1330: the legacy `waddle_*` text half of the old
//! dual-emit is gone; every retired name keeps answering through the
//! Mimir recording-rule aliases in
//! `infrastructure/waddle.cloud/rules/mimir/waddle-reliability-aliases.yaml`.
//! OTel names deliberately start with `xmpp`, not `waddle`, so the
//! translated `xmpp_<rest>_total` series never collided with the legacy
//! scrape during the dual-emit release.

use super::attributes::{Janitor, PushRetryReason, PushSuppressReason, SweepOutcome};

macro_rules! reliability_increment {
    ($helper:ident, $name:literal, $unit:literal, $description:literal) => {
        #[doc = concat!("Increment the OTel reliability counter `", $name, "`.")]
        pub fn $helper() {
            crate::counter_add!($name, $unit, $description, 1);
        }
    };
}

macro_rules! reliability_add {
    ($helper:ident, $name:literal, $unit:literal, $description:literal) => {
        #[doc = concat!("Add to the OTel reliability counter `", $name, "`.")]
        pub fn $helper(count: u64) {
            crate::counter_add!($name, $unit, $description, count);
        }
    };
}

reliability_increment!(
    increment_sm_unacked_evicted,
    "xmpp.sm.unacked_evicted",
    "{stanza}",
    "XEP-0198 unacked stanzas evicted from an attached session replay window."
);
reliability_add!(
    add_sm_promotion_storage_failed,
    "xmpp.sm.promotion_storage_failed",
    "{stanza}",
    "Unacked stanzas whose XEP-0160 promotion storage write failed."
);
reliability_increment!(
    increment_sm_promotion_not_promotable,
    "xmpp.sm.promotion_not_promotable",
    "{stanza}",
    "Unacked stanzas that were not XEP-0160 promotion candidates."
);
reliability_increment!(
    increment_sm_promotion_blocklist_failed,
    "xmpp.sm.promotion_blocklist_failed",
    "{session}",
    "XEP-0198 promotion sessions skipped because blocklist loading failed."
);
reliability_increment!(
    increment_sm_promotion_dead_lettered,
    "xmpp.sm.promotion_dead_lettered",
    "{session}",
    "XEP-0198 promotion sessions dead-lettered after exhausting retries."
);
reliability_increment!(
    increment_sm_drain_timeout,
    "xmpp.sm.drain_timeout",
    "{event}",
    "Graceful-shutdown XEP-0198 drains that exceeded their deadline."
);
reliability_increment!(
    increment_sm_resume_window_clamped,
    "xmpp.sm.resume_window_clamped",
    "{session}",
    "XEP-0198 sessions whose requested resume window was clamped."
);
reliability_increment!(
    increment_sm_send_window_pause,
    "xmpp.sm.send_window_pauses",
    "{event}",
    "XEP-0198 wire-write pauses engaged at the send-window high watermark."
);
reliability_increment!(
    increment_sm_send_window_pause_timeout,
    "xmpp.sm.send_window_pause_timeouts",
    "{event}",
    "XEP-0198 send-window pauses that exceeded their acknowledgement deadline."
);
reliability_increment!(
    increment_sm_detached_unacked_evicted,
    "xmpp.sm.detached_unacked_evicted",
    "{stanza}",
    "Unacked stanzas evicted from a detached XEP-0198 session replay window."
);

/// Record a durable push candidate in the reliability and typed pipeline families.
pub fn increment_push_candidate_created() {
    crate::counter_add!(
        "xmpp.push.candidate_created",
        "{notification}",
        "XEP-0357 notification candidates inserted into the durable pipeline.",
        1,
    );
    super::push_pipeline::increment_candidate_created();
}

/// Record duplicate candidate coalescing in the reliability and typed pipeline families.
pub fn increment_push_candidate_coalesced() {
    crate::counter_add!(
        "xmpp.push.candidate_coalesced",
        "{notification}",
        "Duplicate XEP-0357 notification candidates coalesced at insertion.",
        1,
    );
    super::push_pipeline::increment_coalesced();
}

/// Record Push Service acceptance in the reliability and typed pipeline families.
pub fn increment_push_outbox_published() {
    crate::counter_add!(
        "xmpp.push.outbox_published",
        "{notification}",
        "XEP-0357 notification outbox jobs accepted by the Push Service.",
        1,
    );
    super::push_pipeline::increment_published();
}
/// Hand-written (not `reliability_increment!`) because the counter
/// carries the `reason` label the alias recording rule preserves
/// (`waddle_push_outbox_retry_scheduled_total{reason=...}`).
pub fn increment_push_outbox_retry_scheduled(reason: PushRetryReason) {
    crate::counter_add!(
        "xmpp.push.outbox_retry_scheduled",
        "{notification}",
        "XEP-0357 notification outbox jobs scheduled for retry.",
        1,
        reason,
    );
    super::push_pipeline::increment_retry_scheduled();
}

/// Record terminal outbox dead-lettering in the reliability and typed pipeline families.
pub fn increment_push_outbox_dead_lettered() {
    crate::counter_add!(
        "xmpp.push.outbox_dead_lettered",
        "{notification}",
        "XEP-0357 notification outbox jobs terminally dead-lettered.",
        1,
    );
    super::push_pipeline::increment_dead_lettered();
}

/// Record one typed push suppression in the reliability and typed pipeline families.
pub fn increment_push_suppressed(reason: PushSuppressReason) {
    crate::counter_add!(
        "xmpp.push.suppressed",
        "{notification}",
        "XEP-0357 notification candidates suppressed by a bounded policy reason.",
        1,
        reason,
    );
    super::push_pipeline::increment_suppressed();
}

// The legacy unknown-reason catch-all family is gone with the text
// renderer: the sealed `PushSuppressReason` enum makes an unmapped
// reason a compile error, so it was structurally unreachable and
// permanently 0. It is retired without an alias (rules README).

reliability_increment!(
    increment_pending_delivery_quota_exceeded,
    "xmpp.pending_delivery.quota_exceeded",
    "{message}",
    "Offline messages rejected because the recipient pending-delivery quota was full."
);
reliability_add!(
    add_pending_delivery_orphan_claims_released,
    "xmpp.pending_delivery.orphan_claims_released",
    "{claim}",
    "Orphaned pending-delivery claims released by the claim janitor."
);
reliability_add!(
    add_pending_delivery_aged_out,
    "xmpp.pending_delivery.aged_out",
    "{row}",
    "Pending-delivery rows removed after exceeding the configured maximum age."
);
reliability_increment!(
    increment_pending_delivery_unresolved_poison_pill,
    "xmpp.pending_delivery.unresolved_poison_pill",
    "{row}",
    "Pending-delivery rows dropped because their archived payload could not be resolved."
);
reliability_increment!(
    increment_pending_delivery_archive_lookup_transient_failure,
    "xmpp.pending_delivery.archive_lookup_transient_failure",
    "{event}",
    "Pending-delivery flushes aborted by a transient archive lookup failure."
);
reliability_add!(
    add_pending_flush_batches,
    "xmpp.pending.flush_batches",
    "{event}",
    "Pending-delivery batches drained by offline-message flushes."
);
reliability_add!(
    add_pending_flush_rows_pushed,
    "xmpp.pending.flush_rows_pushed",
    "{row}",
    "Pending-delivery rows pushed to recovering resources."
);

reliability_increment!(
    increment_broadcast_delivered,
    "xmpp.broadcast.delivered",
    "{stanza}",
    "Non-blocking broadcast attempts enqueued to a recipient."
);
reliability_increment!(
    increment_broadcast_not_connected,
    "xmpp.broadcast.not_connected",
    "{stanza}",
    "Non-blocking broadcast attempts with no connected recipient."
);
reliability_increment!(
    increment_broadcast_dropped_full,
    "xmpp.broadcast.dropped_full",
    "{stanza}",
    "Non-blocking broadcast attempts dropped because the recipient channel was full."
);
reliability_increment!(
    increment_broadcast_dropped_closed,
    "xmpp.broadcast.dropped_closed",
    "{stanza}",
    "Non-blocking broadcast attempts dropped because the recipient channel was closed."
);

reliability_increment!(
    increment_delivery_terminal_error_drop,
    "xmpp.delivery.terminal_error_drop",
    "{stanza}",
    "Actor-path deliveries dropped after an enqueue-uncertain terminal error."
);
reliability_increment!(
    increment_delivery_retry_exhausted_drop,
    "xmpp.delivery.retry_exhausted_drop",
    "{stanza}",
    "Deliveries dropped after bounded full-channel retries were exhausted."
);
reliability_increment!(
    increment_resolver_affiliation_sync_capacity_drop,
    "xmpp.resolver.affiliation_sync_capacity_drop",
    "{event}",
    "Resolver-affiliation synchronization jobs dropped at scheduler capacity."
);
reliability_increment!(
    increment_user_actor_reaped,
    "xmpp.user_actor.reaped",
    "{session}",
    "Empty user actors removed by the periodic reaper."
);

reliability_increment!(
    increment_dnd_projection_read_errored,
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

    async fn setup() -> crate::telemetry::test_support::MetricsTestGuard {
        crate::telemetry::test_support::acquire().await
    }

    #[tokio::test]
    async fn sm_helper_emits() {
        let guard = setup().await;
        add_sm_promotion_storage_failed(3);
        assert_eq!(
            guard.counter_sum("xmpp.sm.promotion_storage_failed", &[]),
            Some(3)
        );
    }

    #[tokio::test]
    async fn push_helper_emits_with_typed_reason() {
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
    }

    #[tokio::test]
    async fn pending_delivery_helper_emits() {
        let guard = setup().await;
        add_pending_delivery_aged_out(4);
        assert_eq!(
            guard.counter_sum("xmpp.pending_delivery.aged_out", &[]),
            Some(4)
        );
    }

    #[tokio::test]
    async fn broadcast_helper_emits() {
        let guard = setup().await;
        increment_broadcast_dropped_full();
        assert_eq!(
            guard.counter_sum("xmpp.broadcast.dropped_full", &[]),
            Some(1)
        );
    }

    #[tokio::test]
    async fn delivery_loss_helper_emits() {
        let guard = setup().await;
        increment_delivery_retry_exhausted_drop();
        assert_eq!(
            guard.counter_sum("xmpp.delivery.retry_exhausted_drop", &[]),
            Some(1)
        );
    }

    #[tokio::test]
    async fn dnd_helper_emits() {
        let guard = setup().await;
        increment_dnd_projection_read_errored();
        assert_eq!(
            guard.counter_sum("xmpp.dnd.projection_read_errored", &[]),
            Some(1)
        );
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

    /// One emit contract case: drive `emit`, then expect the exported
    /// OTel counter `otel` to read `expected`.
    struct EmitCase {
        otel: &'static str,
        expected: u64,
        emit: fn(),
    }

    /// #1330 acceptance: metric-reader-seam coverage for EVERY migrated
    /// counter, not a per-family sample. Each helper is driven once and
    /// the exported OTel sample under the `xmpp.*` name is asserted —
    /// the retired `waddle_*` text names answer via the Mimir alias
    /// recording rules, which record from exactly these series.
    #[tokio::test]
    async fn every_migrated_counter_emits_otel() {
        let cases: &[EmitCase] = &[
            // Stream-management family.
            EmitCase {
                otel: "xmpp.sm.unacked_evicted",
                expected: 1,
                emit: increment_sm_unacked_evicted,
            },
            EmitCase {
                otel: "xmpp.sm.promotion_storage_failed",
                expected: 3,
                emit: || add_sm_promotion_storage_failed(3),
            },
            EmitCase {
                otel: "xmpp.sm.promotion_not_promotable",
                expected: 1,
                emit: increment_sm_promotion_not_promotable,
            },
            EmitCase {
                otel: "xmpp.sm.promotion_blocklist_failed",
                expected: 1,
                emit: increment_sm_promotion_blocklist_failed,
            },
            EmitCase {
                otel: "xmpp.sm.promotion_dead_lettered",
                expected: 1,
                emit: increment_sm_promotion_dead_lettered,
            },
            EmitCase {
                otel: "xmpp.sm.drain_timeout",
                expected: 1,
                emit: increment_sm_drain_timeout,
            },
            EmitCase {
                otel: "xmpp.sm.resume_window_clamped",
                expected: 1,
                emit: increment_sm_resume_window_clamped,
            },
            EmitCase {
                otel: "xmpp.sm.send_window_pauses",
                expected: 1,
                emit: increment_sm_send_window_pause,
            },
            EmitCase {
                otel: "xmpp.sm.send_window_pause_timeouts",
                expected: 1,
                emit: increment_sm_send_window_pause_timeout,
            },
            EmitCase {
                otel: "xmpp.sm.detached_unacked_evicted",
                expected: 1,
                emit: increment_sm_detached_unacked_evicted,
            },
            // Push family (reason-carrying helpers asserted separately below).
            EmitCase {
                otel: "xmpp.push.candidate_created",
                expected: 1,
                emit: increment_push_candidate_created,
            },
            EmitCase {
                otel: "xmpp.push.candidate_coalesced",
                expected: 1,
                emit: increment_push_candidate_coalesced,
            },
            EmitCase {
                otel: "xmpp.push.outbox_published",
                expected: 1,
                emit: increment_push_outbox_published,
            },
            EmitCase {
                otel: "xmpp.push.outbox_dead_lettered",
                expected: 1,
                emit: increment_push_outbox_dead_lettered,
            },
            // Pending-delivery family.
            EmitCase {
                otel: "xmpp.pending_delivery.quota_exceeded",
                expected: 1,
                emit: increment_pending_delivery_quota_exceeded,
            },
            EmitCase {
                otel: "xmpp.pending_delivery.orphan_claims_released",
                expected: 4,
                emit: || add_pending_delivery_orphan_claims_released(4),
            },
            EmitCase {
                otel: "xmpp.pending_delivery.aged_out",
                expected: 2,
                emit: || add_pending_delivery_aged_out(2),
            },
            EmitCase {
                otel: "xmpp.pending_delivery.unresolved_poison_pill",
                expected: 1,
                emit: increment_pending_delivery_unresolved_poison_pill,
            },
            EmitCase {
                otel: "xmpp.pending_delivery.archive_lookup_transient_failure",
                expected: 1,
                emit: increment_pending_delivery_archive_lookup_transient_failure,
            },
            EmitCase {
                otel: "xmpp.pending.flush_batches",
                expected: 2,
                emit: || add_pending_flush_batches(2),
            },
            EmitCase {
                otel: "xmpp.pending.flush_rows_pushed",
                expected: 5,
                emit: || add_pending_flush_rows_pushed(5),
            },
            // Broadcast family.
            EmitCase {
                otel: "xmpp.broadcast.delivered",
                expected: 1,
                emit: increment_broadcast_delivered,
            },
            EmitCase {
                otel: "xmpp.broadcast.not_connected",
                expected: 1,
                emit: increment_broadcast_not_connected,
            },
            EmitCase {
                otel: "xmpp.broadcast.dropped_full",
                expected: 1,
                emit: increment_broadcast_dropped_full,
            },
            EmitCase {
                otel: "xmpp.broadcast.dropped_closed",
                expected: 1,
                emit: increment_broadcast_dropped_closed,
            },
            // Delivery-loss family.
            EmitCase {
                otel: "xmpp.delivery.terminal_error_drop",
                expected: 1,
                emit: increment_delivery_terminal_error_drop,
            },
            EmitCase {
                otel: "xmpp.delivery.retry_exhausted_drop",
                expected: 1,
                emit: increment_delivery_retry_exhausted_drop,
            },
            EmitCase {
                otel: "xmpp.resolver.affiliation_sync_capacity_drop",
                expected: 1,
                emit: increment_resolver_affiliation_sync_capacity_drop,
            },
            EmitCase {
                otel: "xmpp.user_actor.reaped",
                expected: 1,
                emit: increment_user_actor_reaped,
            },
            // DND.
            EmitCase {
                otel: "xmpp.dnd.projection_read_errored",
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

        for case in cases {
            assert_eq!(
                guard.counter_sum(case.otel, &[]),
                Some(case.expected),
                "OTel sample missing or wrong for {}",
                case.otel
            );
        }
        // The two reason-carrying helpers keep the label shape the alias
        // recording rules preserve.
        assert_eq!(
            guard.counter_sum("xmpp.push.outbox_retry_scheduled", &[("reason", "unknown")]),
            Some(1)
        );
        assert_eq!(
            guard.counter_sum("xmpp.push.suppressed", &[("reason", "waddle_dnd")]),
            Some(1)
        );
        for stage in ["suppressed", "retry_scheduled", "dead_lettered"] {
            assert_eq!(
                guard.counter_sum("waddle.push.pipeline", &[("stage", stage)]),
                Some(1),
                "typed pipeline sample missing or wrong for {stage}"
            );
        }
    }
}
