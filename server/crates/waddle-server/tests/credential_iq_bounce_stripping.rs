use jid::{DomainPart, FullJid, Jid};
use minidom::Element;
use waddle_server::credential_iq::stanza_to_xml;
use waddle_server::credential_iq::{
    bounce_iq_error_xml_with_payload, bounce_outgoing_block_frames, bounce_undeliverable_iq_reply,
    bounce_unroutable_iq_reply,
};
use waddle_server::destination::UnroutableReason;
use waddle_xmpp::stream_management::StreamManagementState;
use waddle_xmpp::telemetry::attributes::SmEvictionPath;
use waddle_xmpp::waddle_credential::NS_WADDLE_CREDENTIAL;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const SENTINEL: &str = "sk-waddle-strip-top-level-sentinel-a1b2c3d4";

#[test]
fn bounce_stripping_removes_top_level_credential_payload_and_preserves_benign_waddle_iqs() {
    let cred_payload = Element::builder("enroll-credential", NS_WADDLE_CREDENTIAL)
        .append(SENTINEL)
        .build();
    let iq = Iq::Set {
        from: Some("alice@example.com/web".parse::<Jid>().expect("from")),
        to: Some("bob@example.com/phone".parse::<Jid>().expect("to")),
        id: "strip-1".to_owned(),
        payload: cred_payload.clone(),
    };

    let mut sm = StreamManagementState::new();
    sm.enable("sm-strip".to_owned(), true, Some(300));

    let r1 = stanza_to_xml(
        &bounce_undeliverable_iq_reply(&Stanza::Iq(Box::new(iq.clone()))).expect("r1"),
    );
    let sender: Jid = "alice@example.com/web".parse().expect("sender");
    let home = DomainPart::new("example.com").expect("home");
    let r2 = stanza_to_xml(&Stanza::Iq(Box::new(
        bounce_unroutable_iq_reply(&iq, &sender, &home, UnroutableReason::FederationDisabled)
            .expect("r2"),
    )));
    let sender_full: FullJid = "alice@example.com/web".parse().expect("full");
    let target: Jid = "bob@example.com/phone".parse().expect("target");
    let r3 = bounce_outgoing_block_frames(&iq, &sender_full, &target)
        .into_iter()
        .next()
        .expect("r3");
    let r4 = bounce_iq_error_xml_with_payload(
        "strip-4",
        Some("room@muc.example.com"),
        Some("alice@example.com/web"),
        cred_payload,
        StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::Forbidden,
            "en",
            "denied",
        ),
    );

    for xml in [&r1, &r2, &r3, &r4] {
        assert!(!xml.contains(SENTINEL));
        assert!(!xml.contains(NS_WADDLE_CREDENTIAL));
        let _ = sm.record_outbound(xml.clone(), SmEvictionPath::DirectOutbound);
    }

    for entry in sm.stanzas_to_resend(0) {
        assert!(!entry.stanza_xml.contains(SENTINEL));
    }

    // Benign non-credential `urn:waddle:pin:0` IQ is preserved on bounce.
    let pin_iq = Iq::Set {
        from: Some(sender.clone()),
        to: Some(target),
        id: "pin-1".to_owned(),
        payload: Element::builder("pin", "urn:waddle:pin:0").build(),
    };
    let pin_reply = stanza_to_xml(
        &bounce_undeliverable_iq_reply(&Stanza::Iq(Box::new(pin_iq))).expect("pin reply"),
    );
    assert!(pin_reply.contains("urn:waddle:pin:0"));
}
