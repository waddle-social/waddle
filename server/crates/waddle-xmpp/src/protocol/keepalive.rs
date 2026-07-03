//! RFC 7395 §3.8 transport keepalive policy (issue #1090).
//!
//! RFC 7395 §3.8 ("Pings and Keepalives") sanctions WebSocket ping/pong
//! control frames as the keepalive mechanism for the XMPP subprotocol
//! (alongside XEP-0199 and XEP-0198); Waddle uses WS ping/pong because
//! it needs no stanza semantics and browsers answer it automatically at
//! the protocol layer. This module owns the *policy* —
//! when to probe, when to give up — as pure, clock-free state driven by
//! [`InboundEvent::Tick`](super::InboundEvent::Tick) and liveness
//! evidence. The transport adapter owns the *mechanism*: it maps
//! [`OutboundEvent::SendKeepaliveProbe`] to its native probe frame (a
//! WS `Ping` today; a future TCP transport would map it to whitespace
//! keepalive or XEP-0199) and feeds ticks and
//! [`InboundEvent::KeepaliveAck`](super::InboundEvent::KeepaliveAck)
//! evidence back in.
//!
//! # Probe basis: inbound idle
//!
//! Probes fire on *inbound* silence, not outbound. A dead client on a
//! busy MUC stream never sends anything while the server happily keeps
//! writing; an outbound-idle basis would never detect its death and the
//! XEP-0198 unacked queue would grow until the queue-cap eviction
//! permanently breaks resume. Outbound stanzas independently keep the
//! gateway's bidirectional stream-idle timer happy, so inbound-idle
//! covers both goals with one clock.
//!
//! # Liveness evidence: any inbound frame
//!
//! A peer that sends *anything* — a stanza, a client-initiated ping, a
//! pong with any payload — is alive, which is the only fact the close
//! policy needs. There is deliberately no RFC 6455 pong-payload
//! correlation: it would add state and a false-positive class
//! (payload-mangling intermediaries) for zero liveness value.
//!
//! # Timing
//!
//! With interval `I` and miss limit `M`:
//!
//! - An idle-but-alive peer sees a probe at most every `2·I` (its pong
//!   counts as activity for the following tick), so the worst-case
//!   inter-traffic gap on the stream is `2·I` — the deployment config
//!   ceiling keeps that under the gateway's 300s idle timeout.
//! - A dead peer is closed after `M` unanswered probes: between
//!   `(M + 1)·I` and `(M + 2)·I` after its last inbound frame.

use super::event::{OutboundEvent, TimerId};
use tracing::Level;

/// The reserved [`TimerId`] for the transport keepalive clock.
///
/// Timer ids are per-connection; other state-machine timers (SCRAM
/// timeouts, SM ack deadlines) must allocate distinct ids.
pub const KEEPALIVE_TIMER: TimerId = TimerId(1);

/// Deployment-tunable keepalive knobs.
///
/// Validated at server startup (`waddle-server` `config.rs`): the
/// interval ceiling guarantees `2·interval` stays under the gateway's
/// stream-idle timeout, replacing the "raise gateway idleTimeout"
/// defense-in-depth from issue #1090's original acceptance criteria.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepaliveConfig {
    /// Tick interval in milliseconds.
    pub interval_ms: u64,
    /// Consecutive unanswered probes tolerated before the connection
    /// is closed.
    pub miss_limit: u32,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            interval_ms: 45_000,
            miss_limit: 2,
        }
    }
}

/// Ticks a connection may spend outside the `Ready` phase before it is
/// closed regardless of pong answers.
///
/// Every RFC 6455 stack auto-pongs below the application (browsers
/// answer from the network process even for a frozen tab), so "answers
/// probes" proves only that the TCP path and WS stack are alive — not
/// that the client is making session progress. Without this bound, an
/// unauthenticated socket would be held forever: the server's own
/// pings keep the gateway's idle timer happy and the auto-pongs keep
/// the miss counter at zero. Ironically, before issue #1090 the
/// gateway's ~300s reset reaped such sockets; the keepalive must not
/// remove that bound. Three ticks is 135s at the default 45s interval
/// (and 360s at the 120s config ceiling) — orders of magnitude above a
/// normal sub-second SASL + bind negotiation.
pub const NEGOTIATION_TICK_LIMIT: u32 = 3;

/// Per-connection keepalive policy state.
///
/// Owned by [`XmppStateMachine`](super::XmppStateMachine); driven
/// exclusively through the machine's [`handle`] entry point:
///
/// - [`InboundEvent::TransportReady`] → [`Self::on_transport_ready`]
///   arms the timer.
/// - [`InboundEvent::Tick`]`(`[`KEEPALIVE_TIMER`]`)` →
///   [`Self::on_tick`] evaluates the policy and re-arms.
/// - [`InboundEvent::KeepaliveAck`] (and any machine-visible inbound
///   client frame) → [`Self::mark_alive`].
///
/// [`InboundEvent::TransportReady`]: super::InboundEvent::TransportReady
/// [`InboundEvent::Tick`]: super::InboundEvent::Tick
/// [`InboundEvent::KeepaliveAck`]: super::InboundEvent::KeepaliveAck
/// [`handle`]: super::XmppStateMachine::handle
#[derive(Debug)]
pub struct KeepalivePolicy {
    config: KeepaliveConfig,
    /// Whether any inbound evidence arrived since the last tick.
    alive_since_tick: bool,
    /// Probes sent without any intervening inbound evidence.
    consecutive_misses: u32,
    /// Ticks observed while the session was not yet `Ready`.
    negotiation_ticks: u32,
}

impl KeepalivePolicy {
    /// A fresh policy starts `alive`: the transport just proved
    /// liveness by connecting (or, for the bind-time machine
    /// replacement in the WebSocket adapter, by binding), so the first
    /// quiet interval passes without a probe.
    pub fn new(config: KeepaliveConfig) -> Self {
        Self {
            config,
            alive_since_tick: true,
            consecutive_misses: 0,
            negotiation_ticks: 0,
        }
    }

    /// Arm the keepalive clock. Emitted-once when the transport
    /// reports readiness (WS upgrade), *before* authentication — a
    /// connection that wedges pre-bind is reaped by the same policy.
    pub fn on_transport_ready(&mut self) -> Vec<OutboundEvent> {
        self.alive_since_tick = true;
        self.consecutive_misses = 0;
        self.negotiation_ticks = 0;
        vec![OutboundEvent::SetTimer {
            id: KEEPALIVE_TIMER,
            duration_ms: self.config.interval_ms,
        }]
    }

    /// Record inbound liveness evidence.
    pub fn mark_alive(&mut self) {
        self.alive_since_tick = true;
        self.consecutive_misses = 0;
    }

    /// Evaluate the policy on a keepalive tick. `session_ready` is
    /// whether the machine has reached the `Ready` phase (bound
    /// resource); pre-`Ready` connections are additionally bounded by
    /// [`NEGOTIATION_TICK_LIMIT`].
    ///
    /// - Still negotiating past the tick limit → close, even if the
    ///   peer answers probes (auto-pong proves nothing about session
    ///   progress).
    /// - Evidence since the last tick → quiet re-arm.
    /// - Silence → probe and re-arm, until `miss_limit` probes have
    ///   gone unanswered, then close the transport.
    pub fn on_tick(&mut self, session_ready: bool) -> Vec<OutboundEvent> {
        if session_ready {
            self.negotiation_ticks = 0;
        } else {
            self.negotiation_ticks += 1;
            if self.negotiation_ticks > NEGOTIATION_TICK_LIMIT {
                return vec![
                    OutboundEvent::Log {
                        level: Level::INFO,
                        message: format!(
                            "Keepalive: session still negotiating after {} ticks \
                             (interval {}ms); closing stalled pre-bind connection",
                            self.negotiation_ticks - 1,
                            self.config.interval_ms
                        ),
                    },
                    OutboundEvent::CloseTransport,
                ];
            }
        }
        let rearm = OutboundEvent::SetTimer {
            id: KEEPALIVE_TIMER,
            duration_ms: self.config.interval_ms,
        };
        if self.alive_since_tick {
            self.alive_since_tick = false;
            return vec![rearm];
        }
        if self.consecutive_misses >= self.config.miss_limit {
            return vec![
                OutboundEvent::Log {
                    level: Level::INFO,
                    message: format!(
                        "Keepalive: {} unanswered probes (interval {}ms); closing transport \
                         for dead peer",
                        self.consecutive_misses, self.config.interval_ms
                    ),
                },
                OutboundEvent::CloseTransport,
            ];
        }
        self.consecutive_misses += 1;
        vec![OutboundEvent::SendKeepaliveProbe, rearm]
    }
}

/// Dispatch a [`Tick`](InboundEvent::Tick) by timer id.
///
/// Free function so [`XmppStateMachine::handle`] stays a thin match:
/// unknown ids are logged (a cancelled-then-fired race is benign), the
/// keepalive id runs the policy. `session_ready` mirrors whether the
/// machine's phase is `Ready`.
///
/// [`XmppStateMachine::handle`]: super::XmppStateMachine::handle
pub(super) fn dispatch_tick(
    policy: &mut KeepalivePolicy,
    id: TimerId,
    session_ready: bool,
) -> Vec<OutboundEvent> {
    if id == KEEPALIVE_TIMER {
        policy.on_tick(session_ready)
    } else {
        vec![OutboundEvent::Log {
            level: Level::WARN,
            message: format!("Tick for unknown timer id {id:?}; ignoring"),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(interval_ms: u64, miss_limit: u32) -> KeepaliveConfig {
        KeepaliveConfig {
            interval_ms,
            miss_limit,
        }
    }

    fn assert_rearm(events: &[OutboundEvent], interval_ms: u64) {
        assert!(
            events.iter().any(|e| matches!(
                e,
                OutboundEvent::SetTimer { id, duration_ms }
                    if *id == KEEPALIVE_TIMER && *duration_ms == interval_ms
            )),
            "expected keepalive re-arm in {events:?}"
        );
    }

    fn has_probe(events: &[OutboundEvent]) -> bool {
        events
            .iter()
            .any(|e| matches!(e, OutboundEvent::SendKeepaliveProbe))
    }

    fn has_close(events: &[OutboundEvent]) -> bool {
        events
            .iter()
            .any(|e| matches!(e, OutboundEvent::CloseTransport))
    }

    #[test]
    fn transport_ready_arms_the_timer() {
        let mut p = KeepalivePolicy::new(cfg(45_000, 2));
        let events = p.on_transport_ready();
        assert_eq!(events.len(), 1);
        assert_rearm(&events, 45_000);
    }

    #[test]
    fn active_peer_is_never_probed() {
        let mut p = KeepalivePolicy::new(cfg(45_000, 2));
        p.on_transport_ready();
        for _ in 0..10 {
            p.mark_alive();
            let events = p.on_tick(true);
            assert!(!has_probe(&events), "active peer got probed: {events:?}");
            assert!(!has_close(&events));
            assert_rearm(&events, 45_000);
        }
    }

    #[test]
    fn idle_alive_peer_is_probed_every_other_tick() {
        let mut p = KeepalivePolicy::new(cfg(45_000, 2));
        p.on_transport_ready();
        // Tick 1: initial alive grace clears quietly.
        assert!(!has_probe(&p.on_tick(true)));
        // Tick 2: silence → probe. Peer pongs.
        let events = p.on_tick(true);
        assert!(has_probe(&events));
        assert_rearm(&events, 45_000);
        p.mark_alive();
        // Tick 3: pong counted as evidence → quiet.
        assert!(!has_probe(&p.on_tick(true)));
        // Tick 4: silence again → probe. Worst-case wire gap is 2·interval.
        assert!(has_probe(&p.on_tick(true)));
    }

    #[test]
    fn dead_peer_closes_after_miss_limit_unanswered_probes() {
        let mut p = KeepalivePolicy::new(cfg(45_000, 2));
        p.on_transport_ready();
        // Tick 1: initial grace.
        assert!(!has_probe(&p.on_tick(true)));
        // Ticks 2..=3: two probes, never answered.
        for _ in 0..2 {
            let events = p.on_tick(true);
            assert!(has_probe(&events));
            assert!(!has_close(&events));
        }
        // Tick 4: miss limit reached → close, no further probe or re-arm.
        let events = p.on_tick(true);
        assert!(has_close(&events));
        assert!(!has_probe(&events));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboundEvent::SetTimer { .. })),
            "closing tick must not re-arm: {events:?}"
        );
    }

    #[test]
    fn late_evidence_resets_the_miss_counter() {
        let mut p = KeepalivePolicy::new(cfg(45_000, 2));
        p.on_transport_ready();
        p.on_tick(true); // grace
        assert!(has_probe(&p.on_tick(true))); // miss 1
        assert!(has_probe(&p.on_tick(true))); // miss 2
        p.mark_alive(); // peer answers just in time
        assert!(!has_probe(&p.on_tick(true))); // quiet tick
                                               // The full miss budget is available again.
        assert!(has_probe(&p.on_tick(true)));
        assert!(has_probe(&p.on_tick(true)));
        assert!(has_close(&p.on_tick(true)));
    }

    #[test]
    fn miss_limit_one_closes_on_second_silent_tick() {
        let mut p = KeepalivePolicy::new(cfg(1_000, 1));
        p.on_transport_ready();
        p.on_tick(true); // grace
        assert!(has_probe(&p.on_tick(true)));
        assert!(has_close(&p.on_tick(true)));
    }

    #[test]
    fn unknown_timer_id_is_logged_not_actioned() {
        let mut p = KeepalivePolicy::new(cfg(45_000, 2));
        p.on_transport_ready();
        let events = dispatch_tick(&mut p, TimerId(999), true);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], OutboundEvent::Log { .. }));
        assert!(!has_probe(&events) && !has_close(&events));
    }

    #[test]
    fn keepalive_tick_routes_through_dispatch() {
        let mut p = KeepalivePolicy::new(cfg(45_000, 2));
        p.on_transport_ready();
        dispatch_tick(&mut p, KEEPALIVE_TIMER, true); // grace
        assert!(has_probe(&dispatch_tick(&mut p, KEEPALIVE_TIMER, true)));
    }

    #[test]
    fn auto_ponging_but_never_binding_peer_is_closed_at_the_negotiation_limit() {
        let mut p = KeepalivePolicy::new(cfg(45_000, 2));
        p.on_transport_ready();
        // The peer's WS stack answers every probe (mark_alive), but the
        // session never reaches Ready — liveness must not grant
        // immortality to a socket that makes no session progress.
        for _ in 0..NEGOTIATION_TICK_LIMIT {
            p.mark_alive();
            let events = p.on_tick(false);
            assert!(!has_close(&events), "still within negotiation budget");
        }
        p.mark_alive();
        let events = p.on_tick(false);
        assert!(
            has_close(&events),
            "negotiation limit exhausted: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboundEvent::SetTimer { .. })),
            "negotiation close must not re-arm: {events:?}"
        );
    }

    #[test]
    fn reaching_ready_resets_the_negotiation_budget() {
        let mut p = KeepalivePolicy::new(cfg(45_000, 2));
        p.on_transport_ready();
        for _ in 0..NEGOTIATION_TICK_LIMIT {
            p.mark_alive();
            assert!(!has_close(&p.on_tick(false)));
        }
        // Session binds just in time; ready ticks clear the counter and
        // ordinary keepalive behavior takes over indefinitely.
        for _ in 0..10 {
            p.mark_alive();
            let events = p.on_tick(true);
            assert!(!has_close(&events));
            assert_rearm(&events, 45_000);
        }
    }

    #[test]
    fn silent_negotiating_peer_still_dies_by_miss_limit_first() {
        // A pre-bind peer that also stops ponging hits the ordinary
        // dead-peer close (miss limit) before the negotiation limit.
        let mut p = KeepalivePolicy::new(cfg(45_000, 1));
        p.on_transport_ready();
        assert!(!has_probe(&p.on_tick(false))); // grace
        assert!(has_probe(&p.on_tick(false))); // probe, unanswered
        assert!(has_close(&p.on_tick(false)));
    }
}
