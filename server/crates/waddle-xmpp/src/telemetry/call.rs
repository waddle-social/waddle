//! Typed counters for the `waddle.call.sfu_token.*` family, shared by
//! the protocol-layer Jingle handler and the server-side Muji gate so
//! one family is never emitted from divergent macro sites.

use super::attributes::{
    AdminOp, CallControlRateLimitedSurface, CallSetupFailureReason, SfuDenialReason,
};

/// Count a minted LiveKit SFU token.
pub fn increment_sfu_token_minted() {
    crate::counter_add!(
        "waddle.call.sfu_token.minted",
        "1",
        "LiveKit SFU tokens minted.",
        1,
    );
}

/// Count an SFU token denial by reason.
pub fn increment_sfu_token_denied(reason: SfuDenialReason) {
    crate::counter_add!(
        "waddle.call.sfu_token.denied",
        "1",
        "SFU token requests denied by reason.",
        1,
        reason,
    );
}

fn add_admin_call_failed(count: u64, op: AdminOp) {
    crate::counter_add!(
        "waddle.call.admin.call_failed",
        "1",
        "LiveKit admin control-plane calls that failed after final normalization, by operation.",
        count,
        op,
    );
}

/// Count a failed LiveKit admin control-plane call by operation.
pub fn increment_admin_call_failed(op: AdminOp) {
    add_admin_call_failed(1, op);
}

/// `add(0)` the admin-failure family so fresh pods export every
/// alertable operation series before the first real failure.
pub(super) fn register_admin_call_failed_counter() {
    for op in AdminOp::ALL {
        add_admin_call_failed(0, op);
    }
}

/// Count one Jingle `session-initiate` entering call setup (#1452).
///
/// This is the denominator of the call success rate. It is emitted
/// exactly once per attempt, at the point the action is known to be a
/// `session-initiate`; every attempt then terminates in exactly one
/// [`increment_call_setup_ok`] or [`increment_call_setup_failed`].
pub fn increment_call_setup_attempted() {
    add_call_setup_attempted(1);
}

fn add_call_setup_attempted(count: u64) {
    crate::counter_add!(
        "waddle.call.setup.attempted",
        "1",
        "Jingle call setups attempted (session-initiate).",
        count,
    );
}

/// Count a call setup that produced a usable session for the caller
/// (a join token was issued and the negotiation stanza was forwarded
/// or accepted).
///
/// Precisely: `ok` means the server authorized the attempt and the
/// invite reached the peer's routing layer with a usable destination.
/// For the Muji path that is token mint + registration; for the 1:1
/// path the handler defers to the routing interpreter via
/// [`PendingCallSetupRoute`], so an invite whose full JID turns out to
/// be unroutable counts `failed{reason=peer_unavailable}` instead of
/// `ok` (#1488).
pub fn increment_call_setup_ok() {
    add_call_setup_ok(1);
}

fn add_call_setup_ok(count: u64) {
    crate::counter_add!(
        "waddle.call.setup.ok",
        "1",
        "Jingle call setups that completed successfully.",
        count,
    );
}

/// Count a call setup that terminated in an error, by reason.
pub fn increment_call_setup_failed(reason: CallSetupFailureReason) {
    add_call_setup_failed(1, reason);
}

fn add_call_setup_failed(count: u64, reason: CallSetupFailureReason) {
    crate::counter_add!(
        "waddle.call.setup.failed",
        "1",
        "Jingle call setups that failed, by reason.",
        count,
        reason,
    );
}

/// `add(0)` the call setup SLI family — and every failure reason — so
/// a fresh pod exports it before the first call of a deploy (#1436):
/// `CallSetupFailureRate` and `CallSetupRoomNotFound` read these via
/// `increase()` and would otherwise evaluate to no-data. Called from
/// [`super::reliability::register_reliability_counters`].
pub(super) fn register_call_setup_counters() {
    add_call_setup_attempted(0);
    add_call_setup_ok(0);
    for reason in CallSetupFailureReason::ALL {
        add_call_setup_failed(0, reason);
    }
}

/// An open 1:1 call-setup attempt handed off to the routing layer
/// (#1488).
///
/// The Jingle handler counts `attempted` when it opens the attempt,
/// but for a routed 1:1 `session-initiate` the outcome is only known
/// after the sans-I/O boundary, when the interpreter has resolved the
/// addressed full JID against live and detached resources. The handler
/// therefore attaches this ticket to the routing effect instead of
/// counting `ok` at emit time, and the interpreter MUST close it
/// exactly once from the route disposition: [`Self::delivered`] when
/// the invite reached a live resource, a detached XEP-0198 session, or
/// an ambiguous-but-possibly-committed cluster relay;
/// [`Self::undeliverable`] when no usable destination existed and the
/// caller got the undeliverable bounce.
///
/// Exactly-once is enforced at the type level (Qodo review on PR
/// #1611): the ticket is a shared one-shot guard, so clones — e.g.
/// through `OutboundEvent`'s derived `Clone` — share the same closed
/// bit and the first `delivered`/`undeliverable` wins. A second close
/// is a counted-nowhere no-op that logs at warn, and fabrication is
/// prevented by the [`Self::open`] constructor being `pub(crate)`:
/// outside this crate a ticket can only be obtained from the routing
/// effect the Jingle handler built — after it counted `attempted` —
/// or minted deliberately in tests via [`Self::open_for_test`].
#[derive(Debug, Clone)]
pub struct PendingCallSetupRoute(std::sync::Arc<TicketCell>);

/// Shared closed bit with a Drop backstop: when the LAST clone of an
/// unclosed ticket is dropped — the routing pipeline discarded the
/// invite without resolving delivery — the attempt is closed as
/// `failed{reason=route_abandoned}` so `attempted` always receives a
/// terminal increment and the success-rate denominator cannot leak
/// (#1611 review).
#[derive(Debug)]
struct TicketCell(std::sync::atomic::AtomicBool);

impl Drop for TicketCell {
    fn drop(&mut self) {
        if !self.0.load(std::sync::atomic::Ordering::SeqCst) {
            tracing::warn!(
                "call-setup ticket dropped without a disposition; counting route_abandoned"
            );
            increment_call_setup_failed(CallSetupFailureReason::RouteAbandoned);
        }
    }
}

impl PendingCallSetupRoute {
    /// Open a ticket for a routed 1:1 `session-initiate`.
    ///
    /// Crate-private on purpose (#1611 review): every ticket's terminal
    /// `ok`/`failed` increment presumes the Jingle handler already
    /// counted `waddle.call.setup.attempted` for the same attempt, so
    /// only that handler may mint tickets — a ticket opened without the
    /// `attempted` increment corrupts the `CallSetupFailureRate`
    /// denominator. Everything downstream receives the ticket through
    /// `OutboundEvent::RouteToConnection`; cross-crate tests use
    /// [`Self::open_for_test`].
    pub(crate) fn open() -> Self {
        Self(std::sync::Arc::new(TicketCell(
            std::sync::atomic::AtomicBool::new(false),
        )))
    }

    /// Test-only ticket constructor that models production accounting:
    /// it counts `waddle.call.setup.attempted` before opening, exactly
    /// as the Jingle handler does, so suites exercising ticket closure
    /// keep the SLI numerator and denominator consistent.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn open_for_test() -> Self {
        increment_call_setup_attempted();
        Self::open()
    }

    /// Flip the shared closed bit; `true` iff this call performed the
    /// close and may count.
    fn close(&self) -> bool {
        self.0
             .0
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }

    /// The routed invite reached a usable destination: close the
    /// attempt as `ok`.
    pub fn delivered(self) {
        if self.close() {
            increment_call_setup_ok();
        } else {
            tracing::warn!("call-setup ticket closed twice (delivered); second close ignored");
        }
    }

    /// No live or detached resource could take the invite (or the
    /// delivery was dropped): close the attempt as
    /// `failed{reason=peer_unavailable}`.
    pub fn undeliverable(self) {
        if self.close() {
            increment_call_setup_failed(CallSetupFailureReason::PeerUnavailable);
        } else {
            tracing::warn!("call-setup ticket closed twice (undeliverable); second close ignored");
        }
    }
}

/// Count a call setup rejected at a gate that runs *before* the
/// per-attempt tracker in the Jingle handler opens — the server-side
/// Muji membership gate (which short-circuits IQ dispatch entirely),
/// the `session-initiate` rate limiter, and the handler's own
/// pre-routing wire-shape checks.
///
/// Emits the attempted/failed pair together so `attempted` stays a
/// true denominator: a gate rejection is a complete attempt that
/// simply never reached the negotiation path.
pub fn record_call_setup_rejected(reason: CallSetupFailureReason) {
    increment_call_setup_attempted();
    increment_call_setup_failed(reason);
}

/// Count a call-control request rejected by a local sliding-window
/// limiter.
pub fn increment_call_control_rate_limited(surface: CallControlRateLimitedSurface) {
    crate::counter_add!(
        "waddle.call.control.rate_limited",
        "1",
        "Call-control requests rejected by local rate limits.",
        1,
        surface,
    );
}

/// Count a webhook-driven teardown dropped because its observed SID
/// belongs to an older call incarnation.
pub fn increment_call_teardown_stale_dropped() {
    crate::counter_add!(
        "waddle.call.teardown.stale_dropped",
        "1",
        "Webhook-driven call teardowns dropped due to stale room or participant SIDs.",
        1,
    );
}

/// Record the durable teardown queue depth observed by one janitor sweep.
pub fn record_call_teardown_outbox_depth(depth: u64) {
    crate::histogram_record!(
        "waddle.call.teardown.outbox.depth",
        "1",
        "Queued durable call teardown intents observed at sweep time.",
        depth as f64,
    );
}

/// Record the age of the oldest queued teardown intent observed by a sweep.
pub fn record_call_teardown_outbox_oldest_age(seconds: f64) {
    crate::histogram_record!(
        "waddle.call.teardown.outbox.oldest_age",
        "s",
        "Age of the oldest queued durable call teardown intent.",
        buckets: crate::telemetry::SECOND_SCALE_BUCKETS,
        seconds,
    );
}

/// Count durable teardown intents completed by the outbox drain.
pub fn add_call_teardown_outbox_drained(count: u64) {
    if count > 0 {
        crate::counter_add!(
            "waddle.call.teardown.outbox.drained",
            "1",
            "Durable call teardown intents completed by the outbox drain.",
            count,
        );
    }
}

/// Count durable teardown intents scheduled for another attempt.
pub fn add_call_teardown_outbox_requeued(count: u64) {
    if count > 0 {
        crate::counter_add!(
            "waddle.call.teardown.outbox.requeued",
            "1",
            "Durable call teardown intents requeued after retryable failures.",
            count,
        );
    }
}

/// Count durable teardown intents that exhausted their retry budget.
pub fn add_call_teardown_outbox_failed(count: u64) {
    if count > 0 {
        crate::counter_add!(
            "waddle.call.teardown.outbox.failed",
            "1",
            "Durable call teardown intents moved to terminal failure.",
            count,
        );
    }
}

/// Record the wall-clock duration of one LiveKit reconciliation pass.
pub fn record_reconcile_pass_duration(seconds: f64) {
    crate::histogram_record!(
        "waddle.call.reconcile.pass_duration",
        "s",
        "Wall-clock duration of one LiveKit call reconciliation pass.",
        buckets: crate::telemetry::SECOND_SCALE_BUCKETS,
        seconds,
    );
}

/// Add the number of rooms whose occupancy was examined in a pass.
pub fn add_reconcile_rooms_examined(count: u64) {
    if count == 0 {
        return;
    }
    crate::counter_add!(
        "waddle.call.reconcile.rooms_examined",
        "1",
        "LiveKit rooms examined by call reconciliation.",
        count,
    );
}

/// Add the number of LiveKit rooms adopted into the local registry.
pub fn add_reconcile_rooms_adopted(count: u64) {
    if count == 0 {
        return;
    }
    crate::counter_add!(
        "waddle.call.reconcile.rooms_adopted",
        "1",
        "LiveKit rooms adopted into the local call registry.",
        count,
    );
}

/// Add the number of rooms from which at least one ghost was swept.
pub fn add_reconcile_rooms_swept(count: u64) {
    if count == 0 {
        return;
    }
    crate::counter_add!(
        "waddle.call.reconcile.rooms_swept",
        "1",
        "LiveKit rooms with registry ghosts swept by reconciliation.",
        count,
    );
}

/// Add failed per-room occupancy probes.
///
/// This is alert-worthy operational evidence, but unlike the #1436
/// reliability-rate families it must not be startup zero-registered:
/// absence means no failure has ever ticked in this process.
pub fn add_reconcile_occupancy_failures(count: u64) {
    if count == 0 {
        return;
    }
    crate::counter_add!(
        "waddle.call.reconcile.occupancy_failures",
        "1",
        "LiveKit room occupancy probes failed during call reconciliation.",
        count,
    );
}
#[cfg(test)]
mod pending_call_setup_route_tests {
    use super::{increment_admin_call_failed, AdminOp, PendingCallSetupRoute};

    #[tokio::test(flavor = "current_thread")]
    async fn an_unclosed_dropped_ticket_counts_route_abandoned_once() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let ticket = PendingCallSetupRoute::open();
        let clone = ticket.clone();
        drop(ticket);
        // The backstop fires only when the LAST clone goes away.
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.failed", &[("reason", "route_abandoned")])
                .unwrap_or(0),
            0
        );
        drop(clone);
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.failed", &[("reason", "route_abandoned")]),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_closed_ticket_drops_without_extra_counts() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let ticket = PendingCallSetupRoute::open();
        ticket.delivered();
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.failed", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(metrics.counter_sum("waddle.call.setup.ok", &[]), Some(1));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_cloned_ticket_closes_exactly_once() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let ticket = PendingCallSetupRoute::open();
        let clone = ticket.clone();

        ticket.delivered();
        // The second close (via the clone, with the opposite verdict)
        // must be a counted-nowhere no-op.
        clone.undeliverable();

        assert_eq!(metrics.counter_sum("waddle.call.setup.ok", &[]), Some(1));
        assert_eq!(
            metrics
                .counter_sum(
                    "waddle.call.setup.failed",
                    &[("reason", "peer_unavailable")]
                )
                .unwrap_or(0),
            0
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admin_call_failure_helper_emits_with_typed_op() {
        let metrics = crate::telemetry::test_support::acquire().await;
        increment_admin_call_failed(AdminOp::DeleteRoom);
        assert_eq!(
            metrics.counter_sum("waddle.call.admin.call_failed", &[("op", "delete_room")]),
            Some(1)
        );
    }
}
