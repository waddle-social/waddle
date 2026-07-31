//! Typed counters for the `waddle.call.sfu_token.*` family, shared by
//! the protocol-layer Jingle handler and the server-side Muji gate so
//! one family is never emitted from divergent macro sites.

use super::attributes::{CallSetupFailureReason, SfuDenialReason};

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
/// discouraged by the [`Self::open`] constructor being the only way
/// to build one.
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
    /// Open a ticket for a routed 1:1 `session-initiate`. Only the
    /// Jingle handler (and tests standing in for it) should call
    /// this; everything downstream receives the ticket through
    /// `OutboundEvent::RouteToConnection`.
    pub fn open() -> Self {
        Self(std::sync::Arc::new(TicketCell(
            std::sync::atomic::AtomicBool::new(false),
        )))
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

#[cfg(test)]
mod pending_call_setup_route_tests {
    use super::PendingCallSetupRoute;

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
}
