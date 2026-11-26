use jid::{DomainPart, FullJid, Jid};
use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_server::credential_iq::stanza_to_xml;
use waddle_server::credential_iq::{
    bounce_iq_error_xml_with_payload, bounce_outgoing_block_frames, bounce_undeliverable_iq_reply,
    bounce_unroutable_iq_reply,
};
use waddle_server::destination::UnroutableReason;
use waddle_xmpp::waddle_credential::NS_WADDLE_CREDENTIAL;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const NESTED_SENTINEL: &str = "sk-waddle-deeply-nested-sentinel-deadbeef";

fn build_deeply_nested_payload() -> Element {
    Element::builder("query", "http://jabber.org/protocol/muc#admin")
        .append(
            Element::builder("item", "http://jabber.org/protocol/muc#admin")
                .attr(xml_ncname!("affiliation").to_owned(), "member")
                .build(),
        )
        .append(
            Element::builder("wrapper", "urn:test:outer")
                .append(
                    Element::builder("level2", "urn:test:inner")
                        .append(
                            Element::builder("enroll-credential", NS_WADDLE_CREDENTIAL)
                                .append(NESTED_SENTINEL)
                                .build(),
                        )
                        .append(
                            Element::builder("legacy", "urn:waddle:inference:credential:1")
                                .append(NESTED_SENTINEL)
                                .build(),
                        )
                        .append(
                            Element::builder("benign-leaf", "urn:test:inner")
                                .append("keep-me")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

#[test]
fn four_bounce_paths_recursively_strip_nested_credential_payloads() {
    let nested = build_deeply_nested_payload();
    let iq = Iq::Set {
        from: Some("alice@example.com/web".parse::<Jid>().expect("from")),
        to: Some("bob@example.com/phone".parse::<Jid>().expect("to")),
        id: "nested-1".to_owned(),
        payload: nested.clone(),
    };

    // 1. route_to_connection::undeliverable_iq_reply
    let out1 = stanza_to_xml(
        &bounce_undeliverable_iq_reply(&Stanza::Iq(Box::new(iq.clone()))).expect("out1"),
    );

    // 2. destination::unroutable_iq_reply
    let sender: Jid = "alice@example.com/web".parse().expect("sender");
    let home = DomainPart::new("example.com").expect("home");
    let out2 = stanza_to_xml(&Stanza::Iq(Box::new(
        bounce_unroutable_iq_reply(&iq, &sender, &home, UnroutableReason::FederationDisabled)
            .expect("out2"),
    )));

    // 3. full_jid_forward::outgoing_block_frames
    let sender_full: FullJid = "alice@example.com/web".parse().expect("full");
    let target: Jid = "bob@example.com/phone".parse().expect("target");
    let out3 = bounce_outgoing_block_frames(&iq, &sender_full, &target)
        .into_iter()
        .next()
        .expect("out3");

    // 4. transport_xml::build_iq_error_xml_with_payload (muc_admin)
    let out4 = bounce_iq_error_xml_with_payload(
        "nested-4",
        Some("room@muc.example.com"),
        Some("alice@example.com/web"),
        nested,
        StanzaError::new(ErrorType::Auth, DefinedCondition::Forbidden, "en", "denied"),
    );

    for (idx, xml) in [&out1, &out2, &out3, &out4].iter().enumerate() {
        assert!(
            !xml.contains(NESTED_SENTINEL),
            "bounce path {} leaked nested sentinel",
            idx + 1
        );
        assert!(
            !xml.contains(NS_WADDLE_CREDENTIAL),
            "bounce path {} leaked credential namespace",
            idx + 1
        );
        assert!(
            !xml.contains("urn:waddle:inference:credential:1"),
            "bounce path {} leaked inference credential namespace",
            idx + 1
        );
        assert!(
            xml.contains("keep-me"),
            "bounce path {} should preserve benign sibling",
            idx + 1
        );
    }
}
