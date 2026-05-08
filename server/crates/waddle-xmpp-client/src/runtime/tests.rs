use std::str::FromStr;

use jid::{BareJid, FullJid};
use minidom::Element;
use url::Url;

use super::*;
use crate::bootstrap::{NS_BIND, NS_SASL, NS_STREAMS};
use crate::config::{AccessToken, ClientResource, OAuthBearerConfig, WebSocketConfig};
use crate::{ConnectionConfig, StreamManagementEvent};

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
fn runtime_counts_outbound_stanzas_after_enable_is_sent() {
    let mut runtime = XmppRuntime::new(config()).unwrap();

    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            SmState::build_enable(true),
        )))
        .unwrap();

    let message = Element::builder("message", crate::NS_CLIENT)
        .attr("id", "out-1")
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
        .attr("h", "1")
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

fn bind_result(stanza_id: &StanzaId) -> Element {
    Element::builder("iq", crate::NS_CLIENT)
        .attr("id", stanza_id.as_str())
        .attr("type", "result")
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
