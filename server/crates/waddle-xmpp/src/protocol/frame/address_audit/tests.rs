use super::inspect_stanza_addresses;
use crate::protocol::frame::{parse_frame_with_metadata, InboundFrame, CLIENT_STANZA_NS};
use crate::Stanza;
use xmpp_parsers::minidom::rxml::xml_ncname;
use xmpp_parsers::minidom::Element;

#[test]
fn decoded_character_reference_is_captured_before_preparation() {
    let frame = "<message xmlns='jabber:client' to='fu&#xDF;ball@home.example.test' type='chat'/>";
    let audit = inspect_stanza_addresses(frame).expect("audit");
    assert_eq!(
        audit.to.original.as_ref().expect("original").as_str(),
        "fußball@home.example.test"
    );
    assert_eq!(
        audit.to.prepared.as_ref().expect("prepared").as_str(),
        "fussball@home.example.test"
    );
    assert!(audit.from.original.is_none());
    assert!(audit.from.prepared.is_none());
    let parsed = parse_frame_with_metadata(frame).expect("live parser");
    let InboundFrame::Stanza(stanza) = parsed.frame else {
        panic!("stanza")
    };
    let Stanza::Message(message) = *stanza else {
        panic!("message")
    };
    assert_eq!(message.to, audit.to.prepared);
}

#[test]
fn all_stanza_kinds_keep_original_and_prepared_addresses() {
    for kind in ["iq", "message", "presence"] {
        let mut builder = Element::builder(kind, CLIENT_STANZA_NS)
            .attr(xml_ncname!("id").to_owned(), "audit")
            .attr(
                xml_ncname!("from").to_owned(),
                "alice@faß.example.test/resource",
            )
            .attr(xml_ncname!("to").to_owned(), "bob@home.example.test.");
        if kind == "iq" {
            builder = builder.attr(xml_ncname!("type").to_owned(), "result");
        }
        let frame = String::from(&builder.build());
        let audit = inspect_stanza_addresses(&frame).expect("audit");
        assert_eq!(
            audit.from.original.expect("original").as_str(),
            "alice@faß.example.test/resource"
        );
        assert_eq!(
            audit.from.prepared.expect("prepared").as_str(),
            "alice@fass.example.test/resource"
        );
        assert_eq!(
            audit.to.prepared.expect("trailing dot").as_str(),
            "bob@home.example.test."
        );
    }
}

#[test]
fn malformed_and_non_stanza_inputs_fail_instead_of_looking_empty() {
    for frame in [
        "",
        "<message",
        "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing'/>",
        "<message xmlns='jabber:client' to='@'/>",
    ] {
        assert!(inspect_stanza_addresses(frame).is_err());
    }
}
