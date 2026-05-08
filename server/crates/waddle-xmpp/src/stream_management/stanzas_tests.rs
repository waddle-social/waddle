use std::str::FromStr;

use minidom::Element;

use super::stanzas::{SmAck, SmEnable, SmEnabled, SmFailed, SmRequest, SmResume, SmStanza};
use super::SM_NS;

/// Parse the given XML through minidom and return the element. Used by
/// tests below to assert against serialized output without depending on
/// the quoting/attribute-order choices the serializer happens to make.
fn parse_element(xml: &str) -> Element {
    Element::from_str(xml).expect("test fixture must be valid XML")
}

#[test]
fn test_sm_enable_parse() {
    let xml = "<enable xmlns='urn:xmpp:sm:3'/>";
    let enable = SmEnable::parse(xml).unwrap();
    assert!(!enable.resume);
    assert!(enable.max.is_none());

    let xml = "<enable xmlns='urn:xmpp:sm:3' resume='true' max='300'/>";
    let enable = SmEnable::parse(xml).unwrap();
    assert!(enable.resume);
    assert_eq!(enable.max, Some(300));
}

/// Regression guard for the `resume="1"` parsing bug. Stanza.js (the
/// WebSocket client library used by `chat/`) serializes `xs:boolean`
/// attributes in canonical form — `1`/`0`, not `true`/`false`. The old
/// string-match parser only recognised `resume='true'` / `resume="true"`,
/// so every real browser client ended up with a non-resumable SM
/// session and the entire XEP-0198 resume path was effectively disabled.
#[test]
fn test_sm_enable_parses_xs_boolean_canonical_forms() {
    // Stanza.js wire format (double-quoted xs:boolean "1"):
    let enable = SmEnable::parse(r#"<enable xmlns="urn:xmpp:sm:3" resume="1"/>"#).unwrap();
    assert!(
        enable.resume,
        "resume=\"1\" is xs:boolean true — must parse as resume request"
    );

    // Single-quoted variant:
    let enable = SmEnable::parse("<enable xmlns='urn:xmpp:sm:3' resume='1'/>").unwrap();
    assert!(enable.resume);

    // Canonical xs:boolean false (`0`) must remain false.
    let enable = SmEnable::parse(r#"<enable xmlns="urn:xmpp:sm:3" resume="0"/>"#).unwrap();
    assert!(!enable.resume);

    // Unrecognised values fall back to false (XMPP convention for
    // optional boolean attributes).
    let enable = SmEnable::parse(r#"<enable xmlns="urn:xmpp:sm:3" resume="yes"/>"#).unwrap();
    assert!(!enable.resume);
}

#[test]
fn test_sm_enabled_to_xml() {
    let enabled = SmEnabled::new("stream-123".to_string());
    let element = parse_element(&enabled.to_xml());
    assert_eq!(element.name(), "enabled");
    assert_eq!(element.ns(), SM_NS);
    assert_eq!(element.attr("id"), Some("stream-123"));
    assert_eq!(element.attr("resume"), None);

    let enabled = SmEnabled::with_resume("stream-456".to_string(), 300);
    let element = parse_element(&enabled.to_xml());
    assert_eq!(element.attr("resume"), Some("true"));
    assert_eq!(element.attr("max"), Some("300"));
}

#[test]
fn test_sm_request() {
    assert!(SmRequest::is_request("<r xmlns='urn:xmpp:sm:3'/>"));
    // Bare `<r/>` (no xmlns) is NOT an SM request — the old parser
    // accepted it, which mis-classified any `<r>` in another namespace
    // as stream management.
    assert!(!SmRequest::is_request("<r/>"));
    assert!(!SmRequest::is_request("<message/>"));
}

#[test]
fn test_sm_ack_parse_and_serialize() {
    let xml = "<a xmlns='urn:xmpp:sm:3' h='5'/>";
    let ack = SmAck::parse(xml).unwrap();
    assert_eq!(ack.h, 5);

    let element = parse_element(&ack.to_xml());
    assert_eq!(element.name(), "a");
    assert_eq!(element.ns(), SM_NS);
    assert_eq!(element.attr("h"), Some("5"));
}

/// The old string-match parser used `xml.find("h=")`, which happily
/// matched the `h=` substring inside attributes like `bah="99"`. Proper
/// XML parsing rejects that ambiguity. Guard against regressing back to
/// substring search.
#[test]
fn test_sm_ack_is_not_fooled_by_attribute_name_prefix_collision() {
    let xml = r#"<a xmlns="urn:xmpp:sm:3" bah="99" h="7"/>"#;
    let ack = SmAck::parse(xml).expect("should parse");
    assert_eq!(
        ack.h, 7,
        "must read the real `h` attribute, not a substring match of `bah`"
    );
}

#[test]
fn test_sm_failed() {
    let failed = SmFailed::with_condition("item-not-found");
    let element = parse_element(&failed.to_xml());
    assert_eq!(element.name(), "failed");
    assert_eq!(element.ns(), SM_NS);
    let condition = element
        .children()
        .find(|child| child.name() == "item-not-found")
        .expect("condition child");
    assert_eq!(condition.ns(), "urn:ietf:params:xml:ns:xmpp-stanzas");

    let failed = SmFailed::resume_failed("item-not-found", 10);
    let element = parse_element(&failed.to_xml());
    assert_eq!(element.attr("h"), Some("10"));
}

#[test]
fn test_sm_stanza_parse() {
    // Enable
    let enable = SmStanza::parse("<enable xmlns='urn:xmpp:sm:3' resume='true'/>");
    assert!(matches!(enable, Some(SmStanza::Enable(_))));

    // Request
    let request = SmStanza::parse("<r xmlns='urn:xmpp:sm:3'/>");
    assert!(matches!(request, Some(SmStanza::Request)));

    // Ack
    let ack = SmStanza::parse("<a xmlns='urn:xmpp:sm:3' h='10'/>");
    assert!(matches!(ack, Some(SmStanza::Ack(_))));

    // Non-SM stanza
    let other = SmStanza::parse("<message/>");
    assert!(other.is_none());
}

#[test]
fn test_sm_stanza_candidate_prefilter() {
    assert!(SmStanza::is_client_nonza_candidate(
        "<enable xmlns='urn:xmpp:sm:3'/>"
    ));
    assert!(SmStanza::is_client_nonza_candidate(
        "<resume xmlns='urn:xmpp:sm:3' previd='id' h='1'/>"
    ));
    assert!(SmStanza::is_client_nonza_candidate(
        "<r xmlns='urn:xmpp:sm:3'/>"
    ));
    assert!(SmStanza::is_client_nonza_candidate(
        "<a xmlns='urn:xmpp:sm:3' h='4'/>"
    ));

    assert!(!SmStanza::is_client_nonza_candidate(
        "<message xmlns='jabber:client'/>"
    ));
    assert!(!SmStanza::is_client_nonza_candidate("<r/>"));
}

#[test]
fn test_sm_resume_parse() {
    let xml = "<resume xmlns='urn:xmpp:sm:3' previd='stream-123' h='5'/>";
    let resume = SmResume::parse(xml).unwrap();
    assert_eq!(resume.previd, "stream-123");
    assert_eq!(resume.h, 5);
}
