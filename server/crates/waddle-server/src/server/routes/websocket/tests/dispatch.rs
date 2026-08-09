use super::super::{
    interpret_loop::build_interpret_deps,
    replay::{drain_outbound_into_replay, drive_interpret_loop},
    stanza_to_xml,
    state::WsConnState,
};
use super::create_test_websocket_state;
use tokio::sync::mpsc;
use waddle_xmpp::{
    protocol::{Blocklist, InboundEvent, OutboundEvent},
    registry::OutboundStanza,
    Stanza,
};

// ---------------------------------------------------------------
// #229 PR11 - DeliveryKind dispatch in the per-connection main loop
// ---------------------------------------------------------------
//
// The actual main-loop entry point is `xmpp_websocket_handler`, an
// async function tied to a real WebSocket sink. To test the
// dispatch logic in isolation we exercise its two helpers
// (`build_interpret_deps`, `drive_interpret_loop`) and the
// `WsConnState::ensure_state_machine` lifecycle directly. End-to-
// end coverage of the routing flow lands once PR12 emits
// `OutboundStanza::peer_stanza` from `RouteToConnection`.

#[tokio::test]
async fn drive_interpret_loop_resolves_send_stanza_into_wire_frames() {
    // Recipient pass produces `OutboundEvent::SendStanza` for the
    // wire write. Drive the loop with a single SendStanza event
    // and assert it serializes cleanly into a frame (no extra
    // round-trips through the SM since no callback feedback is
    // produced).
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        jid,
        false,
        Blocklist::empty(),
    );
    let sm = conn.state_machine.as_mut().expect("SM");

    let mut msg =
        xmpp_parsers::message::Message::new(Some("alice@example.com".parse().expect("to jid")));
    msg.from = Some("bob@example.com/desk".parse().expect("from jid"));
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "hello".to_string());

    let initial_events = vec![OutboundEvent::SendStanza(Box::new(Stanza::Message(msg)))];
    let deps = build_interpret_deps(state.as_ref(), None);
    let drive = drive_interpret_loop(initial_events, sm, &deps).await;
    let (frames, close) = (drive.frames, drive.close);

    assert!(!close, "SendStanza alone never requests transport close");
    assert_eq!(frames.len(), 1, "single SendStanza -> single wire frame");
    assert!(
        frames[0].contains("hello"),
        "wire frame carries the message body; got {:?}",
        frames[0]
    );
}

#[tokio::test]
async fn drive_interpret_loop_runs_recipient_pass_for_peer_message() {
    // Production-shape regression: feed `InboundEvent::StanzaFromPeer`
    // through a Ready state machine and drive the resulting events
    // via `drive_interpret_loop`. The recipient pass MUST produce
    // a wire frame containing bob's recipient-side `<stanza-id>`
    // stamp so XEP-0359 §5 conformance is preserved end-to-end
    // through the production helpers.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let bob_full: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        bob_full,
        false,
        Blocklist::empty(),
    );
    let sm = conn.state_machine.as_mut().expect("SM");

    let mut peer_msg =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    peer_msg.from = Some("alice@example.com/web".parse().expect("from jid"));
    peer_msg.type_ = xmpp_parsers::message::MessageType::Chat;
    peer_msg.id = Some(xmpp_parsers::message::Id("alice-wire-id".to_string()));
    peer_msg
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "hi bob".to_string());
    // Pre-stamp alice's sender-side stanza-id so we can verify
    // the recipient pass *adds* bob's stamp rather than replacing
    // alice's (XEP-0359 §5 cross-archive preservation).
    peer_msg
        .payloads
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            "alice-A1",
            &"alice@example.com".parse::<jid::Jid>().expect("jid"),
        ));

    let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Message(
        peer_msg,
    ))));
    let deps = build_interpret_deps(state.as_ref(), None);
    let frames = drive_interpret_loop(events, sm, &deps).await.frames;

    // Recipient pass terminates with at least one SendStanza
    // carrying bob's stamp.
    assert!(
        !frames.is_empty(),
        "recipient pass must produce at least one wire frame"
    );
    let combined = frames.join("\n");
    assert!(
        combined.contains("by='bob@example.com'"),
        "recipient-pass wire frame must carry bob's stanza-id stamp; got: {combined}"
    );
    assert!(
        combined.contains("alice-A1"),
        "recipient-pass wire frame must preserve alice's cross-archive stanza-id; \
             got: {combined}"
    );
    assert!(
        combined.contains("hi bob"),
        "recipient-pass wire frame must carry the message body; got: {combined}"
    );
}

#[tokio::test]
async fn drain_outbound_dispatches_direct_frame_into_unacked_unchanged() {
    // Regression for the detach-drain DeliveryKind dispatch
    // Qodo flagged on PR269: DirectFrame values must be recorded
    // byte-for-byte (no recipient pipeline). This is the live
    // contract the SM-resume replay path depends on.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        jid,
        false,
        Blocklist::empty(),
    );
    // Enable SM tracking so `record_outbound` actually retains the
    // drained XML.
    conn.sm_state.enabled = true;

    let mut msg =
        xmpp_parsers::message::Message::new(Some("alice@example.com".parse().expect("to jid")));
    msg.from = Some("bob@example.com/desk".parse().expect("from jid"));
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "plain".to_string());
    let expected_xml = stanza_to_xml(&Stanza::Message(msg.clone()));

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    tx.send(OutboundStanza::new(Stanza::Message(msg)))
        .await
        .expect("send");
    drop(tx); // close so try_recv eventually returns Empty

    drain_outbound_into_replay(
        state.as_ref(),
        conn.state_machine.as_mut(),
        &mut conn.sm_state,
        None,
        &mut rx,
        None,
        super::super::replay::PendingRowDrainPolicy::PreserveForReplay,
    )
    .await;

    let queue = conn.sm_state.get_stanzas_to_resend(0);
    assert_eq!(queue.len(), 1, "DirectFrame recorded once");
    assert_eq!(
        queue[0].stanza_xml, expected_xml,
        "DirectFrame is recorded byte-for-byte (no recipient pipeline rewrite)"
    );
}

#[tokio::test]
async fn drain_outbound_dispatches_peer_stanza_through_recipient_pass() {
    // PeerStanza values queued during detach must run through
    // the recipient pass before being recorded in the SM unacked
    // queue, so a resumed connection's replay carries the
    // recipient-side `<stanza-id>` stamp. Without the dispatch
    // (Qodo's flagged bug), the queued bytes would be the raw
    // peer stanza missing bob's stamp.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let bob_full: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        bob_full,
        false,
        Blocklist::empty(),
    );
    conn.sm_state.enabled = true;

    let mut peer_msg =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    peer_msg.from = Some("alice@example.com/web".parse().expect("from jid"));
    peer_msg.type_ = xmpp_parsers::message::MessageType::Chat;
    peer_msg.id = Some(xmpp_parsers::message::Id("alice-wire-id".to_string()));
    peer_msg.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "hi from drain".to_string(),
    );
    peer_msg
        .payloads
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            "alice-A1",
            &"alice@example.com".parse::<jid::Jid>().expect("jid"),
        ));

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    tx.send(OutboundStanza::peer_stanza(Stanza::Message(peer_msg)))
        .await
        .expect("send");
    drop(tx);

    drain_outbound_into_replay(
        state.as_ref(),
        conn.state_machine.as_mut(),
        &mut conn.sm_state,
        None,
        &mut rx,
        None,
        super::super::replay::PendingRowDrainPolicy::PreserveForReplay,
    )
    .await;

    let queue = conn.sm_state.get_stanzas_to_resend(0);
    assert!(
        !queue.is_empty(),
        "PeerStanza drain MUST record at least the recipient-pass wire frame"
    );
    let combined: String = queue
        .iter()
        .map(|entry| entry.stanza_xml.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        combined.contains("by='bob@example.com'"),
        "drained PeerStanza replay must carry bob's recipient-side stanza-id; got: {combined}"
    );
    assert!(
        combined.contains("alice-A1"),
        "drained PeerStanza replay must preserve alice's cross-archive stamp; got: {combined}"
    );
}
