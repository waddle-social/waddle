//! RFC 7395 §5.6 keepalive — dedicated conformance suite (issue #1090).
//!
//! Exercises the liveness policy through the public
//! [`XmppStateMachine::handle`] entry point exactly as the WebSocket
//! transport adapter drives it: `TransportReady` at upgrade, `Tick`
//! from the adapter's timer wheel, `KeepaliveAck` for transport-level
//! evidence (pong / client ping), `FrameReceived` for client frames.
//! Everything here is pure and clock-free — no sockets, no tokio time.

use waddle_xmpp::protocol::{
    InboundEvent, InboundFrame, KeepaliveConfig, OutboundEvent, StanzaDispatcher, TimerId,
    XmppStateMachine, KEEPALIVE_TIMER,
};

fn machine(interval_ms: u64, miss_limit: u32) -> XmppStateMachine {
    let mut sm = XmppStateMachine::new("example.com", StanzaDispatcher::new());
    sm.set_keepalive_config(KeepaliveConfig {
        interval_ms,
        miss_limit,
    });
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
