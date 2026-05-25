use std::str::FromStr;

use jid::{BareJid, FullJid};
use minidom::Element;
use url::Url;

use super::*;
use crate::bootstrap::{NS_BIND, NS_SASL, NS_STREAMS};
use crate::config::{AccessToken, ClientResource, OAuthBearerConfig, WebSocketConfig};
use crate::{ConnectionConfig, SmResumeState, StreamId, StreamManagementEvent};

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
    let mut config = config();
    config.session.stream_management.resume_state = Some(resume_state);
    let mut runtime = XmppRuntime::new(config).unwrap();
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
