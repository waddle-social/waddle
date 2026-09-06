//! Reliability counters, emitted to the OTel meter family only.
//!
//! Contract phase of #1330: the legacy `waddle_*` text half of the old
//! dual-emit is gone; every retired name keeps answering through the
//! Mimir recording-rule aliases in
//! `infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml`
//! (the `waddle-aliases-*` groups).
//! OTel names deliberately start with `xmpp`, not `waddle`, so the
//! translated `xmpp_<rest>_total` series never collided with the legacy
//! scrape during the dual-emit release.

use super::attributes::{
    IngressAliasOutcome, IngressDecisionClass, IngressGcOutcome, IngressUnresolvedEffectKind,
    Janitor, PushRetryReason, PushSuppressReason, SmAckOutcome, SmEvictionPath, SmResumeOutcome,
    SweepOutcome,
};

/// One table entry: a private `mod <helper> { fn add(count) }` holding
/// the single `counter_add!` call site (and therefore the single
/// `OnceLock` instrument) plus the public helper of the requested
/// shape. Startup registration and the emitting helper both go through
/// that one call site, so the zero-registration and the real increment
/// are guaranteed to be the same instrument.
macro_rules! reliability_counter {
    (increment $helper:ident, $name:literal, $unit:literal, $description:literal) => {
        #[doc = concat!("Increment the OTel reliability counter `", $name, "`.")]
        pub fn $helper() {
            $helper::add(1);
        }
    };
    (add $helper:ident, $name:literal, $unit:literal, $description:literal) => {
        #[doc = concat!("Add to the OTel reliability counter `", $name, "`.")]
        pub fn $helper(count: u64) {
            $helper::add(count);
        }
    };
}

/// The attribute-free reliability counters.
///
/// Adding a row here creates the emitting helper *and* enrolls the
/// counter in [`register_reliability_counters`] — there is no way to
/// add one of the two without the other, which is the point (#1436).
macro_rules! reliability_counters {
    ($(
        $shape:ident $helper:ident, $name:literal, $unit:literal, $description:literal;
    )*) => {
        $(
            // Modules live in the type namespace and the helpers in the
            // value namespace, so the emitting call site can share the
            // helper's name.
            mod $helper {
                pub fn add(count: u64) {
                    crate::counter_add!($name, $unit, $description, count);
                }
            }
            reliability_counter!($shape $helper, $name, $unit, $description);
        )*

        /// `add(0)` every table counter through its own call site.
        fn register_table_counters() {
            $( $helper::add(0); )*
        }

        /// Every table counter's metric name, for the registration test.
        #[cfg(test)]
        const TABLE_COUNTER_NAMES: &[&str] = &[$($name),*];
    };
}

fn add_sm_unacked_evicted(count: u64, path: SmEvictionPath) {
    crate::counter_add!(
        "xmpp.sm.unacked_evicted",
        "{stanza}",
        "XEP-0198 unacked stanzas evicted from an attached replay window, by path.",
        count,
        path,
    );
}

/// Increment an attached-stream replay-window eviction for the
/// enumerated outbound path that caused it.
pub fn increment_sm_unacked_evicted(path: SmEvictionPath) {
    add_sm_unacked_evicted(1, path);
}

fn add_sm_acks(count: u64, outcome: SmAckOutcome) {
    crate::counter_add!(
        "xmpp.sm.acks",
        "{ack}",
        "Client XEP-0198 <a/> acknowledgements received by outcome.",
        count,
        outcome,
    );
}

/// Count one client XEP-0198 `<a/>` acknowledgement by outcome.
pub fn increment_sm_ack(outcome: SmAckOutcome) {
    add_sm_acks(1, outcome);
}

fn add_sm_resume_results(count: u64, outcome: SmResumeOutcome) {
    crate::counter_add!(
        "xmpp.sm.resume.results",
        "{attempt}",
        "Terminal XEP-0198 resume attempts by outcome.",
        count,
        outcome,
    );
}

/// Count one terminal XEP-0198 resume attempt by outcome.
pub fn increment_sm_resume_result(outcome: SmResumeOutcome) {
    add_sm_resume_results(1, outcome);
}

fn add_sm_handled_progress_inner(count: u64) {
    crate::counter_add!(
        "xmpp.sm.handled_progress",
        "{stanza}",
        "XEP-0198 handled-count progress accepted from client acknowledgements.",
        count,
    );
}

/// Add the wrap-aware number of newly acknowledged stanzas from a client
/// XEP-0198 `<a/>`.
pub fn add_sm_handled_progress(count: u64) {
    add_sm_handled_progress_inner(count);
}

/// Record the latency from the oldest outstanding XEP-0198 `<r/>` request to
/// the first `<a/>` that covers its outbound frontier.
pub fn record_sm_request_latency_ms(latency_ms: f64) {
    crate::histogram_record!(
        "xmpp.sm.request.latency",
        "ms",
        "Latency from the oldest outstanding XEP-0198 <r/> request to the <a/> \
         that covers its requested outbound frontier. Later requests coalesce \
         while one remains outstanding.",
        latency_ms,
    );
}

reliability_counters! {
    add add_sm_promotion_storage_failed,
        "xmpp.sm.promotion_storage_failed",
        "{stanza}",
        "Unacked stanzas whose XEP-0160 promotion storage write failed.";
    increment increment_sm_promotion_not_promotable,
        "xmpp.sm.promotion_not_promotable",
        "{stanza}",
        "Unacked stanzas that were not XEP-0160 promotion candidates.";
    increment increment_sm_promotion_blocklist_failed,
        "xmpp.sm.promotion_blocklist_failed",
        "{session}",
        "XEP-0198 promotion sessions skipped because blocklist loading failed.";
    increment increment_sm_promotion_dead_lettered,
        "xmpp.sm.promotion_dead_lettered",
        "{session}",
        "XEP-0198 promotion sessions dead-lettered after exhausting retries.";
    increment increment_sm_drain_timeout,
        "xmpp.sm.drain_timeout",
        "{event}",
        "Graceful-shutdown XEP-0198 drains that exceeded their deadline.";
    increment increment_sm_resume_window_clamped,
        "xmpp.sm.resume_window_clamped",
        "{session}",
        "XEP-0198 sessions whose requested resume window was clamped.";
    increment increment_sm_send_window_pause,
        "xmpp.sm.send_window_pauses",
        "{event}",
        "XEP-0198 wire-write pauses engaged at the send-window high watermark.";
    increment increment_sm_send_window_pause_timeout,
        "xmpp.sm.send_window_pause_timeouts",
        "{event}",
        "XEP-0198 send-window pauses that exceeded their acknowledgement deadline.";
    increment increment_sm_detached_unacked_evicted,
        "xmpp.sm.detached_unacked_evicted",
        "{stanza}",
        "Unacked stanzas evicted from a detached XEP-0198 session replay window.";
    increment increment_pending_delivery_quota_exceeded,
        "xmpp.pending_delivery.quota_exceeded",
        "{message}",
        "Offline messages rejected because the recipient pending-delivery quota was full.";
    add add_pending_delivery_orphan_claims_released,
        "xmpp.pending_delivery.orphan_claims_released",
        "{claim}",
        "Orphaned pending-delivery claims released by the claim janitor.";
    add add_pending_delivery_aged_out,
        "xmpp.pending_delivery.aged_out",
        "{row}",
        "Pending-delivery rows removed after exceeding the configured maximum age.";
    increment increment_pending_delivery_unresolved_poison_pill,
        "xmpp.pending_delivery.unresolved_poison_pill",
        "{row}",
        "Pending-delivery rows dropped because their archived payload could not be resolved.";
    increment increment_pending_delivery_archive_lookup_transient_failure,
        "xmpp.pending_delivery.archive_lookup_transient_failure",
        "{event}",
        "Pending-delivery flushes aborted by a transient archive lookup failure.";
    add add_pending_flush_batches,
        "xmpp.pending.flush_batches",
        "{event}",
        "Pending-delivery batches drained by offline-message flushes.";
    add add_pending_flush_rows_pushed,
        "xmpp.pending.flush_rows_pushed",
        "{row}",
        "Pending-delivery rows pushed to recovering resources.";
    increment increment_broadcast_delivered,
        "xmpp.broadcast.delivered",
        "{stanza}",
        "Non-blocking broadcast attempts enqueued to a recipient.";
    increment increment_broadcast_not_connected,
        "xmpp.broadcast.not_connected",
        "{stanza}",
        "Non-blocking broadcast attempts with no connected recipient.";
    increment increment_broadcast_dropped_full,
        "xmpp.broadcast.dropped_full",
        "{stanza}",
        "Non-blocking broadcast attempts dropped because the recipient channel was full.";
    increment increment_broadcast_dropped_closed,
        "xmpp.broadcast.dropped_closed",
        "{stanza}",
        "Non-blocking broadcast attempts dropped because the recipient channel was closed.";
    increment increment_delivery_terminal_error_drop,
        "xmpp.delivery.terminal_error_drop",
        "{stanza}",
        "Actor-path deliveries dropped after an enqueue-uncertain terminal error.";
    increment increment_delivery_retry_exhausted_drop,
        "xmpp.delivery.retry_exhausted_drop",
        "{stanza}",
        "Deliveries dropped after bounded full-channel retries were exhausted.";
    increment increment_resolver_affiliation_sync_capacity_drop,
        "xmpp.resolver.affiliation_sync_capacity_drop",
        "{event}",
        "Resolver-affiliation synchronization jobs dropped at scheduler capacity.";
    increment increment_user_actor_reaped,
        "xmpp.user_actor.reaped",
        "{session}",
        "Empty user actors removed by the periodic reaper.";
    increment increment_dnd_projection_read_errored,
        "xmpp.dnd.projection_read_errored",
        "{event}",
        "DND projection reads that failed open to inactive.";
}

/// `add(0)` every reliability counter — and every value of its closed
/// attribute enums — so a fresh, healthy pod exports the whole family
/// at zero on its first OTLP push.
///
/// Call once from `waddle-server::telemetry::init`, immediately after
/// the meter provider is installed globally. Every zero goes through
/// the same call site (and therefore the same cached instrument) the
/// emitting helper uses, so registration cannot create a second,
/// divergent stream.
///
/// Deliberately **not** registered here: `waddle.janitor.sweeps`. Its
/// alert is `min by (janitor) (increase(...[30m])) == 0`, so a
/// pre-registered `outcome="failed"` series that stays flat at zero
/// would hold that alert permanently firing. Zero-registration is for
/// counters whose alert asks `> N`; a no-data-is-fine "did this ever
/// tick" alert must keep its absent series.
pub fn register_reliability_counters() {
    register_table_counters();
    for path in SmEvictionPath::ALL {
        add_sm_unacked_evicted(0, path);
    }
    for outcome in IngressGcOutcome::ALL {
        add_ingress_gc_runs(0, outcome);
    }
    for class in IngressDecisionClass::ALL {
        add_ingress_decision(0, class);
    }
    for outcome in IngressAliasOutcome::ALL {
        add_ingress_alias_outcome(0, outcome);
    }
    for kind in IngressUnresolvedEffectKind::ALL {
        add_ingress_effect_unresolved(0, kind);
    }
    add_ingress_tx_retry(0);
    add_ingress_gc_reclaimed_messages(0);
    add_push_candidate_created(0);
    add_push_candidate_coalesced(0);
    add_push_outbox_published(0);
    add_push_outbox_dead_lettered(0);
    for reason in PushRetryReason::ALL {
        add_push_outbox_retry_scheduled(0, reason);
    }
    for reason in PushSuppressReason::ALL {
        add_push_suppressed(0, reason);
    }
    super::push_pipeline::register_pipeline_stages();
    super::call::register_call_setup_counters();
    super::call::register_admin_call_failed_counter();
}

// The push helpers below are hand-written rather than table rows
// because each one drives two families (the `xmpp.push.*` reliability
// counter plus the typed `waddle.push.pipeline` stage) or carries a
// `reason` label the alias recording rules preserve. Each keeps its
// counter's single `counter_add!` call site in a private `add_*(count,
// ..)` function so `register_reliability_counters` can zero it through
// the same instrument.

fn add_push_candidate_created(count: u64) {
    crate::counter_add!(
        "xmpp.push.candidate_created",
        "{notification}",
        "XEP-0357 notification candidates inserted into the durable pipeline.",
        count,
    );
}

/// Record a durable push candidate in the reliability and typed pipeline families.
pub fn increment_push_candidate_created() {
    add_push_candidate_created(1);
    super::push_pipeline::increment_candidate_created();
}

fn add_push_candidate_coalesced(count: u64) {
    crate::counter_add!(
        "xmpp.push.candidate_coalesced",
        "{notification}",
        "Duplicate XEP-0357 notification candidates coalesced at insertion.",
        count,
    );
}

/// Record duplicate candidate coalescing in the reliability and typed pipeline families.
pub fn increment_push_candidate_coalesced() {
    add_push_candidate_coalesced(1);
    super::push_pipeline::increment_coalesced();
}

fn add_push_outbox_published(count: u64) {
    crate::counter_add!(
        "xmpp.push.outbox_published",
        "{notification}",
        "XEP-0357 notification outbox jobs accepted by the Push Service.",
        count,
    );
}

/// Record Push Service acceptance in the reliability and typed pipeline families.
pub fn increment_push_outbox_published() {
    add_push_outbox_published(1);
    super::push_pipeline::increment_published();
}

fn add_push_outbox_retry_scheduled(count: u64, reason: PushRetryReason) {
    crate::counter_add!(
        "xmpp.push.outbox_retry_scheduled",
        "{notification}",
        "XEP-0357 notification outbox jobs scheduled for retry.",
        count,
        reason,
    );
}

/// Record an outbox retry in the reliability and typed pipeline families.
///
/// Carries the `reason` label the alias recording rule preserves
/// (`waddle_push_outbox_retry_scheduled_total{reason=...}`).
pub fn increment_push_outbox_retry_scheduled(reason: PushRetryReason) {
    add_push_outbox_retry_scheduled(1, reason);
    super::push_pipeline::increment_retry_scheduled();
}

fn add_push_outbox_dead_lettered(count: u64) {
    crate::counter_add!(
        "xmpp.push.outbox_dead_lettered",
        "{notification}",
        "XEP-0357 notification outbox jobs terminally dead-lettered.",
        count,
    );
}

/// Record terminal outbox dead-lettering in the reliability and typed pipeline families.
pub fn increment_push_outbox_dead_lettered() {
    add_push_outbox_dead_lettered(1);
    super::push_pipeline::increment_dead_lettered();
}

fn add_push_suppressed(count: u64, reason: PushSuppressReason) {
    crate::counter_add!(
        "xmpp.push.suppressed",
        "{notification}",
        "XEP-0357 notification candidates suppressed by a bounded policy reason.",
        count,
        reason,
    );
}

/// Record one typed push suppression in the reliability and typed pipeline families.
pub fn increment_push_suppressed(reason: PushSuppressReason) {
    add_push_suppressed(1, reason);
    super::push_pipeline::increment_suppressed();
}

fn add_ingress_decision(count: u64, class: IngressDecisionClass) {
    crate::counter_add!(
        "ingress.decisions",
        "{event}",
        "Ingress authority decisions.",
        count,
        class
    );
}
pub fn increment_ingress_decision(class: IngressDecisionClass) {
    add_ingress_decision(1, class);
}

fn add_ingress_alias_outcome(count: u64, outcome: IngressAliasOutcome) {
    crate::counter_add!(
        "ingress.alias.outcomes",
        "{event}",
        "Ingress authority alias.outcomes.",
        count,
        outcome
    );
}
pub fn increment_ingress_alias_outcome(outcome: IngressAliasOutcome) {
    add_ingress_alias_outcome(1, outcome);
}

fn add_ingress_effect_unresolved(count: u64, kind: IngressUnresolvedEffectKind) {
    crate::counter_add!(
        "ingress.effects.unresolved",
        "{event}",
        "Ingress authority effects.unresolved.",
        count,
        kind
    );
}
pub fn increment_ingress_effect_unresolved(kind: IngressUnresolvedEffectKind) {
    add_ingress_effect_unresolved(1, kind);
}

fn add_ingress_tx_retry(count: u64) {
    crate::counter_add!(
        "ingress.tx.retries",
        "{transaction}",
        "Ingress authority transaction retries.",
        count
    );
}
pub fn increment_ingress_tx_retry() {
    add_ingress_tx_retry(1);
}

pub fn record_ingress_tx_duration(duration: std::time::Duration) {
    crate::histogram_record!(
        "ingress.tx.duration",
        "s",
        "Ingress authority transaction duration.",
        duration.as_secs_f64()
    );
}

fn add_ingress_gc_runs(count: u64, outcome: IngressGcOutcome) {
    crate::counter_add!(
        "ingress.gc.runs",
        "{run}",
        "Ingress retention GC runs by closed outcome.",
        count,
        outcome,
    );
}

pub fn increment_ingress_gc_run(outcome: IngressGcOutcome) {
    add_ingress_gc_runs(1, outcome);
}

fn record_ingress_gc_reclaimed_messages(count: u64) {
    crate::counter_add!(
        "ingress.gc.reclaimed_messages",
        "{message}",
        "Ingress messages reclaimed by ingress retention GC runs (lower bound: progress lost to external cancellation is not counted).",
        count,
    );
}

pub fn add_ingress_gc_reclaimed_messages(count: u64) {
    record_ingress_gc_reclaimed_messages(count);
}

// The legacy unknown-reason catch-all family is gone with the text
// renderer: the sealed `PushSuppressReason` enum makes an unmapped
// reason a compile error, so it was structurally unreachable and
// permanently 0. It is retired without an alias (rules README).

/// Record one periodic janitor sweep outcome.
///
/// Not zero-registered — see [`register_reliability_counters`].
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
    async fn ingress_authority_helpers_emit_with_typed_labels() {
        let guard = setup().await;
        increment_ingress_decision(IngressDecisionClass::ExistingCommitted);
        increment_ingress_alias_outcome(IngressAliasOutcome::NoOrigin);
        increment_ingress_tx_retry();
        increment_ingress_effect_unresolved(IngressUnresolvedEffectKind::Delivery);
        record_ingress_tx_duration(std::time::Duration::from_millis(250));
        assert_eq!(
            guard.counter_sum("ingress.decisions", &[("class", "existing_committed")]),
            Some(1)
        );
        assert_eq!(
            guard.counter_sum("ingress.alias.outcomes", &[("outcome", "no_origin")]),
            Some(1)
        );
        assert_eq!(guard.counter_sum("ingress.tx.retries", &[]), Some(1));
        assert_eq!(
            guard.counter_sum("ingress.effects.unresolved", &[("kind", "delivery")]),
            Some(1)
        );
        assert_eq!(guard.histogram_count("ingress.tx.duration", &[]), Some(1));
    }

    #[tokio::test]
    async fn ingress_gc_helpers_emit_with_typed_labels() {
        let guard = setup().await;
        increment_ingress_gc_run(IngressGcOutcome::Partial);
        add_ingress_gc_reclaimed_messages(3);
        assert_eq!(
            guard.counter_sum("ingress.gc.runs", &[("outcome", "partial")]),
            Some(1)
        );
        assert_eq!(
            guard.counter_sum("ingress.gc.reclaimed_messages", &[]),
            Some(3)
        );
    }

    #[tokio::test]
    async fn ingress_gc_failure_rule_matches_exported_outcome_label() {
        let guard = setup().await;
        let rules_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml");
        let rules = std::fs::read_to_string(&rules_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", rules_path.display()));
        let expression = rules
            .lines()
            .skip_while(|line| !line.contains("alert: IngressGcFailing"))
            .nth(1)
            .expect("IngressGcFailing expression");
        assert!(
            expression.contains("ingress_gc_runs_total{outcome=~\"failed|timed_out|unattested\"}")
        );
        for outcome in [
            IngressGcOutcome::Failed,
            IngressGcOutcome::TimedOut,
            IngressGcOutcome::Unattested,
        ] {
            increment_ingress_gc_run(outcome);
            assert_eq!(
                guard.counter_sum("ingress.gc.runs", &[("outcome", outcome.value())]),
                Some(1)
            );
            assert_eq!(
                guard.counter_sum("ingress.gc.runs", &[("result", outcome.value())]),
                Some(0)
            );
        }
    }

    #[test]
    fn ingress_rules_cover_every_non_advancing_decision() {
        let rules_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml");
        let rules = std::fs::read_to_string(&rules_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", rules_path.display()));
        let alerted_classes: Vec<_> = rules
            .lines()
            .filter(|line| line.trim_start().starts_with("expr:"))
            .filter_map(|line| line.split_once("ingress_decisions_total{class=~\""))
            .filter_map(|(_, selector)| selector.split_once('"'))
            .flat_map(|(classes, _)| classes.split('|'))
            .collect();
        for class in IngressDecisionClass::ALL {
            if !class.advances() {
                assert!(
                    alerted_classes.contains(&class.value()),
                    "non-advancing ingress decision {} has no alert",
                    class.value()
                );
            }
        }
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

    /// The counters `register_reliability_counters` zeroes that carry
    /// no attributes, beyond the macro table.
    const REGISTERED_SM_COUNTERS: &[&str] = &["xmpp.sm.unacked_evicted"];

    /// The counters `register_reliability_counters` zeroes that carry
    /// no attributes, beyond the macro table.
    const REGISTERED_PUSH_COUNTERS: &[&str] = &[
        "xmpp.push.candidate_created",
        "xmpp.push.candidate_coalesced",
        "xmpp.push.outbox_published",
        "xmpp.push.outbox_dead_lettered",
    ];

    const REGISTERED_INGRESS_COUNTERS: &[&str] = &[
        "ingress.decisions",
        "ingress.alias.outcomes",
        "ingress.tx.retries",
        "ingress.gc.runs",
        "ingress.gc.reclaimed_messages",
        "ingress.effects.unresolved",
    ];

    /// Every metric name startup registration is expected to export.
    fn registered_counter_names() -> Vec<&'static str> {
        TABLE_COUNTER_NAMES
            .iter()
            .copied()
            .chain(REGISTERED_SM_COUNTERS.iter().copied())
            .chain(REGISTERED_INGRESS_COUNTERS.iter().copied())
            .chain(REGISTERED_PUSH_COUNTERS.iter().copied())
            .chain(["xmpp.push.outbox_retry_scheduled", "xmpp.push.suppressed"])
            .chain([
                "waddle.call.admin.call_failed",
                "waddle.call.setup.attempted",
                "waddle.call.setup.ok",
                "waddle.call.setup.failed",
            ])
            .collect()
    }

    /// #1436 acceptance: after startup registration a pod that has done
    /// nothing wrong still exports every alert-worthy counter at 0, so
    /// `increase(...[1h]) > 0` reads 0 rather than no-data.
    #[tokio::test]
    async fn registration_exports_every_reliability_counter_at_zero() {
        let guard = setup().await;
        register_reliability_counters();

        for name in registered_counter_names() {
            assert_eq!(
                guard.counter_sum(name, &[]),
                Some(0),
                "{name} missing from startup zero-registration"
            );
        }
    }

    /// The label dimensions are enumerated from the closed enums, so
    /// every expected series exists — not just the label-free one.
    #[tokio::test]
    async fn registration_covers_every_closed_attribute_value() {
        let guard = setup().await;
        register_reliability_counters();

        assert_eq!(
            IngressGcOutcome::ALL.map(|outcome| outcome.value()),
            ["completed", "partial", "failed", "timed_out", "unattested"]
        );

        for path in SmEvictionPath::ALL {
            assert_eq!(
                guard.counter_sum("xmpp.sm.unacked_evicted", &[("path", path.value())]),
                Some(0),
                "sm eviction path {} not registered",
                path.value()
            );
        }
        for class in IngressDecisionClass::ALL {
            assert_eq!(
                guard.counter_sum("ingress.decisions", &[("class", class.value())]),
                Some(0),
                "ingress decision class {} not registered",
                class.value()
            );
        }
        for kind in IngressUnresolvedEffectKind::ALL {
            assert_eq!(
                guard.counter_sum("ingress.effects.unresolved", &[("kind", kind.value())]),
                Some(0),
                "ingress unresolved effect kind {} not registered",
                kind.value()
            );
        }
        for outcome in IngressAliasOutcome::ALL {
            assert_eq!(
                guard.counter_sum("ingress.alias.outcomes", &[("outcome", outcome.value())]),
                Some(0),
                "ingress alias outcome {} not registered",
                outcome.value()
            );
        }
        for outcome in IngressGcOutcome::ALL {
            assert_eq!(
                guard.counter_sum("ingress.gc.runs", &[("outcome", outcome.value())]),
                Some(0),
                "ingress GC outcome {} not registered",
                outcome.value()
            );
        }
        for reason in PushRetryReason::ALL {
            assert_eq!(
                guard.counter_sum(
                    "xmpp.push.outbox_retry_scheduled",
                    &[("reason", reason.value())]
                ),
                Some(0),
                "retry reason {} not registered",
                reason.value()
            );
        }
        for reason in PushSuppressReason::ALL {
            assert_eq!(
                guard.counter_sum("xmpp.push.suppressed", &[("reason", reason.as_str())]),
                Some(0),
                "suppress reason {} not registered",
                reason.as_str()
            );
        }
        for stage in [
            "candidate_created",
            "suppressed",
            "coalesced",
            "published",
            "retry_scheduled",
            "dead_lettered",
        ] {
            assert_eq!(
                guard.counter_sum("waddle.push.pipeline", &[("stage", stage)]),
                Some(0),
                "pipeline stage {stage} not registered"
            );
        }
        for reason in crate::telemetry::attributes::CallSetupFailureReason::ALL {
            assert_eq!(
                guard.counter_sum(
                    "waddle.call.setup.failed",
                    &[(
                        "reason",
                        crate::telemetry::attributes::MetricAttribute::value(&reason)
                    )]
                ),
                Some(0),
                "call setup failure reason not registered"
            );
        }
        for op in crate::telemetry::attributes::AdminOp::ALL {
            assert_eq!(
                guard.counter_sum(
                    "waddle.call.admin.call_failed",
                    &[("op", MetricAttribute::value(&op))]
                ),
                Some(0),
                "admin call op not registered"
            );
        }
    }

    /// Registration must reuse the emitting call site's instrument: a
    /// second stream under the same name would double-count on the
    /// Prometheus side.
    #[tokio::test]
    async fn registration_then_increment_reads_one() {
        let guard = setup().await;
        register_reliability_counters();
        increment_sm_drain_timeout();
        increment_sm_unacked_evicted(SmEvictionPath::Batch);
        increment_pending_delivery_unresolved_poison_pill();
        increment_push_outbox_dead_lettered();
        increment_delivery_terminal_error_drop();

        for name in [
            "xmpp.sm.drain_timeout",
            "xmpp.pending_delivery.unresolved_poison_pill",
            "xmpp.push.outbox_dead_lettered",
            "xmpp.delivery.terminal_error_drop",
        ] {
            assert_eq!(
                guard.counter_sum(name, &[]),
                Some(1),
                "{name} double-counted"
            );
        }
        let (monotonic, _) = guard
            .counter_shape("xmpp.sm.drain_timeout")
            .expect("registered counter must export as a u64 sum");
        assert!(monotonic);
        assert_eq!(
            guard.counter_sum("xmpp.sm.unacked_evicted", &[("path", "batch")]),
            Some(1),
            "path-labeled sm eviction counter double-counted"
        );
    }

    #[tokio::test]
    async fn sm_ack_resume_progress_and_latency_helpers_emit() {
        let guard = setup().await;

        increment_sm_ack(SmAckOutcome::Advanced);
        increment_sm_ack(SmAckOutcome::TooHigh);
        increment_sm_resume_result(SmResumeOutcome::Resumed);
        increment_sm_resume_result(SmResumeOutcome::Storage);
        add_sm_handled_progress(7);
        record_sm_request_latency_ms(12.5);

        assert_eq!(
            guard.counter_sum("xmpp.sm.acks", &[("outcome", "advanced")]),
            Some(1)
        );
        assert_eq!(
            guard.counter_sum("xmpp.sm.acks", &[("outcome", "too_high")]),
            Some(1)
        );
        assert_eq!(
            guard.counter_sum("xmpp.sm.resume.results", &[("outcome", "resumed")]),
            Some(1)
        );
        assert_eq!(
            guard.counter_sum("xmpp.sm.resume.results", &[("outcome", "storage")]),
            Some(1)
        );
        assert_eq!(guard.counter_sum("xmpp.sm.handled_progress", &[]), Some(7));
        assert_eq!(
            guard.histogram_count("xmpp.sm.request.latency", &[]),
            Some(1)
        );
        assert_eq!(
            guard.metric_unit("xmpp.sm.request.latency"),
            Some("ms".to_string())
        );
    }

    /// The janitor heartbeat is a `== 0` alert: a pre-registered
    /// `outcome="failed"` series flat at zero would hold
    /// `JanitorHeartbeatStale` permanently firing. Registration must
    /// leave it absent.
    #[tokio::test]
    async fn registration_leaves_the_janitor_heartbeat_absent() {
        let guard = setup().await;
        register_reliability_counters();
        assert_eq!(guard.counter_sum("waddle.janitor.sweeps", &[]), None);
    }

    /// Every `xmpp_*` counter the Mimir reliability rules read must be
    /// zero-registered, or the alias recording rule it feeds goes
    /// no-data on a fresh pod. Guards the rules file and the
    /// registration list against drifting apart.
    #[test]
    fn every_xmpp_counter_in_the_mimir_rules_is_registered() {
        let rules_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml");
        // Full checkouts have the file, and the nix test derivations
        // copy it into the build tree (flake.nix testArgs postUnpack),
        // so a missing file is a broken copy path, not an expected
        // environment — fail loudly rather than silently skipping the
        // guard (#1436).
        let rules = std::fs::read_to_string(&rules_path).unwrap_or_else(|error| {
            panic!(
                "read {}: {error} — if this is a filtered build tree, the flake.nix                  testArgs postUnpack copy is broken",
                rules_path.display()
            )
        });

        let registered: Vec<String> = registered_counter_names()
            .into_iter()
            .map(|name| format!("{}_total", name.replace('.', "_")))
            .collect();

        for word in rules.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if !(word.starts_with("xmpp_")
                || word.starts_with("waddle_call_")
                || word.starts_with("ingress_"))
                || !word.ends_with("_total")
            {
                continue;
            }
            // Registered from waddle-server (its emitting helper lives in
            // the webhook route), with its own zero-registration test
            // there — this crate cannot reach it.
            if word == "waddle_call_webhook_events_total" {
                continue;
            }
            assert!(
                registered.iter().any(|name| name == word),
                "{word} is read by the Mimir reliability rules but is not \
                 zero-registered by register_reliability_counters()"
            );
        }
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
        increment_sm_unacked_evicted(SmEvictionPath::Batch);
        increment_sm_detached_unacked_evicted();
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
        assert_eq!(
            guard.counter_sum("xmpp.sm.unacked_evicted", &[("path", "batch")]),
            Some(1)
        );
        assert_eq!(
            guard.counter_sum("xmpp.sm.detached_unacked_evicted", &[]),
            Some(1)
        );
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

    #[tokio::test]
    async fn sm_eviction_registration_exports_exact_path_samples() {
        let guard = setup().await;
        register_reliability_counters();

        let mut samples = guard
            .counter_samples("xmpp.sm.unacked_evicted")
            .expect("sm eviction counter must export");
        samples.sort_by(|left, right| left.1.cmp(&right.1));

        assert_eq!(
            samples,
            vec![
                (0, vec![("path".to_string(), "batch".to_string())]),
                (0, vec![("path".to_string(), "detach_drain".to_string())]),
                (0, vec![("path".to_string(), "direct_outbound".to_string())]),
                (0, vec![("path".to_string(), "replay_tail".to_string())]),
            ]
        );
    }

    #[tokio::test]
    async fn sm_eviction_helpers_emit_exact_path_samples() {
        let guard = setup().await;
        increment_sm_unacked_evicted(SmEvictionPath::Batch);
        increment_sm_unacked_evicted(SmEvictionPath::DetachDrain);
        increment_sm_unacked_evicted(SmEvictionPath::DirectOutbound);
        increment_sm_unacked_evicted(SmEvictionPath::ReplayTail);

        let mut samples = guard
            .counter_samples("xmpp.sm.unacked_evicted")
            .expect("sm eviction counter must export");
        samples.sort_by(|left, right| left.1.cmp(&right.1));

        assert_eq!(
            samples,
            vec![
                (1, vec![("path".to_string(), "batch".to_string())]),
                (1, vec![("path".to_string(), "detach_drain".to_string())]),
                (1, vec![("path".to_string(), "direct_outbound".to_string())]),
                (1, vec![("path".to_string(), "replay_tail".to_string())]),
            ]
        );
    }
}
