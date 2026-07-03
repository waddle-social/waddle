//! RFC 7395 §3.8 keepalive — dedicated conformance suite (issue #1090).
//!
//! Exercises the liveness policy through the public
//! [`XmppStateMachine::handle`] entry point exactly as the WebSocket
//! transport adapter drives it: `TransportReady` at upgrade, `Tick`
//! from the adapter's timer wheel, `KeepaliveAck` for transport-level
//! evidence (pong / client ping), `FrameReceived` for client frames.
//! Everything here is pure and clock-free — no sockets, no tokio time.

use std::str::FromStr;
use waddle_xmpp::protocol::{
    InboundEvent, InboundFrame, KeepaliveConfig, OutboundEvent, StanzaDispatcher, TimerId,
    XmppStateMachine, KEEPALIVE_TIMER,
};

/// A machine still in stream negotiation (pre-bind), as at WS upgrade.
fn negotiating_machine(interval_ms: u64, miss_limit: u32) -> XmppStateMachine {
    let mut sm = XmppStateMachine::new("example.com", StanzaDispatcher::new());
    sm.set_keepalive_config(KeepaliveConfig {
        interval_ms,
        miss_limit,
    });
    sm
}

/// A machine with a bound session (`Ready`), as after bind — the
/// steady-state the keepalive spends its life in.
fn machine(interval_ms: u64, miss_limit: u32) -> XmppStateMachine {
    let mut sm = negotiating_machine(interval_ms, miss_limit);
    let jid = jid::FullJid::from_str("alice@example.com/web").expect("static test jid");
    sm.transition_to_ready(jid, false);
    sm
}

fn tick(sm: &mut XmppStateMachine) -> Vec<OutboundEvent> {
    sm.handle(InboundEvent::Tick(KEEPALIVE_TIMER))
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

fn has_rearm(events: &[OutboundEvent], interval_ms: u64) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            OutboundEvent::SetTimer { id, duration_ms }
                if *id == KEEPALIVE_TIMER && *duration_ms == interval_ms
        )
    })
}

#[test]
fn transport_ready_arms_the_keepalive_clock() {
    let mut sm = machine(45_000, 2);
    let events = sm.handle(InboundEvent::TransportReady);
    assert!(
        has_rearm(&events, 45_000),
        "TransportReady must arm the keepalive timer: {events:?}"
    );
    assert!(!has_probe(&events) && !has_close(&events));
}

#[test]
fn every_tick_rearms_until_close() {
    let mut sm = machine(45_000, 2);
    sm.handle(InboundEvent::TransportReady);
    // Grace tick + two probing ticks all re-arm; the closing tick must not.
    for _ in 0..3 {
        assert!(has_rearm(&tick(&mut sm), 45_000));
    }
    let closing = tick(&mut sm);
    assert!(has_close(&closing));
    assert!(!has_rearm(&closing, 45_000), "close must not re-arm");
}

#[test]
fn idle_connection_probes_and_pong_keeps_it_alive_forever() {
    let mut sm = machine(45_000, 2);
    sm.handle(InboundEvent::TransportReady);
    tick(&mut sm); // initial grace
    for _ in 0..20 {
        let events = tick(&mut sm);
        assert!(has_probe(&events), "silent tick must probe");
        assert!(!has_close(&events), "answered probes must never close");
        // The adapter feeds the client's pong as KeepaliveAck.
        assert!(sm.handle(InboundEvent::KeepaliveAck).is_empty());
        // Pong counted as evidence → the next tick is quiet.
        let quiet = tick(&mut sm);
        assert!(!has_probe(&quiet) && !has_close(&quiet));
    }
}

#[test]
fn dead_peer_is_closed_after_miss_limit_probes() {
    let mut sm = machine(45_000, 2);
    sm.handle(InboundEvent::TransportReady);
    tick(&mut sm); // grace
    assert!(has_probe(&tick(&mut sm))); // probe 1, unanswered
    assert!(has_probe(&tick(&mut sm))); // probe 2, unanswered
    let events = tick(&mut sm);
    assert!(
        has_close(&events),
        "miss limit exhausted must close: {events:?}"
    );
    assert!(!has_probe(&events));
}

#[test]
fn client_frames_count_as_liveness_evidence() {
    let mut sm = machine(45_000, 2);
    sm.handle(InboundEvent::TransportReady);
    tick(&mut sm); // grace
    tick(&mut sm); // probe 1
                   // A parsed client frame (stream open) arrives — the machine path
                   // marks alive without a separate KeepaliveAck.
    sm.handle(InboundEvent::FrameReceived(InboundFrame::Open));
    let events = tick(&mut sm);
    assert!(
        !has_probe(&events) && !has_close(&events),
        "frame evidence must reset the policy: {events:?}"
    );
}

#[test]
fn evidence_resets_the_full_miss_budget() {
    let mut sm = machine(45_000, 1);
    sm.handle(InboundEvent::TransportReady);
    tick(&mut sm); // grace
    assert!(has_probe(&tick(&mut sm))); // probe (budget exhausted)
    sm.handle(InboundEvent::KeepaliveAck);
    assert!(!has_probe(&tick(&mut sm))); // quiet
    assert!(has_probe(&tick(&mut sm))); // full budget again
    assert!(has_close(&tick(&mut sm)));
}

#[test]
fn unknown_timer_ids_are_ignored_by_the_policy() {
    let mut sm = machine(45_000, 2);
    sm.handle(InboundEvent::TransportReady);
    tick(&mut sm); // grace
    for _ in 0..10 {
        let events = sm.handle(InboundEvent::Tick(TimerId(999)));
        assert!(
            !has_probe(&events) && !has_close(&events),
            "foreign timer must not drive the keepalive policy: {events:?}"
        );
    }
    // The keepalive clock still behaves normally afterwards.
    assert!(has_probe(&tick(&mut sm)));
}

#[test]
fn keepalive_ack_is_effect_free() {
    let mut sm = machine(45_000, 2);
    sm.handle(InboundEvent::TransportReady);
    assert!(sm.handle(InboundEvent::KeepaliveAck).is_empty());
}

#[test]
fn auto_ponging_socket_that_never_binds_is_reaped() {
    // Every RFC 6455 stack auto-pongs below the application, so a
    // wedged-but-TCP-alive pre-auth socket answers every probe. The
    // negotiation deadline must reap it anyway — otherwise the
    // keepalive would hold unauthenticated sockets forever (the
    // gateway's idle reset used to bound them; issue #1090 must not
    // remove that bound).
    let mut sm = negotiating_machine(45_000, 2);
    sm.handle(InboundEvent::TransportReady);
    let mut closed_at = None;
    for round in 1..=10 {
        sm.handle(InboundEvent::KeepaliveAck); // auto-pong
        if has_close(&tick(&mut sm)) {
            closed_at = Some(round);
            break;
        }
    }
    assert_eq!(
        closed_at,
        Some(3),
        "pre-bind socket must close on the NEGOTIATION_TICK_LIMIT tick itself \
         (3 ticks = 135s at the default 45s interval)"
    );
}

#[test]
fn binding_before_the_negotiation_deadline_grants_normal_keepalive() {
    let mut sm = negotiating_machine(45_000, 2);
    sm.handle(InboundEvent::TransportReady);
    // Two negotiating ticks pass while the client authenticates.
    sm.handle(InboundEvent::KeepaliveAck);
    assert!(!has_close(&tick(&mut sm)));
    sm.handle(InboundEvent::KeepaliveAck);
    assert!(!has_close(&tick(&mut sm)));
    // Bind lands (the adapter replaces the machine at bind; phase
    // transition models the same outcome for the policy).
    let jid = jid::FullJid::from_str("alice@example.com/web").expect("static test jid");
    sm.transition_to_ready(jid, false);
    // A responsive bound session now lives indefinitely.
    for _ in 0..10 {
        sm.handle(InboundEvent::KeepaliveAck);
        let events = tick(&mut sm);
        assert!(!has_close(&events), "bound responsive session must live");
    }
}
