use waddle_xmpp::xep::xep0482::{
    build_accept_element, build_invite_element, build_jingle_method, extract_call_invite_payload,
    has_call_invite_payload, parse_call_invite_payload, try_extract_call_invite_payload,
    CallInvite, CallInviteError, CallInviteId, CallInvitePayload, JingleSessionId, JoinMethod,
    NS_CALL_INVITES,
};
use xmpp_parsers::{message::Message, minidom::Element};

#[test]
fn xep0482_invite_round_trips_jingle_join_method() {
    let media_jid: jid::Jid = "media.example.test/sid-a".parse().expect("jid");
    let invite = CallInvite {
        audio: true,
        video: true,
        methods: vec![JoinMethod::Jingle {
            sid: JingleSessionId::new("sid-a").expect("sid"),
            jid: Some(media_jid.clone()),
        }],
    };

    let elem = build_invite_element(&invite);
    assert_eq!(elem.name(), "invite");
    assert_eq!(elem.ns(), NS_CALL_INVITES);
    assert_eq!(elem.attr("video"), Some("true"));

    let parsed = parse_call_invite_payload(&elem).expect("call invite");
    let CallInvitePayload::Invite(parsed_invite) = parsed else {
        panic!("expected invite");
    };
    assert_eq!(
        parsed_invite.methods[0]
            .jingle_sid()
            .map(JingleSessionId::as_str),
        Some("sid-a")
    );
    assert_eq!(parsed_invite.methods[0].jingle_jid(), Some(&media_jid));
}

#[test]
fn xep0482_lifecycle_payload_references_invite_id() {
    let invite_id = CallInviteId::new("room-stanza-id").expect("invite id");
    let method = JoinMethod::Jingle {
        sid: JingleSessionId::new("sid-a").expect("sid"),
        jid: None,
    };
    let elem = build_accept_element(&invite_id, &method);
    let parsed = parse_call_invite_payload(&elem).expect("accept");
    assert_eq!(
        parsed.reference_id().map(CallInviteId::as_str),
        Some("room-stanza-id")
    );
}

#[test]
fn xep0482_message_detection_is_namespace_strict() {
    let mut message = Message::new(None);
    message
        .payloads
        .push(Element::builder("invite", "urn:not-call-invites").build());
    assert!(!has_call_invite_payload(&message));

    message
        .payloads
        .push(build_invite_element(&CallInvite::new().with_method(
            JoinMethod::Jingle {
                sid: JingleSessionId::new("sid-a").expect("sid"),
                jid: None,
            },
        )));
    assert!(has_call_invite_payload(&message));
    assert!(matches!(
        extract_call_invite_payload(&message),
        Some(CallInvitePayload::Invite(_))
    ));
}

#[test]
fn xep0482_jingle_method_requires_sid() {
    let elem: Element = "<invite xmlns='urn:xmpp:call-invites:0'><jingle/></invite>"
        .parse()
        .expect("xml");
    assert_eq!(
        parse_call_invite_payload(&elem),
        Err(CallInviteError::MissingAttribute("sid"))
    );
    let sid = JingleSessionId::new("sid-a").expect("sid");
    let elem = build_jingle_method(&sid, None);
    assert_eq!(elem.attr("sid"), Some("sid-a"));
}

#[test]
fn xep0482_try_extract_preserves_malformed_payload_errors() {
    let mut message = Message::new(None);
    let malformed: Element = "<invite xmlns='urn:xmpp:call-invites:0'><jingle/></invite>"
        .parse()
        .expect("xml");
    message.payloads.push(malformed);

    assert_eq!(
        try_extract_call_invite_payload(&message),
        Err(CallInviteError::MissingAttribute("sid"))
    );
}
