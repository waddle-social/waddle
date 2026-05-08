use super::*;

use xmpp_parsers::message::Message;

#[test]
fn test_is_reference_element() {
    let elem = Element::builder("reference", NS_REFERENCE)
        .attr("type", "mention")
        .build();
    assert!(is_reference_element(&elem));

    let wrong_ns = Element::builder("reference", "jabber:client").build();
    assert!(!is_reference_element(&wrong_ns));
}

#[test]
fn test_extract_mentions() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello @alice and @bob</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' begin='6' end='12' uri='xmpp:alice@example.com'/>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' begin='17' end='21' uri='xmpp:bob@example.com'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let refs = extract_references_from_message(&msg);
    assert_eq!(refs.len(), 2);
    assert!(refs[0].is_mention());
    assert_eq!(refs[0].begin, Some(6));
    assert_eq!(refs[0].end, Some(12));
    assert_eq!(refs[0].uri, "xmpp:alice@example.com");
    assert_eq!(refs[0].bare_jid(), Some("alice@example.com"));
}

#[test]
fn test_extract_mentioned_jids() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>@alice @bob</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' uri='xmpp:alice@example.com'/>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' uri='xmpp:bob@example.com'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let jids = extract_mentioned_jids(&msg);
    assert_eq!(jids, vec!["alice@example.com", "bob@example.com"]);
}

#[test]
fn test_extract_no_references() {
    let msg = Message::new(None::<jid::Jid>);
    assert!(extract_references_from_message(&msg).is_empty());
}

#[test]
fn test_reference_missing_type_skipped() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <reference xmlns='urn:xmpp:reference:0' uri='xmpp:alice@example.com'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    // Missing type → skipped (not an error, just ignored)
    assert!(extract_references_from_message(&msg).is_empty());
}

#[test]
fn test_parse_reference_requires_uri() {
    let elem = Element::builder("reference", NS_REFERENCE)
        .attr("type", "mention")
        .build();

    assert!(matches!(
        parse_reference_element(&elem),
        Err(ReferenceError::MissingUri)
    ));
}

#[test]
fn test_reference_missing_uri_skipped() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    assert!(extract_references_from_message(&msg).is_empty());
}

#[test]
fn test_reference_empty_uri_skipped() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' uri=''/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    assert!(extract_references_from_message(&msg).is_empty());
}

#[test]
fn test_reference_whitespace_uri_skipped() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' uri='   '/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    assert!(extract_references_from_message(&msg).is_empty());
}

#[test]
fn test_reference_data_type() {
    let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>See the file</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='data' uri='https://files.example.com/cat.jpg'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let refs = extract_references_from_message(&msg);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].ref_type, ReferenceType::Data);
    assert!(!refs[0].is_mention());
}

#[test]
fn test_build_reference_mention() {
    let r = Reference::mention_at(6, 12, "xmpp:alice@example.com");
    let elem = build_reference_element(&r);

    assert_eq!(elem.name(), "reference");
    assert_eq!(elem.ns(), NS_REFERENCE);
    assert_eq!(elem.attr("type"), Some("mention"));
    assert_eq!(elem.attr("begin"), Some("6"));
    assert_eq!(elem.attr("end"), Some("12"));
    assert_eq!(elem.attr("uri"), Some("xmpp:alice@example.com"));
}

#[test]
fn test_build_reference_no_position() {
    let r = Reference::mention("xmpp:bob@example.com");
    let elem = build_reference_element(&r);

    assert_eq!(elem.attr("type"), Some("mention"));
    assert_eq!(elem.attr("begin"), None);
    assert_eq!(elem.attr("end"), None);
    assert_eq!(elem.attr("uri"), Some("xmpp:bob@example.com"));
}

#[test]
fn test_build_reference_with_anchor() {
    let r = Reference::mention("xmpp:alice@example.com").with_anchor("@alice");
    let elem = build_reference_element(&r);

    assert_eq!(elem.attr("anchor"), Some("@alice"));
}

#[test]
fn test_add_reference() {
    let mut msg = Message::new(None::<jid::Jid>);
    add_reference(&mut msg, &Reference::mention("xmpp:alice@example.com"));
    add_reference(&mut msg, &Reference::mention("xmpp:bob@example.com"));

    assert_eq!(extract_references_from_message(&msg).len(), 2);
}

#[test]
fn test_strip_references() {
    let mut msg = Message::new(None::<jid::Jid>);
    add_reference(&mut msg, &Reference::mention("xmpp:alice@example.com"));

    strip_references(&mut msg);
    assert!(!has_references(&msg));
}

#[test]
fn test_reference_carrier_trait() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>@alice hello</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' uri='xmpp:alice@example.com'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    assert!(msg.has_references());
    assert!(msg.has_mentions());
    assert!(msg.mentions_jid("alice@example.com"));
    assert!(!msg.mentions_jid("bob@example.com"));
    assert_eq!(msg.mentions().len(), 1);
}

#[test]
fn test_reference_type_display() {
    assert_eq!(ReferenceType::Mention.to_string(), "mention");
    assert_eq!(ReferenceType::Data.to_string(), "data");
}

#[test]
fn test_reference_new_helpers() {
    let m = Reference::mention("xmpp:a@b.com");
    assert!(m.is_mention());
    assert_eq!(m.bare_jid(), Some("a@b.com"));

    let d = Reference::data("https://example.com/file.png");
    assert!(!d.is_mention());
    assert_eq!(d.ref_type, ReferenceType::Data);
}
