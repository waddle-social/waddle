//! XEP-0388 SASL2 inline negotiation in the client runtime: XEP-0198 inline
//! stream resumption, with RFC 6120 resource binding when it fails. The
//! runtime never sends an XEP-0386 Bind 2 request, so `<bound/>` is always
//! an unrequested result.

use std::str::FromStr;

use chrono::{TimeZone, Utc};
use jid::{BareJid, FullJid};
use minidom::Element;
use url::Url;
use waddle_xmpp_client::{
    AccessToken, AuthenticationRequest, ClientConfig, ClientError, ClientEvent, ClientRequest,
    ClientResource, ConnectionConfig, ConnectionEvent, MessageDeliveryEvent, OAuthBearerConfig,
    OAuthBearerRequest, SessionBinding, SessionPhase, SmResumeState, StreamFeatures, StreamId,
    StreamManagementEvent, StreamOpen, TransportEvent, TransportMessage, TransportState,
    UnhandledOutboundEntry, WebSocketConfig, XmppRuntime, NS_BIND, NS_CLIENT, NS_SASL, NS_SASL2,
    NS_SM, NS_STREAMS,
};
use waddle_xmpp_core::xep0388_inline::NS_BIND2;

const NS_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
const NS_STREAM_ERRORS: &str = "urn:ietf:params:xml:ns:xmpp-streams";

fn config(resume_state: Option<SmResumeState>) -> ClientConfig {
    let mut config = ClientConfig::try_new(
        ConnectionConfig::new(BareJid::from_str("waddle.example").unwrap()),
        WebSocketConfig::try_new(Url::parse("wss://chat.example.com/ws").unwrap()).unwrap(),
        OAuthBearerConfig::try_new(
            BareJid::from_str("alice@example.com").unwrap(),
            ClientResource::try_new("macbook").unwrap(),
            AccessToken::new("token"),
        )
        .unwrap(),
    )
    .unwrap();
    config.session.stream_management.resume_state = resume_state;
    config
}

fn message(id: &str) -> Element {
    Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .build()
}

/// A resumable session that handled seven inbound stanzas and sent twelve, of
/// which the server has not yet acknowledged `m-11` and `m-12`.
fn resume_state() -> SmResumeState {
    let sent_at = Utc
        .with_ymd_and_hms(2026, 7, 27, 12, 0, 0)
        .single()
        .unwrap();
    SmResumeState::from_unhandled_outbound_entries(
        StreamId::new("old-sm-id"),
        7,
        12,
        [
            UnhandledOutboundEntry::try_new(message("m-11"), sent_at).unwrap(),
            UnhandledOutboundEntry::try_new(message("m-12"), sent_at).unwrap(),
        ],
    )
    .unwrap()
}

fn sasl2_features(inline: Option<Vec<Element>>) -> Element {
    let mut authentication = Element::builder("authentication", NS_SASL2).append(
        Element::builder("mechanism", NS_SASL2)
            .append("OAUTHBEARER")
            .build(),
    );
    if let Some(features) = inline {
        let mut container = Element::builder("inline", NS_SASL2);
        for feature in features {
            container = container.append(feature);
        }
        authentication = authentication.append(container.build());
    }
    Element::builder("features", NS_STREAMS)
        .append(authentication.build())
        .build()
}

fn sm_inline_feature() -> Element {
    Element::builder("sm", NS_SM).build()
}

fn bind2_feature() -> Element {
    Element::builder("bind", NS_BIND2)
        .append(
            Element::builder("inline", NS_BIND2)
                .append(
                    Element::builder("feature", NS_BIND2)
                        .attr(minidom::rxml::xml_ncname!("var").to_owned(), NS_SM)
                        .build(),
                )
                .build(),
        )
        .build()
}

fn post_auth_features(stream_management: bool) -> Element {
    let mut features =
        Element::builder("features", NS_STREAMS).append(Element::builder("bind", NS_BIND).build());
    if stream_management {
        features = features.append(Element::builder("sm", NS_SM).build());
    }
    features.build()
}

fn success(identity: &str, inline_results: Vec<Element>) -> Element {
    let mut success = Element::builder("success", NS_SASL2).append(
        Element::builder("authorization-identifier", NS_SASL2)
            .append(identity)
            .build(),
    );
    for result in inline_results {
        success = success.append(result);
    }
    success.build()
}

fn resumed(previd: &str, h: &str) -> Element {
    Element::builder("resumed", NS_SM)
        .attr(minidom::rxml::xml_ncname!("previd").to_owned(), previd)
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), h)
        .build()
}

fn failed(h: Option<&str>) -> Element {
    let mut failed = Element::builder("failed", NS_SM);
    if let Some(h) = h {
        failed = failed.attr(minidom::rxml::xml_ncname!("h").to_owned(), h);
    }
    failed
        .append(Element::builder("item-not-found", NS_STANZAS).build())
        .build()
}

fn bound() -> Element {
    Element::builder("bound", NS_BIND2)
        .append(Element::builder("metadata", "urn:xmpp:mam:2").build())
        .build()
}

fn receive(runtime: &mut XmppRuntime, element: Element) -> Result<Vec<ClientEvent>, ClientError> {
    runtime.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        element,
    )))
}

/// Open a connection on `runtime` and feed the pre-authentication stream
/// features, returning the events of the step that sends the authentication
/// request.
fn connect(runtime: &mut XmppRuntime, features: Element) -> Vec<ClientEvent> {
    runtime.queue_request(ClientRequest::Connect).unwrap();
    runtime
        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .unwrap();
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
        )))
        .unwrap();
    receive(runtime, features).unwrap()
}

fn runtime_after_features(
    resume_state: Option<SmResumeState>,
    features: Element,
) -> (XmppRuntime, Vec<ClientEvent>) {
    let mut runtime = XmppRuntime::try_new(config(resume_state)).unwrap();
    let events = connect(&mut runtime, features);
    (runtime, events)
}

fn sent(runtime: &mut XmppRuntime, element: Element) -> Vec<ClientEvent> {
    runtime
        .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
            element,
        )))
        .unwrap()
}

/// A runtime whose resumable session `fresh-sm-id` was bound by the server
/// to a resource other than the configured one, has one unacknowledged
/// message `m-1`, and has since lost its transport.
fn runtime_after_dropped_session_with_server_assigned_resource() -> XmppRuntime {
    let (mut runtime, _) = runtime_after_features(None, sasl2_features(None));
    receive(&mut runtime, success("alice@example.com", Vec::new())).unwrap();
    let bind = receive(&mut runtime, post_auth_features(true)).unwrap();
    let bind_id = outbound(&bind)[0].attr("id").expect("bind id").to_owned();
    let bound = receive(
        &mut runtime,
        Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), bind_id)
            .append(
                Element::builder("bind", NS_BIND)
                    .append(
                        Element::builder("jid", NS_BIND)
                            .append("alice@example.com/assigned-by-server")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
    .unwrap();
    let enable = outbound(&bound)[0].clone();
    assert!(enable.is("enable", NS_SM));
    sent(&mut runtime, enable);
    receive(
        &mut runtime,
        Element::builder("enabled", NS_SM)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "fresh-sm-id")
            .build(),
    )
    .unwrap();
    sent(&mut runtime, message("m-1"));
    runtime
        .apply_transport_event(TransportEvent::Closed)
        .unwrap();
    assert_eq!(runtime.snapshot().binding, None);
    runtime
}

fn server_assigned_binding() -> SessionBinding {
    SessionBinding {
        jid: FullJid::from_str("alice@example.com/assigned-by-server").unwrap(),
        stream_id: Some(StreamId::new("fresh-sm-id")),
        resumable: true,
    }
}

fn outbound(events: &[ClientEvent]) -> Vec<&Element> {
    events
        .iter()
        .filter_map(|event| match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )) => Some(element),
            _ => None,
        })
        .collect()
}

fn authenticate(events: &[ClientEvent]) -> Element {
    let outbound = outbound(events);
    assert_eq!(outbound.len(), 1, "only the authentication request is sent");
    assert!(outbound[0].is("authenticate", NS_SASL2));
    outbound[0].clone()
}

fn child_names(element: &Element) -> Vec<(String, String)> {
    element
        .children()
        .map(|child| (child.name().to_owned(), child.ns()))
        .collect()
}

fn serialised(element: &Element) -> Vec<u8> {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).unwrap();
    bytes
}

fn plain_sasl2_authenticate() -> Element {
    OAuthBearerRequest {
        account: BareJid::from_str("alice@example.com").unwrap(),
        access_token: AccessToken::new("token"),
    }
    .to_sasl2_element()
}

fn sm_failed(events: &[ClientEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Failed
            ))
        )
    })
}

fn delivery_acked(events: &[ClientEvent], id: &str) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
                if stanza_id.as_str() == id
        )
    })
}

fn delivery_failed(events: &[ClientEvent], id: &str) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id })
                if stanza_id.as_str() == id
        )
    })
}

fn full_offer() -> Element {
    sasl2_features(Some(vec![sm_inline_feature(), bind2_feature()]))
}

#[test]
fn xep0388_inline_offer_is_exposed_on_stream_features() {
    let features = StreamFeatures::from_element(&full_offer()).unwrap();
    let offer = features
        .sasl2_inline_offer()
        .unwrap()
        .expect("inline offer");
    assert!(offer.stream_resumption);
    assert!(offer
        .bind2
        .expect("Bind 2 offer")
        .supports_stream_management());

    let without_inline = StreamFeatures::from_element(&sasl2_features(None)).unwrap();
    assert_eq!(without_inline.sasl2_inline_offer().unwrap(), None);
}

#[test]
fn xep0198_inline_resume_is_requested_without_a_bind_2_request() {
    for features in [
        sasl2_features(Some(vec![sm_inline_feature()])),
        // Bind 2 is offered but never taken up.
        full_offer(),
    ] {
        let (runtime, events) = runtime_after_features(Some(resume_state()), features);

        let authenticate = authenticate(&events);
        assert_eq!(
            child_names(&authenticate),
            vec![
                ("initial-response".to_owned(), NS_SASL2.to_owned()),
                ("resume".to_owned(), NS_SM.to_owned()),
            ]
        );
        let resume = authenticate.get_child("resume", NS_SM).unwrap();
        assert_eq!(resume.attr("previd"), Some("old-sm-id"));
        assert_eq!(resume.attr("h"), Some("7"));
        assert_eq!(
            authenticate.get_child("initial-response", NS_SASL2),
            plain_sasl2_authenticate().get_child("initial-response", NS_SASL2)
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::AuthenticationRequested(
                AuthenticationRequest::Sasl2OAuthBearerInline(request)
            )) if request.inline.resume.is_some() && request.inline.bind.is_none()
        )));
        assert_eq!(runtime.snapshot().phase, SessionPhase::Authenticating);
    }
}

#[test]
fn xep0388_authenticate_is_unchanged_without_an_inline_resumption_offer() {
    let expected = serialised(&plain_sasl2_authenticate());
    for (resume_state, features) in [
        (Some(resume_state()), sasl2_features(None)),
        (Some(resume_state()), sasl2_features(Some(Vec::new()))),
        (
            Some(resume_state()),
            sasl2_features(Some(vec![bind2_feature()])),
        ),
        (
            Some(resume_state()),
            sasl2_features(Some(vec![Element::builder("sm", "urn:xmpp:sm:2").build()])),
        ),
        (None, full_offer()),
    ] {
        let (_, events) = runtime_after_features(resume_state, features.clone());
        assert_eq!(serialised(&authenticate(&events)), expected, "{features:?}");
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::AuthenticationRequested(
                AuthenticationRequest::Sasl2OAuthBearer(_)
            ))
        )));
    }
}

#[test]
fn xep0388_malformed_inline_offer_counts_as_no_offer_and_does_not_block_login() {
    let expected = serialised(&plain_sasl2_authenticate());
    for inline in [
        vec![sm_inline_feature(), sm_inline_feature()],
        vec![
            sm_inline_feature(),
            Element::builder("resume", NS_SM).build(),
        ],
        vec![
            sm_inline_feature(),
            Element::builder("bound", NS_BIND2).build(),
        ],
        vec![
            sm_inline_feature(),
            Element::builder("bind", NS_BIND2)
                .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1")
                .build(),
        ],
    ] {
        let features = sasl2_features(Some(inline));
        assert!(matches!(
            StreamFeatures::from_element(&features)
                .expect("stream features still parse")
                .sasl2_inline_offer(),
            Err(ClientError::InvalidSasl2Feature)
        ));

        for resume_state in [Some(resume_state()), None] {
            let (mut runtime, events) = runtime_after_features(resume_state, features.clone());
            assert_eq!(serialised(&authenticate(&events)), expected, "{features:?}");
            receive(&mut runtime, success("alice@example.com", Vec::new())).unwrap();
            assert_eq!(runtime.snapshot().phase, SessionPhase::Authenticating);
        }
    }
}

#[test]
fn xep0388_authenticate_is_unchanged_when_resumption_is_disabled_locally() {
    let mut config = config(Some(resume_state()));
    config.session.stream_management.allow_resume = false;
    let mut runtime = XmppRuntime::try_new(config).unwrap();
    let events = connect(&mut runtime, full_offer());

    assert_eq!(
        serialised(&authenticate(&events)),
        serialised(&plain_sasl2_authenticate())
    );
}

#[test]
fn xep0198_inline_resumed_continues_the_session_and_applies_the_server_h() {
    let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());

    let events = receive(
        &mut runtime,
        success("alice@example.com", vec![resumed("old-sm-id", "11")]),
    )
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
    assert!(runtime.can_send_app_stanza());
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Resumed { h: 11 }
        ))
    )));
    assert!(delivery_acked(&events, "m-11"));
    assert!(!delivery_acked(&events, "m-12"));
    // The unacknowledged tail is replayed, then acknowledged, with no bind.
    let sent = outbound(&events);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], &message("m-12"));
    assert!(sent[1].is("r", NS_SM));
    assert!(!events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(
            ConnectionEvent::ResourceBindingRequested(_) | ConnectionEvent::ResourceBound(_)
        )
    )));
    let resume_state = runtime.resume_state().expect("session stays resumable");
    assert_eq!(resume_state.previd(), &StreamId::new("old-sm-id"));
    assert_eq!(resume_state.inbound_h(), 7);
}

#[test]
fn xep0198_inline_resumed_applies_a_handled_count_that_wrapped_around() {
    // Two stanzas were sent across the 2^32 boundary: the first took the
    // counter to 0 and the second to 1, with neither acknowledged.
    let sent_at = Utc
        .with_ymd_and_hms(2026, 7, 27, 12, 0, 0)
        .single()
        .unwrap();
    let wrapped = SmResumeState::from_unhandled_outbound_entries(
        StreamId::new("old-sm-id"),
        7,
        1,
        [
            UnhandledOutboundEntry::try_new(message("m-wrap-0"), sent_at).unwrap(),
            UnhandledOutboundEntry::try_new(message("m-wrap-1"), sent_at).unwrap(),
        ],
    )
    .unwrap();

    let (mut runtime, _) = runtime_after_features(Some(wrapped.clone()), full_offer());
    let events = receive(
        &mut runtime,
        success("alice@example.com", vec![resumed("old-sm-id", "0")]),
    )
    .unwrap();
    assert!(delivery_acked(&events, "m-wrap-0"));
    assert!(!delivery_acked(&events, "m-wrap-1"));
    assert_eq!(outbound(&events)[0], &message("m-wrap-1"));

    // The server's counter before either stanza is a valid `h` too.
    let (mut runtime, _) = runtime_after_features(Some(wrapped.clone()), full_offer());
    let events = receive(
        &mut runtime,
        success(
            "alice@example.com",
            vec![resumed("old-sm-id", "4294967295")],
        ),
    )
    .unwrap();
    assert!(!delivery_acked(&events, "m-wrap-0"));
    assert_eq!(runtime.snapshot().phase, SessionPhase::Established);

    // One past the last stanza sent is not.
    let (mut runtime, _) = runtime_after_features(Some(wrapped), full_offer());
    let error = receive(
        &mut runtime,
        success("alice@example.com", vec![resumed("old-sm-id", "2")]),
    )
    .unwrap_err();
    assert!(matches!(error, ClientError::InvalidSasl2InlineResult));
}

#[test]
fn xep0198_inline_results_naming_another_session_or_too_many_stanzas_are_rejected() {
    for results in [
        vec![resumed("someone-elses-id", "11")],
        // Only twelve stanzas were sent on the retained session.
        vec![resumed("old-sm-id", "13")],
        vec![failed(Some("13"))],
    ] {
        let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());
        let before = runtime.resume_state();

        let error =
            receive(&mut runtime, success("alice@example.com", results.clone())).unwrap_err();

        assert!(
            matches!(error, ClientError::InvalidSasl2InlineResult),
            "{results:?}: {error:?}"
        );
        assert_eq!(runtime.snapshot().phase, SessionPhase::Disconnecting);
        assert_eq!(runtime.snapshot().binding, None);
        assert_eq!(runtime.resume_state(), before, "{results:?}");
    }
}

#[test]
fn xep0198_resumed_session_keeps_the_jid_the_server_bound() {
    // Resumption on the authenticated stream.
    let mut runtime = runtime_after_dropped_session_with_server_assigned_resource();
    connect(&mut runtime, sasl2_features(None));
    receive(&mut runtime, success("alice@example.com", Vec::new())).unwrap();
    let resume = receive(&mut runtime, post_auth_features(true)).unwrap();
    assert!(outbound(&resume)[0].is("resume", NS_SM));
    receive(&mut runtime, resumed("fresh-sm-id", "0")).unwrap();
    assert_eq!(runtime.snapshot().binding, Some(server_assigned_binding()));

    // Inline resumption, with the bare identifier XEP-0388 shows.
    let mut runtime = runtime_after_dropped_session_with_server_assigned_resource();
    let requested = connect(&mut runtime, full_offer());
    assert_eq!(
        authenticate(&requested)
            .get_child("resume", NS_SM)
            .and_then(|resume| resume.attr("previd")),
        Some("fresh-sm-id")
    );
    let events = receive(
        &mut runtime,
        success("alice@example.com", vec![resumed("fresh-sm-id", "0")]),
    )
    .unwrap();
    assert_eq!(runtime.snapshot().binding, Some(server_assigned_binding()));
    assert_eq!(outbound(&events)[0], &message("m-1"));

    // Inline resumption naming the retained resource.
    let mut runtime = runtime_after_dropped_session_with_server_assigned_resource();
    connect(&mut runtime, full_offer());
    receive(
        &mut runtime,
        success(
            "alice@example.com/assigned-by-server",
            vec![resumed("fresh-sm-id", "1")],
        ),
    )
    .unwrap();
    assert_eq!(runtime.snapshot().binding, Some(server_assigned_binding()));
}

#[test]
fn xep0198_bound_jid_travels_with_the_exported_resume_state() {
    let runtime = runtime_after_dropped_session_with_server_assigned_resource();
    let exported = runtime.resume_state().expect("resumable session");
    assert_eq!(
        exported.bound_jid(),
        Some(&FullJid::from_str("alice@example.com/assigned-by-server").unwrap())
    );

    let (mut restored, _) = runtime_after_features(Some(exported), full_offer());
    receive(
        &mut restored,
        success("alice@example.com", vec![resumed("fresh-sm-id", "1")]),
    )
    .unwrap();
    assert_eq!(restored.snapshot().binding, Some(server_assigned_binding()));
}

#[test]
fn xep0198_inline_resumed_rejects_a_full_jid_other_than_the_bound_one() {
    for identity in [
        "alice@example.com/macbook",
        "alice@example.com/some-other-resource",
    ] {
        let mut runtime = runtime_after_dropped_session_with_server_assigned_resource();
        connect(&mut runtime, full_offer());
        let before = runtime.resume_state();

        let error = receive(
            &mut runtime,
            success(identity, vec![resumed("fresh-sm-id", "1")]),
        )
        .unwrap_err();

        assert!(
            matches!(error, ClientError::Sasl2AuthorizationIdentityMismatch),
            "{identity}: {error:?}"
        );
        assert_eq!(runtime.snapshot().phase, SessionPhase::Disconnecting);
        assert_eq!(runtime.snapshot().binding, None);
        assert_eq!(runtime.resume_state(), before);
    }
}

#[test]
fn xep0198_inline_resumed_takes_the_reported_jid_when_none_is_retained() {
    // State restored by a host that did not persist the bound JID.
    let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());

    receive(
        &mut runtime,
        success(
            "alice@example.com/assigned-by-server",
            vec![resumed("old-sm-id", "12")],
        ),
    )
    .unwrap();

    let jid = FullJid::from_str("alice@example.com/assigned-by-server").unwrap();
    assert_eq!(
        runtime
            .snapshot()
            .binding
            .as_ref()
            .map(|binding| binding.jid.clone()),
        Some(jid.clone())
    );
    assert_eq!(
        runtime
            .resume_state()
            .and_then(|state| state.bound_jid().cloned()),
        Some(jid)
    );
}

#[test]
fn xep0198_failed_inline_resume_falls_back_to_resource_binding() {
    for features in [
        sasl2_features(Some(vec![sm_inline_feature()])),
        full_offer(),
    ] {
        let (mut runtime, _) = runtime_after_features(Some(resume_state()), features);

        // The server's `h` settles what it handled; the rest waits for the
        // fresh session, and nothing is written before the stream features.
        let events = receive(
            &mut runtime,
            success("alice@example.com", vec![failed(Some("11"))]),
        )
        .unwrap();
        assert!(sm_failed(&events));
        assert!(delivery_acked(&events, "m-11"));
        assert!(delivery_failed(&events, "m-12"));
        assert!(!delivery_failed(&events, "m-11"));
        assert!(outbound(&events).is_empty());
        assert_eq!(runtime.snapshot().phase, SessionPhase::Authenticating);
        assert!(!runtime.can_send_app_stanza());

        let bind = receive(&mut runtime, post_auth_features(true)).unwrap();
        assert_eq!(runtime.snapshot().phase, SessionPhase::Binding);
        let sent = outbound(&bind);
        assert_eq!(sent.len(), 1);
        assert!(sent[0].is("iq", NS_CLIENT));
        assert!(sent[0].get_child("bind", NS_BIND).is_some());
        assert!(bind.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(_))
        )));
    }
}

#[test]
fn xep0198_failed_inline_resume_without_a_handled_count_keeps_the_whole_tail_for_retry() {
    let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());

    let events = receive(
        &mut runtime,
        success("alice@example.com", vec![failed(None)]),
    )
    .unwrap();

    assert!(sm_failed(&events));
    assert!(delivery_failed(&events, "m-11"));
    assert!(delivery_failed(&events, "m-12"));
    assert!(!delivery_acked(&events, "m-11"));
}

#[test]
fn xep0198_unanswered_inline_resume_is_retried_on_the_authenticated_stream() {
    let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());

    let events = receive(&mut runtime, success("alice@example.com", Vec::new())).unwrap();
    assert!(outbound(&events).is_empty());
    assert!(!sm_failed(&events));

    let resume = receive(&mut runtime, post_auth_features(true)).unwrap();
    assert_eq!(runtime.snapshot().phase, SessionPhase::Resuming);
    let sent = outbound(&resume);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].is("resume", NS_SM));
    assert_eq!(sent[0].attr("previd"), Some("old-sm-id"));
    assert_eq!(sent[0].attr("h"), Some("7"));
}

#[test]
fn xep0198_resume_follows_sasl2_on_the_same_stream_without_an_inline_offer() {
    let (mut runtime, _) = runtime_after_features(Some(resume_state()), sasl2_features(None));
    receive(&mut runtime, success("alice@example.com", Vec::new())).unwrap();

    let resume = receive(&mut runtime, post_auth_features(true)).unwrap();
    assert_eq!(runtime.snapshot().phase, SessionPhase::Resuming);
    assert!(outbound(&resume)
        .iter()
        .any(|element| element.is("resume", NS_SM)));

    let events = receive(&mut runtime, resumed("old-sm-id", "12")).unwrap();
    assert_eq!(runtime.snapshot().phase, SessionPhase::Established);
    assert!(delivery_acked(&events, "m-12"));
}

#[test]
fn xep0388_malformed_inline_results_are_rejected_without_touching_the_session() {
    for results in [
        // XEP-0198 requires both `h` and `previd` on <resumed/>.
        vec![Element::builder("resumed", NS_SM)
            .attr(minidom::rxml::xml_ncname!("previd").to_owned(), "old-sm-id")
            .build()],
        vec![resumed("old-sm-id", "not-a-number")],
        vec![resumed("old-sm-id", "11"), failed(None)],
        vec![failed(None), failed(None)],
        vec![resumed("old-sm-id", "11"), bound()],
        vec![failed(None), bound(), bound()],
        vec![Element::builder("failed", NS_SM)
            .append(Element::builder("text", NS_STANZAS).append("gone").build())
            .build()],
        vec![Element::builder("enabled", NS_SM).build()],
    ] {
        let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());
        let before = runtime.resume_state();

        let error =
            receive(&mut runtime, success("alice@example.com", results.clone())).unwrap_err();

        assert!(
            matches!(error, ClientError::InvalidSasl2InlineResult),
            "{results:?}: {error:?}"
        );
        assert_eq!(runtime.snapshot().phase, SessionPhase::Disconnecting);
        assert_eq!(runtime.snapshot().binding, None);
        assert_eq!(runtime.resume_state(), before, "{results:?}");
        assert!(!runtime.can_send_app_stanza());
    }
}

#[test]
fn xep0386_bound_is_always_an_unrequested_result() {
    for (identity, results) in [
        (
            "alice@example.com/assigned-by-server",
            vec![failed(None), bound()],
        ),
        ("alice@example.com/assigned-by-server", vec![bound()]),
        (
            "alice@example.com",
            vec![
                failed(None),
                Element::builder("future", "urn:example:feature").build(),
            ],
        ),
    ] {
        let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());
        let before = runtime.resume_state();

        let error = receive(&mut runtime, success(identity, results.clone())).unwrap_err();

        assert!(
            matches!(error, ClientError::UnexpectedSasl2InlineResult),
            "{results:?}: {error:?}"
        );
        assert_eq!(runtime.snapshot().phase, SessionPhase::Disconnecting);
        assert_eq!(runtime.snapshot().binding, None);
        assert_eq!(runtime.resume_state(), before, "{results:?}");
    }
}

#[test]
fn xep0388_inline_success_must_authorize_the_configured_account() {
    for (identity, results) in [
        ("mallory@example.com", vec![resumed("old-sm-id", "11")]),
        ("mallory@example.com", vec![failed(None)]),
        // No resource is bound after a failed resumption.
        ("alice@example.com/phone", vec![failed(None)]),
        ("alice@example.com/phone", Vec::new()),
    ] {
        let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());
        let before = runtime.resume_state();

        let error = receive(&mut runtime, success(identity, results)).unwrap_err();

        assert!(
            matches!(error, ClientError::Sasl2AuthorizationIdentityMismatch),
            "{identity}: {error:?}"
        );
        assert_eq!(runtime.snapshot().phase, SessionPhase::Disconnecting);
        assert_eq!(runtime.resume_state(), before, "{identity}");
    }
}

#[test]
fn xep0388_nothing_is_sent_between_authenticate_and_the_terminal_element() {
    let (mut runtime, requested) = runtime_after_features(Some(resume_state()), full_offer());
    let authenticate = authenticate(&requested);

    // Writing the request releases neither the retained tail nor any stanza.
    let written = sent(&mut runtime, authenticate);
    assert!(outbound(&written).is_empty());
    assert!(!runtime.can_send_app_stanza());

    // Everything the resumption releases is ordered after the success.
    let events = receive(
        &mut runtime,
        success("alice@example.com", vec![resumed("old-sm-id", "10")]),
    )
    .unwrap();
    let succeeded = events
        .iter()
        .position(|event| {
            matches!(
                event,
                ClientEvent::Connection(ConnectionEvent::AuthenticationSucceeded)
            )
        })
        .expect("authentication success event");
    let first_write = events
        .iter()
        .position(|event| {
            matches!(
                event,
                ClientEvent::Connection(ConnectionEvent::OutboundMessage(_))
            )
        })
        .expect("replayed tail");
    assert!(succeeded < first_write);
    assert_eq!(
        outbound(&events)
            .iter()
            .map(|element| element.attr("id"))
            .collect::<Vec<_>>(),
        vec![Some("m-11"), Some("m-12"), None]
    );
}

#[test]
fn xep0388_failure_ignores_inline_requests_and_keeps_the_session_resumable() {
    let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());
    let before = runtime.resume_state();

    let events = receive(
        &mut runtime,
        Element::builder("failure", NS_SASL2)
            .append(Element::builder("not-authorized", NS_SASL).build())
            .build(),
    )
    .unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::AuthenticationFailed(_))
    )));
    assert!(outbound(&events).is_empty());
    assert!(!sm_failed(&events));
    assert_eq!(runtime.snapshot().phase, SessionPhase::Disconnecting);
    assert_eq!(runtime.resume_state(), before);
}

#[test]
fn xep0198_stream_error_answering_an_inline_resume_discards_the_session() {
    let (mut runtime, _) = runtime_after_features(Some(resume_state()), full_offer());

    let events = receive(
        &mut runtime,
        Element::builder("error", NS_STREAMS)
            .append(Element::builder("internal-server-error", NS_STREAM_ERRORS).build())
            .build(),
    )
    .unwrap();

    assert!(sm_failed(&events));
    assert!(delivery_failed(&events, "m-11"));
    assert!(delivery_failed(&events, "m-12"));
    assert_eq!(runtime.resume_state(), None);

    // The next connection authenticates without repeating the resumption.
    runtime
        .apply_transport_event(TransportEvent::Closed)
        .unwrap();
    let requested = connect(&mut runtime, full_offer());
    assert_eq!(
        serialised(&authenticate(&requested)),
        serialised(&plain_sasl2_authenticate())
    );
}

#[test]
fn xep0198_stream_error_during_plain_sasl2_authentication_keeps_the_session() {
    // No `<resume/>` is outstanding, so the stream error says nothing about
    // whether the session can still be resumed.
    let (mut runtime, _) = runtime_after_features(Some(resume_state()), sasl2_features(None));
    let before = runtime.resume_state();

    let events = receive(
        &mut runtime,
        Element::builder("error", NS_STREAMS)
            .append(Element::builder("internal-server-error", NS_STREAM_ERRORS).build())
            .build(),
    )
    .unwrap();

    assert!(!sm_failed(&events));
    assert_eq!(runtime.resume_state(), before);
}
