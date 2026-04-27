use waddle_xmpp::xep::{
    xep0166::{
        build_jingle_ack, build_jingle_error, is_jingle_iq, parse_jingle_iq,
        validate_webrtc_jingle, JingleErrorCondition, JingleValidationError, NS_JINGLE,
    },
    xep0167::{
        build_payload_type, build_rtp_description, PayloadTypeName, RtpMedia, NS_JINGLE_RTP,
    },
    xep0176::{
        build_ice_udp_transport, ice_candidates_have_credentials, IcePassword, IceUfrag,
        NS_JINGLE_ICE_UDP,
    },
    xep0320::{
        build_dtls_fingerprint, DtlsFingerprint, DtlsFingerprintHash, DtlsSetup, NS_JINGLE_DTLS,
    },
    xep0338::{
        build_group, ContentName, GroupSemantics, FEATURE_RFC5888_GROUPING, NS_JINGLE_GROUPING,
    },
};
use xmpp_parsers::{
    iq::{Iq, IqType},
    jingle::{Action, Content, ContentId, Creator, Description, Jingle, SessionId, Transport},
    minidom::Element,
    stanza_error::{DefinedCondition, ErrorType},
};

#[test]
fn xep0166_jingle_iq_parses_and_acknowledges() {
    let iq: Iq = Iq::try_from(
        "<iq xmlns='jabber:client' from='romeo@example.test/orchard' to='media.example.test/s1' type='set' id='j1'><jingle xmlns='urn:xmpp:jingle:1' action='session-terminate' sid='s1'/></iq>"
            .parse::<Element>()
            .expect("xml"),
    )
    .expect("iq");

    assert!(is_jingle_iq(&iq));
    let jingle = parse_jingle_iq(&iq).expect("jingle");
    assert_eq!(jingle.action, Action::SessionTerminate);

    let ack = build_jingle_ack(&iq);
    assert!(matches!(ack.payload, IqType::Result(None)));
    assert_eq!(ack.from, iq.to);
    assert_eq!(ack.to, iq.from);
}

#[test]
fn xep0166_webrtc_validation_accepts_rtp_ice_udp_content() {
    let ufrag = IceUfrag::new("ufrag").expect("ufrag");
    let pwd = IcePassword::new("pwd").expect("pwd");
    let jingle = Jingle::new(Action::SessionInitiate, SessionId("s1".to_string())).add_content(
        Content::new(Creator::Initiator, ContentId("audio".to_string()))
            .with_description(Description::Unknown(build_rtp_description(RtpMedia::Audio)))
            .with_transport(Transport::Unknown(build_ice_udp_transport(
                Some(&ufrag),
                Some(&pwd),
            ))),
    );

    assert_eq!(validate_webrtc_jingle(&jingle), Ok(()));
}

#[test]
fn xep0176_candidates_require_ice_credentials() {
    let missing_credentials = Element::builder("transport", NS_JINGLE_ICE_UDP)
        .append(Element::builder("candidate", NS_JINGLE_ICE_UDP).build())
        .build();
    assert!(!ice_candidates_have_credentials(&missing_credentials));

    let jingle = Jingle::new(Action::TransportInfo, SessionId("s1".to_string())).add_content(
        Content::new(Creator::Initiator, ContentId("audio".to_string()))
            .with_transport(Transport::Unknown(missing_credentials)),
    );
    assert_eq!(
        validate_webrtc_jingle(&jingle),
        Err(JingleValidationError::MissingIceCredentials)
    );
}

#[test]
fn xep0166_jingle_error_can_include_jingle_specific_condition() {
    let iq = Iq {
        from: Some("romeo@example.test/orchard".parse().expect("jid")),
        to: Some("media.example.test/s1".parse().expect("jid")),
        id: "j2".to_string(),
        payload: IqType::Set(Element::builder("jingle", NS_JINGLE).build()),
    };
    let error = build_jingle_error(
        &iq,
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        Some(JingleErrorCondition::UnknownSession),
        "Unknown session.",
    );
    let IqType::Error(stanza_error) = error.payload else {
        panic!("expected error");
    };
    assert!(stanza_error.other.is_some());
}

#[test]
fn xep0167_0320_0338_build_webrtc_related_payloads() {
    let payload_name = PayloadTypeName::new("opus").expect("payload name");
    let payload = build_payload_type(111, Some(&payload_name), Some(48_000));
    assert_eq!(payload.ns(), NS_JINGLE_RTP);
    assert_eq!(payload.attr("name"), Some("opus"));

    let fingerprint_value = DtlsFingerprint::new("AA:BB").expect("fingerprint");
    let fingerprint = build_dtls_fingerprint(
        DtlsFingerprintHash::Sha256,
        DtlsSetup::Actpass,
        &fingerprint_value,
    );
    assert_eq!(fingerprint.ns(), NS_JINGLE_DTLS);
    assert_eq!(fingerprint.text(), "AA:BB");

    let content_names = [
        ContentName::new("audio").expect("content name"),
        ContentName::new("video").expect("content name"),
    ];
    let group = build_group(GroupSemantics::Bundle, &content_names);
    assert_eq!(group.ns(), NS_JINGLE_GROUPING);
    assert_eq!(group.attr("semantics"), Some("BUNDLE"));
    assert_eq!(FEATURE_RFC5888_GROUPING, "urn:ietf:rfc:5888");
}
