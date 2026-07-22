use std::str::FromStr;

use jid::{BareJid, FullJid};
use minidom::Element;
use url::Url;

use super::*;
use crate::bootstrap::{NS_BIND, NS_SASL, NS_STREAMS};
use crate::config::{AccessToken, ClientResource, OAuthBearerConfig, WebSocketConfig};
use crate::{
    ConnectionConfig, SmResumeState, StreamErrorCondition, StreamId, StreamManagementEvent,
};

fn config() -> ClientConfig {
    ClientConfig::new(
        ConnectionConfig::new(BareJid::from_str("waddle.example").unwrap()),
        WebSocketConfig::new(Url::parse("wss://chat.example.com/ws").unwrap()).unwrap(),
        OAuthBearerConfig::new(
            BareJid::from_str("alice@example.com").unwrap(),
            ClientResource::new("macbook").unwrap(),
            AccessToken::new("token"),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn runtime_updates_state_when_connecting() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let events = runtime.queue_request(ClientRequest::Connect).unwrap();
    assert_eq!(runtime.snapshot().phase, SessionPhase::Connecting);
    assert_eq!(events.len(), 2);
}

#[test]
fn runtime_emits_initial_open_when_transport_opens() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    runtime.queue_request(ClientRequest::Connect).unwrap();
    let events = runtime
        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .unwrap();

    assert_eq!(runtime.snapshot().phase, SessionPhase::OpeningStream);
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamOpening(open))
            if open.to.as_ref() == Some(&BareJid::from_str("waddle.example").unwrap())
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(TransportMessage::Open(_)))
    )));
}

#[test]
fn app_stanza_routes_jmi_message_to_typed_call_event() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='alice@waddle.test/desktop' to='bob@waddle.test'>
        <propose xmlns='urn:xmpp:jingle-message:0' id='call-1'>
          <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
        </propose>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        ClientEvent::Call(call) if call.sid.0 == "call-1"
    ));
}

#[test]
fn app_stanza_emits_standalone_pubsub_retract_and_summary_events() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element = "<message xmlns='jabber:client' type='headline' \
            from='community.waddle.test' to='alice@example.com'>\
        <event xmlns='http://jabber.org/protocol/pubsub#event'>\
          <items node='urn:xmpp:pubsub-social-feed:stories:0'>\
            <retract id='story-1'/>\
            <item id='story-2'>\
              <summary xmlns='urn:xmpp:pubsub-attachments:summary:1'>\
                <reactions><reaction count='2'>👍</reaction></reactions>\
              </summary>\
            </item>\
          </items>\
        </event>\
    </message>"
        .parse()
        .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    // The message event still carries the full pubsub_events list …
    assert!(matches!(
        &events[0],
        ClientEvent::Messaging(crate::messaging::MessagingEvent::Message(message))
            if message.pubsub_events.len() == 1
    ));
    // … and the two state transitions are also emitted standalone.
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::PubsubItemsRetracted(retracted)
            if retracted.node == "urn:xmpp:pubsub-social-feed:stories:0"
                && retracted.item_ids == vec!["story-1".to_owned()]
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::PubsubAttachmentSummary(update)
            if update.item_id.as_deref() == Some("story-2")
                && update.summary.reactions.len() == 1
                && update.summary.reactions[0].count == 2
    )));
    assert_eq!(events.len(), 3);
}

#[test]
fn app_stanza_answers_client_caps_disco_info() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    runtime.snapshot.phase = SessionPhase::Established;
    runtime.snapshot.binding = Some(SessionBinding {
        jid: FullJid::from_str("alice@example.com/macbook").unwrap(),
        stream_id: None,
        resumable: false,
    });
    let stanza = Element::builder("iq", crate::bootstrap::NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "caps-1")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "bob@example.com/phone",
        )
        .append(
            Element::builder("query", crate::discovery::DISCO_INFO_NS)
                .attr(
                    minidom::rxml::xml_ncname!("node").to_owned(),
                    crate::caps::client_caps_node_ver(),
                )
                .build(),
        )
        .build();

    let events = runtime.handle_app_stanza(&stanza);

    assert_eq!(events.len(), 1);
    let ClientEvent::Connection(ConnectionEvent::OutboundMessage(TransportMessage::Element(
        response,
    ))) = &events[0]
    else {
        panic!("expected outbound disco response, got {:?}", events[0]);
    };
    assert_eq!(response.attr("type"), Some("result"));
    assert_eq!(response.attr("id"), Some("caps-1"));
    assert_eq!(response.attr("from"), Some("alice@example.com/macbook"));
    let query = response
        .get_child("query", crate::discovery::DISCO_INFO_NS)
        .expect("disco query present");
    assert!(query.children().any(|child| {
        child.name() == "feature" && child.attr("var") == Some(crate::mds::NS_MDS_NOTIFY)
    }));
}

#[test]
fn app_stanza_routes_carbon_wrapped_jmi_to_typed_call_event() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='alice@example.com' to='alice@example.com/macbook'>
        <sent xmlns='urn:xmpp:carbons:2'>
          <forwarded xmlns='urn:xmpp:forward:0'>
            <message xmlns='jabber:client' from='alice@example.com/phone' to='bob@example.com/desktop'>
              <finish xmlns='urn:xmpp:jingle-message:0' id='call-1'/>
            </message>
          </forwarded>
        </sent>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        ClientEvent::Call(call)
            if call.sid.0 == "call-1"
                && call.from.to_string() == "alice@example.com/phone"
                && call.to.as_ref().map(ToString::to_string).as_deref() == Some("bob@example.com/desktop")
    ));
}

#[test]
fn app_stanza_ignores_forged_carbon_wrapped_call_from_peer() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='bob@example.com' to='alice@example.com/macbook'>
        <sent xmlns='urn:xmpp:carbons:2'>
          <forwarded xmlns='urn:xmpp:forward:0'>
            <message xmlns='jabber:client' from='alice@example.com/phone' to='bob@example.com/desktop'>
              <finish xmlns='urn:xmpp:jingle-message:0' id='call-1'/>
            </message>
          </forwarded>
        </sent>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ClientEvent::Call(_))),
        "XEP-0280 carbon copies must come from the authenticated account bare JID"
    );
}

// ─── XEP-0280 normal-message carbon unwrap (#1243) ─────────────────────────

#[test]
fn app_stanza_unwraps_received_carbon_into_inner_message() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='alice@example.com' to='alice@example.com/macbook' type='chat'>
        <received xmlns='urn:xmpp:carbons:2'>
          <forwarded xmlns='urn:xmpp:forward:0'>
            <message xmlns='jabber:client' from='bob@example.com/desktop' to='alice@example.com/phone' type='chat' id='m1'>
              <body>hi alice</body>
            </message>
          </forwarded>
        </received>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    assert_eq!(events.len(), 1);
    let ClientEvent::Messaging(crate::messaging::MessagingEvent::Message(message)) = &events[0]
    else {
        panic!("expected inner message event, got {:?}", events[0]);
    };
    assert_eq!(message.body.as_deref(), Some("hi alice"));
    assert_eq!(message.from.as_deref(), Some("bob@example.com/desktop"));
    assert_eq!(message.to.as_deref(), Some("alice@example.com/phone"));
    assert_eq!(message.id.as_deref(), Some("m1"));
    assert_eq!(
        message.carbon,
        Some(crate::messaging::CarbonDirection::Received)
    );
}

#[test]
fn app_stanza_unwraps_sent_carbon_into_inner_message() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='alice@example.com' to='alice@example.com/macbook' type='chat'>
        <sent xmlns='urn:xmpp:carbons:2'>
          <forwarded xmlns='urn:xmpp:forward:0'>
            <message xmlns='jabber:client' from='alice@example.com/phone' to='bob@example.com' type='chat' id='m2'>
              <body>sent elsewhere</body>
            </message>
          </forwarded>
        </sent>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    assert_eq!(events.len(), 1);
    let ClientEvent::Messaging(crate::messaging::MessagingEvent::Message(message)) = &events[0]
    else {
        panic!("expected inner message event, got {:?}", events[0]);
    };
    assert_eq!(message.body.as_deref(), Some("sent elsewhere"));
    assert_eq!(message.from.as_deref(), Some("alice@example.com/phone"));
    assert_eq!(
        message.carbon,
        Some(crate::messaging::CarbonDirection::Sent)
    );
}

/// XEP-0297 §5: the `<forwarded/>` wrapper's `<delay/>` is the original
/// delivery time; the unwrapped inner message must carry it when the
/// inner element has no delay of its own (#1267 item 6).
#[test]
fn app_stanza_propagates_forwarded_delay_onto_unwrapped_carbon() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='alice@example.com' to='alice@example.com/macbook' type='chat'>
        <received xmlns='urn:xmpp:carbons:2'>
          <forwarded xmlns='urn:xmpp:forward:0'>
            <delay xmlns='urn:xmpp:delay' stamp='2026-07-01T10:00:00Z'/>
            <message xmlns='jabber:client' from='bob@example.com/desktop' to='alice@example.com/phone' type='chat' id='m3'>
              <body>delayed copy</body>
            </message>
          </forwarded>
        </received>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    let ClientEvent::Messaging(crate::messaging::MessagingEvent::Message(message)) = &events[0]
    else {
        panic!("expected inner message event, got {:?}", events[0]);
    };
    assert_eq!(
        message.timestamp.map(|t| t.to_rfc3339()),
        Some("2026-07-01T10:00:00+00:00".to_string())
    );
}

/// XEP-0280 §11: carbon envelopes not from the account's own bare JID
/// MUST be ignored — including the outer envelope, which must not fall
/// through to the messaging parser as a body-less phantom message.
#[test]
fn app_stanza_ignores_forged_carbon_wrapped_message_entirely() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='mallory@example.com' to='alice@example.com/macbook' type='chat'>
        <received xmlns='urn:xmpp:carbons:2'>
          <forwarded xmlns='urn:xmpp:forward:0'>
            <message xmlns='jabber:client' from='bob@example.com/desktop' to='alice@example.com/phone' type='chat' id='f1'>
              <body>forged</body>
            </message>
          </forwarded>
        </received>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    assert!(
        events.is_empty(),
        "forged carbon must be fully ignored, got {events:?}"
    );
}

/// XEP-0280 §6.1/§6.2: the wrapping 'from' MUST be the account's BARE
/// JID. A carbon-shaped stanza from one of the account's own FULL JIDs
/// (a sibling resource, not the server) must be ignored, or an
/// authenticated device could smuggle an inner message forged as any
/// sender.
#[test]
fn app_stanza_ignores_carbon_envelope_from_own_full_jid() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='alice@example.com/attacker' to='alice@example.com/macbook' type='chat'>
        <received xmlns='urn:xmpp:carbons:2'>
          <forwarded xmlns='urn:xmpp:forward:0'>
            <message xmlns='jabber:client' from='bob@example.com/desktop' to='alice@example.com/phone' type='chat' id='f2'>
              <body>forged via sibling resource</body>
            </message>
          </forwarded>
        </received>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    assert!(
        events.is_empty(),
        "full-JID carbon envelope must be fully ignored, got {events:?}"
    );
}

/// Same strictness on the call-carbon path: a JMI event wrapped in a
/// carbon envelope from the account's own FULL JID is not a
/// server-generated carbon.
#[test]
fn app_stanza_ignores_carbon_wrapped_call_from_own_full_jid() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='alice@example.com/attacker' to='alice@example.com/macbook'>
        <sent xmlns='urn:xmpp:carbons:2'>
          <forwarded xmlns='urn:xmpp:forward:0'>
            <message xmlns='jabber:client' from='alice@example.com/phone' to='bob@example.com/desktop'>
              <finish xmlns='urn:xmpp:jingle-message:0' id='call-9'/>
            </message>
          </forwarded>
        </sent>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ClientEvent::Call(_))),
        "full-JID carbon envelope must not surface a call event"
    );
}

/// A directly received message (no carbon envelope) must not be stamped
/// with a carbon direction.
#[test]
fn app_stanza_leaves_direct_messages_unstamped() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element =
        r#"<message xmlns='jabber:client' from='bob@example.com/desktop' to='alice@example.com/macbook' type='chat' id='d1'>
        <body>direct</body>
    </message>"#
            .parse()
            .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    let ClientEvent::Messaging(crate::messaging::MessagingEvent::Message(message)) = &events[0]
    else {
        panic!("expected message event, got {:?}", events[0]);
    };
    assert_eq!(message.carbon, None);
    assert_eq!(message.body.as_deref(), Some("direct"));
}

#[test]
fn app_stanza_acknowledges_jingle_iq_set_and_surfaces_call_event() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    let stanza: Element = r#"<iq xmlns='jabber:client' type='set' id='j1' from='alice@waddle.test/desktop' to='bob@waddle.test/phone'>
        <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate' sid='call-1'>
          <reason><success/></reason>
        </jingle>
    </iq>"#
        .parse()
        .unwrap();

    let events = runtime.handle_app_stanza(&stanza);

    assert_eq!(events.len(), 2);
    match &events[0] {
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(TransportMessage::Element(
            ack,
        ))) => {
            assert_eq!(ack.name(), "iq");
            assert_eq!(ack.attr("type"), Some("result"));
            assert_eq!(ack.attr("id"), Some("j1"));
            assert_eq!(ack.attr("to"), Some("alice@waddle.test/desktop"));
            assert_eq!(ack.attr("from"), Some("bob@waddle.test/phone"));
        }
        other => panic!("expected outbound IQ ack, got {other:?}"),
    }
    assert!(matches!(
        &events[1],
        ClientEvent::Call(call) if call.sid.0 == "call-1"
    ));
}

#[test]
fn runtime_bootstraps_auth_bind_and_ready_state() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    runtime.queue_request(ClientRequest::Connect).unwrap();
    runtime
        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
        )))
        .unwrap();

    let auth_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            pre_auth_features(),
        )))
        .unwrap();
    assert_eq!(runtime.snapshot().phase, SessionPhase::Authenticating);
    assert!(auth_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::AuthenticationRequested(
            AuthenticationRequest::OAuthBearer(_)
        ))
    )));

    let reopen_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("success", NS_SASL).build(),
        )))
        .unwrap();
    assert!(reopen_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::AuthenticationSucceeded)
    )));
    assert!(reopen_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(TransportMessage::Open(_)))
    )));

    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
        )))
        .unwrap();
    let bind_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features(),
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
        .unwrap();
    assert_eq!(runtime.snapshot().phase, SessionPhase::Binding);

    let ready_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();

    assert_eq!(runtime.snapshot().phase, SessionPhase::Established);
    assert_eq!(runtime.snapshot().client_state(), crate::ClientState::Ready);
    assert_eq!(
        runtime.snapshot().binding,
        Some(SessionBinding {
            jid: FullJid::from_str("alice@example.com/macbook").unwrap(),
            stream_id: None,
            resumable: false,
        })
    );
    assert!(ready_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::SessionReady(binding))
            if binding.jid == FullJid::from_str("alice@example.com/macbook").unwrap()
    )));
    assert!(ready_events.iter().any(|event| matches!(
        event,
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(binding))
            if binding.jid == FullJid::from_str("alice@example.com/macbook").unwrap()
    )));
}

#[test]
fn runtime_requests_sm_resume_before_resource_binding_when_resume_state_exists() {
    let mut config = config();
    config.session.stream_management.resume_state =
        Some(SmResumeState::new("old-sm-id", 7, 12).unwrap());
    let mut runtime = XmppRuntime::new(config).unwrap();
    drive_to_authenticated_stream(&mut runtime);

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    assert_eq!(runtime.snapshot().phase, SessionPhase::Resuming);

    let outbound_elements: Vec<&Element> = events
        .iter()
        .filter_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) => Some(element),
            _ => None,
        })
        .collect();

    let resume = outbound_elements
        .iter()
        .find(|element| {
            element.name() == "resume" && element.ns() == crate::stream_management::NS_SM
        })
        .expect("resume element before bind");
    assert_eq!(resume.attr("previd"), Some("old-sm-id"));
    assert_eq!(resume.attr("h"), Some("7"));

    assert!(
        !outbound_elements.iter().any(|element| {
            element.name() == "iq" && element.get_child("bind", NS_BIND).is_some()
        }),
        "resume must be attempted before resource binding"
    );
}

#[test]
fn runtime_establishes_session_from_successful_sm_resume_without_binding() {
    let mut config = config();
    config.session.stream_management.resume_state =
        Some(SmResumeState::new("old-sm-id", 7, 12).unwrap());
    let mut runtime = XmppRuntime::new(config).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("resumed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("previd").to_owned(), "old-sm-id")
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "12")
                .build(),
        )))
        .unwrap();

    assert_eq!(runtime.snapshot().phase, SessionPhase::Established);
    assert_eq!(
        runtime.snapshot().binding,
        Some(SessionBinding {
            jid: FullJid::from_str("alice@example.com/macbook").unwrap(),
            stream_id: Some(StreamId::new("old-sm-id")),
            resumable: true,
        })
    );
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Resumed { h: 12 }
        ))
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(_))
            | ClientEvent::Connection(ConnectionEvent::ResourceBound(_))
    )));
}

#[test]
fn runtime_rejects_resumed_without_required_h() {
    let mut config = config();
    config.session.stream_management.resume_state =
        Some(SmResumeState::new("old-sm-id", 7, 12).unwrap());
    let mut runtime = XmppRuntime::new(config).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("resumed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("previd").to_owned(), "old-sm-id")
                .build(),
        )))
        .unwrap();

    assert_eq!(runtime.snapshot().phase, SessionPhase::Resuming);
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed
        ))
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(TransportMessage::Close(_)))
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::SessionReady(_))
            | ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_))
    )));
}

#[test]
fn runtime_rejects_resumed_with_mismatched_previd() {
    let mut config = config();
    config.session.stream_management.resume_state =
        Some(SmResumeState::new("old-sm-id", 7, 12).unwrap());
    let mut runtime = XmppRuntime::new(config).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("resumed", crate::stream_management::NS_SM)
                .attr(
                    minidom::rxml::xml_ncname!("previd").to_owned(),
                    "different-sm-id",
                )
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "12")
                .build(),
        )))
        .unwrap();

    assert_eq!(runtime.snapshot().phase, SessionPhase::Resuming);
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed
        ))
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.name() == "error"
            && element
                .get_child("bad-request", "urn:ietf:params:xml:ns:xmpp-streams")
                .is_some()
    )));
}

#[test]
fn runtime_falls_back_to_resource_binding_when_sm_resume_fails() {
    let mut config = config();
    config.session.stream_management.resume_state =
        Some(SmResumeState::new("old-sm-id", 7, 12).unwrap());
    let mut runtime = XmppRuntime::new(config).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "12")
                .build(),
        )))
        .unwrap();

    assert_eq!(runtime.snapshot().phase, SessionPhase::Binding);
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed
        ))
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(_))
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.name() == "iq" && element.get_child("bind", NS_BIND).is_some()
    )));
}

#[test]
fn runtime_fresh_sm_session_ack_counts_only_current_session_inbound_stanzas() {
    // Issue #1181: a failed resume followed by a fresh bind + <enable/>
    // starts a NEW XEP-0198 session. XEP-0198 §5 zeroes both counters at
    // session start, so the h reported to the new stream must not carry
    // the previous session's received-stanza count (7 here).
    let mut config = config();
    config.session.stream_management.resume_state =
        Some(SmResumeState::new("old-sm-id", 7, 12).unwrap());
    let mut runtime = XmppRuntime::new(config).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let failed_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failed", crate::stream_management::NS_SM).build(),
        )))
        .unwrap();
    let bind_id = failed_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(request)) => {
                Some(request.stanza_id.clone())
            }
            _ => None,
        })
        .expect("bind requested after failed resume");
    let ready_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();
    let enable = ready_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.name() == "enable" && element.ns() == crate::stream_management::NS_SM => {
                Some(element.clone())
            }
            _ => None,
        })
        .expect("fresh stream management enable");
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            enable,
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("enabled", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "new-sm-id")
                .build(),
        )))
        .unwrap();

    for id in ["inbound-1", "inbound-2"] {
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("message", crate::NS_CLIENT)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                    .build(),
            )))
            .unwrap();
    }

    let ack_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("r", crate::stream_management::NS_SM).build(),
        )))
        .unwrap();

    let ack = ack_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.name() == "a" && element.ns() == crate::stream_management::NS_SM => {
                Some(element.clone())
            }
            _ => None,
        })
        .expect("ack in response to <r/>");
    assert_eq!(
        ack.attr("h"),
        Some("2"),
        "fresh SM session must count only stanzas received on the new session, \
         not carry the previous session's inbound count"
    );
}

#[test]
fn runtime_resume_after_fresh_sm_session_reports_only_current_session_inbound() {
    // Issue #1181 prod loop: failed resume → fresh enable → detach →
    // resume. The h in the next <resume/> is what the server compares
    // against its per-session send count; a carried-over count gets the
    // resume rejected with handled-count-too-high.
    let mut first_config = config();
    first_config.session.stream_management.resume_state =
        Some(SmResumeState::new("old-sm-id", 7, 12).unwrap());
    let mut runtime = XmppRuntime::new(first_config).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();
    let failed_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failed", crate::stream_management::NS_SM).build(),
        )))
        .unwrap();
    let bind_id = failed_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(request)) => {
                Some(request.stanza_id.clone())
            }
            _ => None,
        })
        .expect("bind requested after failed resume");
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("enabled", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "new-sm-id")
                .build(),
        )))
        .unwrap();
    for id in ["inbound-1", "inbound-2"] {
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("message", crate::NS_CLIENT)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                    .build(),
            )))
            .unwrap();
    }

    // Transport drops; the snapshot carries the session into a new runtime.
    let resume_state = runtime.resume_state().expect("resume state");
    let mut next_config = config();
    next_config.session.stream_management.resume_state = Some(resume_state);
    let mut next_runtime = XmppRuntime::new(next_config).unwrap();
    drive_to_authenticated_stream(&mut next_runtime);
    let events = next_runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let resume = events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.name() == "resume" && element.ns() == crate::stream_management::NS_SM => {
                Some(element.clone())
            }
            _ => None,
        })
        .expect("resume element");
    assert_eq!(resume.attr("previd"), Some("new-sm-id"));
    assert_eq!(
        resume.attr("h"),
        Some("2"),
        "resume h must never exceed what the server sent on the new session"
    );
}

#[test]
fn runtime_rejects_duplicate_enabled_on_live_sm_session() {
    // A stray/duplicate <enabled/> mid-session must be a protocol
    // violation (mirroring unexpected <resumed/>), NOT a silent
    // re-establish: re-running the XEP-0198 §5 counter reset on a live
    // session would drive the client's next <a h/> backwards on the
    // wire (issue #1181 adversarial review).
    let mut runtime = XmppRuntime::new(config()).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    let bind_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
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
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();
    let enabled = Element::builder("enabled", crate::stream_management::NS_SM)
        .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "sm-1")
        .build();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            enabled.clone(),
        )))
        .unwrap();
    for id in ["live-1", "live-2", "live-3"] {
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("message", crate::NS_CLIENT)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                    .build(),
            )))
            .unwrap();
    }

    let duplicate_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            enabled,
        )))
        .unwrap();

    assert!(
        duplicate_events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element)
            )) if element.name() == "error"
                && element
                    .get_child("bad-request", "urn:ietf:params:xml:ns:xmpp-streams")
                    .is_some()
        )),
        "duplicate <enabled/> on a live SM session must be answered with a \
         stream error, not silently re-establish the session"
    );
}

#[test]
fn runtime_resume_h_stays_consistent_across_repeated_resume_cycles() {
    // Issue #1181 conformance cycle: enable → receive → detach → resume →
    // receive → detach → resume. Each <resume/> h must equal exactly the
    // stanzas received across the SAME SM session (successful resumes
    // continue the session; the counter carries forward, never inflates).
    let mut runtime = XmppRuntime::new(config()).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    let bind_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
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
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("enabled", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "sm-1")
                .build(),
        )))
        .unwrap();
    for id in ["a-1", "a-2", "a-3"] {
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("message", crate::NS_CLIENT)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                    .build(),
            )))
            .unwrap();
    }

    // First detach → resume.
    let resume_state = runtime.resume_state().expect("first resume state");
    let mut second_config = config();
    second_config.session.stream_management.resume_state = Some(resume_state);
    let mut second = XmppRuntime::new(second_config).unwrap();
    drive_to_authenticated_stream(&mut second);
    let events = second
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();
    let first_resume = events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.name() == "resume" => Some(element.clone()),
            _ => None,
        })
        .expect("first resume element");
    assert_eq!(first_resume.attr("h"), Some("3"));
    second
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("resumed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("previd").to_owned(), "sm-1")
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                .build(),
        )))
        .unwrap();
    for id in ["b-1", "b-2"] {
        second
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("message", crate::NS_CLIENT)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                    .build(),
            )))
            .unwrap();
    }

    // Second detach → resume: the counter continues the same session.
    let resume_state = second.resume_state().expect("second resume state");
    let mut third_config = config();
    third_config.session.stream_management.resume_state = Some(resume_state);
    let mut third = XmppRuntime::new(third_config).unwrap();
    drive_to_authenticated_stream(&mut third);
    let events = third
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();
    let second_resume = events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.name() == "resume" => Some(element.clone()),
            _ => None,
        })
        .expect("second resume element");
    assert_eq!(second_resume.attr("previd"), Some("sm-1"));
    assert_eq!(second_resume.attr("h"), Some("5"));
}

#[test]
fn runtime_requests_default_resume_window_when_enabling_stream_management() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    let bind_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
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
        .unwrap();

    let ready_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();

    let enable = ready_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.name() == "enable" && element.ns() == crate::stream_management::NS_SM => {
                Some(element)
            }
            _ => None,
        })
        .expect("stream management enable");

    assert_eq!(enable.attr("resume"), Some("true"));
    assert_eq!(enable.attr("max"), Some("300"));
}

#[test]
fn runtime_releases_send_barrier_when_fresh_sm_enable_fails() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    let bind_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
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
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();

    assert!(!runtime.can_send_app_stanza());

    let failed_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failed", crate::stream_management::NS_SM).build(),
        )))
        .unwrap();

    assert_eq!(runtime.snapshot().phase, SessionPhase::Established);
    assert!(runtime.can_send_app_stanza());
    assert!(failed_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed
        ))
    )));
}

#[test]
fn runtime_replays_unhandled_stanzas_after_resume_without_recounting_them() {
    let resume_state = resume_state_with_sent_messages(["handled", "unhandled"]);
    let mut resume_config = config();
    resume_config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(resume_config).unwrap();
    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let resume_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("resumed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("previd").to_owned(), "old-sm-id")
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "1")
                .build(),
        )))
        .unwrap();

    assert!(resume_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.attr("id") == Some("unhandled")
            && element.get_child("delay", "urn:xmpp:delay").is_none()
    )));

    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            Element::builder("message", crate::NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "unhandled")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                .build(),
        )))
        .unwrap();

    let too_high_ack_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("a", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "3")
                .build(),
        )))
        .unwrap();

    assert!(too_high_ack_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.name() == "error"
            && element
                .get_child("handled-count-too-high", crate::stream_management::NS_SM)
                .is_some()
    )));
}

#[test]
fn runtime_resume_state_carries_unhandled_stanzas_into_next_runtime() {
    let mut first_runtime = XmppRuntime::new(config()).unwrap();
    first_runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();
    first_runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("enabled", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "old-sm-id")
                .build(),
        )))
        .unwrap();
    first_runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            Element::builder("message", crate::NS_CLIENT)
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    "carried-unhandled",
                )
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                .build(),
        )))
        .unwrap();

    let resume_state = first_runtime
        .resume_state()
        .expect("resume state with unhandled queue");
    let mut config = config();
    config.session.stream_management.resume_state = Some(resume_state);
    let mut next_runtime = XmppRuntime::new(config).unwrap();

    drive_to_authenticated_stream(&mut next_runtime);
    next_runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let resumed_events = next_runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("resumed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("previd").to_owned(), "old-sm-id")
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                .build(),
        )))
        .unwrap();

    assert!(resumed_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.attr("id") == Some("carried-unhandled")
    )));
}

#[test]
fn runtime_retries_only_unhandled_stanzas_after_failed_resume_fresh_enable() {
    let resume_state = resume_state_with_sent_messages(["handled-before-fail", "retry-after-fail"]);
    let mut config = config();
    config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(config).unwrap();

    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let failed_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "1")
                .build(),
        )))
        .unwrap();

    assert!(failed_events.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(crate::MessageDeliveryEvent::Acked { stanza_id })
            if stanza_id.as_str() == "handled-before-fail"
    )));
    assert!(failed_events.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(crate::MessageDeliveryEvent::Failed { stanza_id })
            if stanza_id.as_str() == "retry-after-fail"
    )));

    let bind_id = failed_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(request)) => {
                Some(request.stanza_id.clone())
            }
            _ => None,
        })
        .expect("fresh bind request");
    let bind_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();

    let enable = bind_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.name() == "enable" => Some(element.clone()),
            _ => None,
        })
        .expect("fresh SM enable");
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            enable,
        )))
        .unwrap();

    let enabled_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("enabled", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "fresh-sm-id")
                .build(),
        )))
        .unwrap();

    assert!(!enabled_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.attr("id") == Some("handled-before-fail")
    )));
    let retry = enabled_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.attr("id") == Some("retry-after-fail") => Some(element),
            _ => None,
        })
        .expect("retry-after-fail replay");
    let delay = retry
        .get_child("delay", "urn:xmpp:delay")
        .expect("fallback replay should include XEP-0203 delay");
    assert!(delay.attr("stamp").is_some());
}

#[test]
fn runtime_preserves_failed_resume_snapshot_until_fallback_retry_is_sent() {
    let resume_state = resume_state_with_sent_messages(["retry-after-drop"]);
    let mut config = config();
    config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(config).unwrap();

    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let failed_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                .build(),
        )))
        .unwrap();
    let bind_id = failed_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(request)) => {
                Some(request.stanza_id.clone())
            }
            _ => None,
        })
        .expect("fresh bind request");
    let bind_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();
    let enable = bind_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.name() == "enable" => Some(element.clone()),
            _ => None,
        })
        .expect("fresh SM enable");

    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            enable,
        )))
        .unwrap();

    assert_resume_state_replays_id(
        runtime
            .resume_state()
            .expect("fallback snapshot should survive fresh enable attempt"),
        "retry-after-drop",
    );
}

#[test]
fn runtime_discards_resume_state_after_prebind_stream_error_close() {
    let resume_state = resume_state_with_sent_messages(["retry-after-stream-error"]);
    let mut resume_config = config();
    resume_config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(resume_config).unwrap();

    drive_to_authenticated_stream(&mut runtime);
    let feature_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    assert!(feature_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.name() == "resume"
    )));

    let error_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            handled_count_too_high_stream_error(3, 2),
        )))
        .unwrap();

    assert!(error_events.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(crate::MessageDeliveryEvent::Failed { stanza_id })
            if stanza_id.as_str() == "retry-after-stream-error"
    )));
    assert!(error_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed
        ))
    )));
    assert!(runtime.resume_state().is_none());

    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Close(
            StreamClose,
        )))
        .unwrap();
    assert!(runtime.resume_state().is_none());

    let mut next_runtime = XmppRuntime::new(config()).unwrap();
    drive_to_authenticated_stream(&mut next_runtime);
    let next_events = next_runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    assert!(next_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(_))
    )));
    assert!(!next_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.name() == "resume"
    )));
}

#[test]
fn runtime_discards_resume_state_after_generic_prebind_stream_error() {
    let resume_state = resume_state_with_sent_messages(["retry-after-generic-stream-error"]);
    let mut resume_config = config();
    resume_config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(resume_config).unwrap();

    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let error_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            internal_server_error_stream_error(),
        )))
        .unwrap();

    assert!(error_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamError {
            condition: StreamErrorCondition::InternalServerError,
            ..
        })
    )));
    assert!(error_events.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(crate::MessageDeliveryEvent::Failed { stanza_id })
            if stanza_id.as_str() == "retry-after-generic-stream-error"
    )));
    assert!(error_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed
        ))
    )));
    assert!(runtime.resume_state().is_none());
}

#[test]
fn runtime_emits_stream_error_condition_for_browser_telemetry() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    drive_to_authenticated_stream(&mut runtime);

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            not_authorized_stream_error(),
        )))
        .unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamError {
            condition: StreamErrorCondition::NotAuthorized,
            ..
        })
    )));
}

#[test]
fn runtime_emits_stream_error_detail_for_handled_count_too_high() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    drive_to_authenticated_stream(&mut runtime);

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            handled_count_too_high_stream_error(3, 2),
        )))
        .unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamError {
            condition: StreamErrorCondition::UndefinedCondition,
            detail: Some(crate::StreamErrorDetail::HandledCountTooHigh {
                h: 3,
                send_count: 2
            })
        })
    )));
}

#[test]
fn runtime_rejects_stanza_only_conditions_in_stream_error_namespace() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    drive_to_authenticated_stream(&mut runtime);

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            invalid_forbidden_stream_error(),
        )))
        .unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamError {
            condition: StreamErrorCondition::UndefinedCondition,
            ..
        })
    )));
}

#[test]
fn runtime_discards_fallback_resume_state_after_prebind_stream_error() {
    let resume_state = resume_state_with_sent_messages(["retry-after-fallback-error"]);
    let mut resume_config = config();
    resume_config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(resume_config).unwrap();

    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                .build(),
        )))
        .unwrap();

    assert!(
        runtime.resume_state().is_some(),
        "failed resume should keep fallback retry state until the fresh stream can bind"
    );

    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            internal_server_error_stream_error(),
        )))
        .unwrap();
    assert!(runtime.resume_state().is_none());

    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Close(
            StreamClose,
        )))
        .unwrap();
    assert!(runtime.resume_state().is_none());
}

#[test]
fn runtime_keeps_resume_state_when_transport_fails_during_resume_without_stream_error() {
    let resume_state = resume_state_with_sent_messages(["retry-after-transport-fail"]);
    let expected_resume_state = resume_state.clone();
    let mut resume_config = config();
    resume_config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(resume_config).unwrap();

    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let events = runtime
        .apply_transport_event(TransportEvent::StateChanged(TransportState::Failed))
        .unwrap();

    assert!(!events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed
        ))
    )));
    assert_eq!(runtime.resume_state(), Some(expected_resume_state));
}

#[test]
fn runtime_retries_unhandled_stanzas_when_fresh_sm_enable_fails() {
    let resume_state = resume_state_with_sent_messages(["handled-before-fail", "retry-after-fail"]);
    let mut config = config();
    config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(config).unwrap();

    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();

    let failed_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "1")
                .build(),
        )))
        .unwrap();
    let bind_id = failed_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(request)) => {
                Some(request.stanza_id.clone())
            }
            _ => None,
        })
        .expect("fresh bind request");
    let bind_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            bind_result(&bind_id),
        )))
        .unwrap();

    let enable = bind_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.name() == "enable" => Some(element.clone()),
            _ => None,
        })
        .expect("fresh SM enable");
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            enable,
        )))
        .unwrap();

    let failed_enable_events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failed", crate::stream_management::NS_SM).build(),
        )))
        .unwrap();

    assert!(runtime.can_send_app_stanza());
    assert!(!failed_enable_events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.attr("id") == Some("handled-before-fail")
    )));
    let retry = failed_enable_events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) if element.attr("id") == Some("retry-after-fail") => Some(element),
            _ => None,
        })
        .expect("retry-after-fail replay after fresh enable failure");
    let delay = retry
        .get_child("delay", "urn:xmpp:delay")
        .expect("fresh enable failure replay should include XEP-0203 delay");
    assert!(delay.attr("stamp").is_some());
}

#[test]
fn runtime_emits_message_delivery_ack_from_core_sm_queue() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            Element::builder("message", crate::NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "core-tracked")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                .build(),
        )))
        .unwrap();

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("a", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "1")
                .build(),
        )))
        .unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(crate::MessageDeliveryEvent::Acked { stanza_id })
            if stanza_id.as_str() == "core-tracked"
    )));
}

#[test]
fn runtime_counts_outbound_stanzas_after_enable_is_sent() {
    let mut runtime = XmppRuntime::new(config()).unwrap();

    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();

    let message = Element::builder("message", crate::NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "out-1")
        .build();
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            message,
        )))
        .unwrap();

    assert!(runtime.sm_state.outbound_enabled);
    assert_eq!(runtime.sm_state.outbound_count, 1);
}

#[test]
fn runtime_rejects_sm_ack_above_sent_count() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();

    let ack = Element::builder("a", crate::stream_management::NS_SM)
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), "1")
        .build();
    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            ack,
        )))
        .unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed
        ))
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.name() == "error"
            && element
                .get_child("handled-count-too-high", crate::stream_management::NS_SM)
                .is_some()
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(TransportMessage::Close(_)))
    )));
    assert_eq!(runtime.sm_state.server_h, 0);
}

#[test]
fn runtime_requires_oauthbearer_feature() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    runtime.queue_request(ClientRequest::Connect).unwrap();
    runtime
        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
        )))
        .unwrap();

    let error = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("features", NS_STREAMS).build(),
        )))
        .unwrap_err();

    assert!(matches!(
        error,
        ClientError::MissingStreamFeature {
            feature: RequiredStreamFeature::Authentication(AuthMechanism::OAuthBearer)
        }
    ));
}

#[test]
fn runtime_emits_typed_auth_failure() {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    runtime.queue_request(ClientRequest::Connect).unwrap();
    runtime
        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            pre_auth_features(),
        )))
        .unwrap();

    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("failure", NS_SASL)
                .append(Element::builder("not-authorized", NS_SASL).build())
                .build(),
        )))
        .unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::AuthenticationFailed(failure))
            if failure.condition == crate::SaslFailureCondition::NotAuthorized
    )));
    assert_eq!(runtime.snapshot().phase, SessionPhase::Disconnecting);
}

fn pre_auth_features() -> Element {
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
        .build()
}

fn post_auth_features() -> Element {
    Element::builder("features", NS_STREAMS)
        .append(Element::builder("bind", NS_BIND).build())
        .build()
}

fn post_auth_features_with_sm() -> Element {
    Element::builder("features", NS_STREAMS)
        .append(Element::builder("bind", NS_BIND).build())
        .append(Element::builder("sm", crate::stream_management::NS_SM).build())
        .build()
}

fn drive_to_authenticated_stream(runtime: &mut XmppRuntime) {
    runtime.queue_request(ClientRequest::Connect).unwrap();
    runtime
        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            pre_auth_features(),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("success", NS_SASL).build(),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
        )))
        .unwrap();
}

fn bind_result(stanza_id: &StanzaId) -> Element {
    Element::builder("iq", crate::NS_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            stanza_id.as_str(),
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("bind", NS_BIND)
                .append(
                    Element::builder("jid", NS_BIND)
                        .append("alice@example.com/macbook")
                        .build(),
                )
                .build(),
        )
        .build()
}

fn resume_state_with_sent_messages<const N: usize>(ids: [&str; N]) -> SmResumeState {
    let mut runtime = XmppRuntime::new(config()).unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("enabled", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "old-sm-id")
                .build(),
        )))
        .unwrap();
    for id in ids {
        runtime
            .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
                Element::builder("message", crate::NS_CLIENT)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                    .build(),
            )))
            .unwrap();
    }
    runtime.resume_state().expect("resume state")
}

fn handled_count_too_high_stream_error(h: u32, send_count: u32) -> Element {
    Element::builder("error", NS_STREAMS)
        .append(
            Element::builder("undefined-condition", "urn:ietf:params:xml:ns:xmpp-streams").build(),
        )
        .append(
            Element::builder("handled-count-too-high", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string())
                .attr(
                    minidom::rxml::xml_ncname!("send-count").to_owned(),
                    send_count.to_string(),
                )
                .build(),
        )
        .build()
}

fn internal_server_error_stream_error() -> Element {
    Element::builder("error", NS_STREAMS)
        .append(
            Element::builder(
                "internal-server-error",
                "urn:ietf:params:xml:ns:xmpp-streams",
            )
            .build(),
        )
        .build()
}

fn not_authorized_stream_error() -> Element {
    Element::builder("error", NS_STREAMS)
        .append(
            Element::builder("text", "urn:ietf:params:xml:ns:xmpp-streams")
                .append("authentication expired")
                .build(),
        )
        .append(Element::builder("not-authorized", "urn:ietf:params:xml:ns:xmpp-streams").build())
        .build()
}

fn invalid_forbidden_stream_error() -> Element {
    Element::builder("error", NS_STREAMS)
        .append(Element::builder("forbidden", "urn:ietf:params:xml:ns:xmpp-streams").build())
        .build()
}

fn assert_resume_state_replays_id(resume_state: SmResumeState, expected_id: &str) {
    let mut config = config();
    config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(config).unwrap();

    drive_to_authenticated_stream(&mut runtime);
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            post_auth_features_with_sm(),
        )))
        .unwrap();
    let events = runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            Element::builder("resumed", crate::stream_management::NS_SM)
                .attr(minidom::rxml::xml_ncname!("previd").to_owned(), "old-sm-id")
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                .build(),
        )))
        .unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(element)
        )) if element.attr("id") == Some(expected_id)
    )));
}
