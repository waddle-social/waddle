use std::str::FromStr;

use jid::BareJid;
use minidom::Element;
use url::Url;
use waddle_xmpp_client::stream_management::{
    AckRequest, SentStanzaKind, SmAckHandledCountTooHigh, SmState, NS_SM,
};
use waddle_xmpp_client::{
    AccessToken, ClientConfig, ClientEvent, ClientRequest, ClientResource, ConnectionConfig,
    ConnectionEvent, OAuthBearerConfig, SmResumeState, StreamClose, StreamManagementEvent,
    TransportEvent, TransportMessage, TransportState, WebSocketConfig, XmppRuntime, NS_BIND,
    NS_CLIENT, NS_SASL, NS_STREAMS,
};

fn config() -> ClientConfig {
    ClientConfig::new(
        ConnectionConfig::new(BareJid::from_str("waddle.example").unwrap()),
        WebSocketConfig::new(Url::parse("wss://chat.example.com/ws").unwrap()).unwrap(),
        OAuthBearerConfig::new(
            BareJid::from_str("alice@example.com").unwrap(),
            ClientResource::new("browser").unwrap(),
            AccessToken::new("token"),
        )
        .unwrap(),
    )
    .unwrap()
}

fn message(id: &str) -> Element {
    Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .build()
}

fn enabled_state() -> SmState {
    let mut state = SmState::new();
    state.start_outbound();
    state.enabled = true;
    state
}

#[test]
fn xep0198_ack_cadence_uses_transport_confirmation_and_repeats_five_second_rung() {
    let mut state = enabled_state();
    let first = state.record_sent_stanza_at(&message("one"), 100);
    assert_eq!(
        first.request,
        Some(AckRequest {
            attempt: 1,
            unacked: 1,
        })
    );
    assert_eq!(
        state.next_ack_wakeup_in_ms(100),
        Some(30_000),
        "the five-second response clock cannot start before <r/> reaches transport"
    );

    assert!(state.confirm_ack_request_sent_at(1_000));
    let timeout = state.poll_ack_timer_at(6_000);
    assert!(timeout.request_timed_out);
    assert_eq!(timeout.request.map(|request| request.attempt), Some(2));

    let mut repeated = enabled_state();
    repeated.record_sent_stanza_at(&message("repeat"), 0);
    assert!(repeated.confirm_ack_request_sent_at(0));
    let mut now_ms = 0;
    for (delay_ms, expected_attempt) in [
        (250, 2),
        (500, 3),
        (1_000, 4),
        (2_000, 5),
        (5_000, 6),
        (5_000, 7),
    ] {
        repeated.process_ack_at(0, now_ms).unwrap();
        now_ms += delay_ms;
        assert_eq!(
            repeated
                .poll_ack_timer_at(now_ms)
                .request
                .map(|request| request.attempt),
            Some(expected_attempt)
        );
        assert!(repeated.confirm_ack_request_sent_at(now_ms));
    }
    assert_eq!(
        repeated.poll_ack_timer_at(30_000).progress_stalled_ms,
        Some(30_000)
    );
}

#[test]
fn xep0198_wrapping_ack_validation_is_atomic_and_replay_is_not_recounted() {
    let mut state = enabled_state();
    state.server_h = u32::MAX - 1;
    state.outbound_count = u32::MAX - 1;
    state.record_sent_stanza(&message("last-before-wrap"));
    state.record_sent_stanza(&message("first-after-wrap"));

    assert_eq!(
        state.process_ack(1),
        Err(SmAckHandledCountTooHigh {
            h: 1,
            send_count: 0,
        })
    );
    assert_eq!(state.server_h, u32::MAX - 1);
    assert_eq!(state.unacked_count(), 2);
    assert_eq!(state.process_ack(0).unwrap().len(), 2);

    let replay = message("byte-stable-replay");
    let resume =
        SmResumeState::from_unhandled_outbound_stanzas("session", 9, 1, [replay.clone()]).unwrap();
    let mut resumed = SmState::from_resume_state(&resume);
    resumed.enabled = true;
    resumed.outbound_enabled = true;
    resumed.begin_replay_transition_at(500);
    assert_eq!(resumed.mark_unhandled_for_replay(), vec![replay.clone()]);
    let sent = resumed.record_sent_stanza_at(&replay, 501);
    assert_eq!(sent.kind, SentStanzaKind::Replay);
    assert_eq!(resumed.outbound_count, 1);
    assert_eq!(resumed.inbound_count, 9);

    resumed.inbound_count = 44;
    resumed.start_inbound();
    assert_eq!(
        resumed.inbound_count, 0,
        "only a fresh <enabled/> sequence resets inbound h"
    );
}

#[test]
fn xep0198_inbound_request_emits_current_h_and_graceful_close_finishes_with_ack() {
    let mut runtime = establish_sm_runtime();
    for id in ["inbound-1", "inbound-2"] {
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                message(id),
            )))
            .unwrap();
    }

    let request_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            SmState::build_request_ack(),
        )))
        .unwrap();
    assert!(request_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.name() == "a"
            && element.ns() == NS_SM
            && element.attr("h") == Some("2")
    )));
    assert!(request_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckRequested
        ))
    )));

    let close_events = runtime.request_stream_close().unwrap();
    let outbound = close_events
        .iter()
        .filter_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(message)) => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(outbound.len(), 2);
    assert!(matches!(
        outbound[0],
        TransportMessage::Element(ack)
            if ack.name() == "a" && ack.attr("h") == Some("2")
    ));
    assert!(matches!(outbound[1], TransportMessage::Close(StreamClose)));
}

#[test]
fn xep0198_peer_close_reciprocates_after_final_inbound_ack() {
    let mut runtime = establish_sm_runtime();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            message("peer-close-inbound"),
        )))
        .unwrap();

    let close_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Close(
            StreamClose,
        )))
        .unwrap();
    let outbound = close_events
        .iter()
        .filter_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(message)) => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(outbound.len(), 2);
    assert!(matches!(
        outbound[0],
        TransportMessage::Element(ack)
            if ack.name() == "a"
                && ack.ns() == NS_SM
                && ack.attr("h") == Some("1")
    ));
    assert!(matches!(outbound[1], TransportMessage::Close(StreamClose)));
}

fn establish_sm_runtime() -> XmppRuntime {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    runtime.queue_request(ClientRequest::Connect).unwrap();
    runtime
        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            waddle_xmpp_client::StreamOpen::from_server(
                BareJid::from_str("waddle.example").unwrap(),
            ),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("features", NS_STREAMS)
                .append(
                    Element::builder("mechanisms", NS_SASL)
                        .append(
                            Element::builder("mechanism", NS_SASL)
                                .append("OAUTHBEARER")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("success", NS_SASL).build(),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            waddle_xmpp_client::StreamOpen::from_server(
                BareJid::from_str("waddle.example").unwrap(),
            ),
        )))
        .unwrap();
    let bind_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("features", NS_STREAMS)
                .append(Element::builder("bind", NS_BIND).build())
                .append(Element::builder("sm", NS_SM).build())
                .build(),
        )))
        .unwrap();
    let bind_id = bind_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(request)) => {
                Some(request.stanza_id.clone())
            }
            _ => None,
        })
        .expect("resource binding request");
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("iq", NS_CLIENT)
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    bind_id.as_str(),
                )
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
                .append(
                    Element::builder("bind", NS_BIND)
                        .append(
                            Element::builder("jid", NS_BIND)
                                .append("alice@example.com/browser")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("enabled", NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    "browser-sm-session",
                )
                .build(),
        )))
        .unwrap();
    runtime
}
