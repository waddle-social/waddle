use super::*;

use xmpp_parsers::message::{Message, MessageType};

#[test]
fn test_is_retract_element() {
    let retract = Element::builder("retract", NS_MESSAGE_RETRACT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "orig-1")
        .build();
    assert!(is_retract_element(&retract));

    let wrong_ns = Element::builder("retract", "jabber:client").build();
    assert!(!is_retract_element(&wrong_ns));

    let retracted = Element::builder("retracted", NS_MESSAGE_RETRACT).build();
    assert!(!is_retract_element(&retracted));
}

#[test]
fn test_is_retracted_element() {
    let retracted = Element::builder("retracted", NS_MESSAGE_RETRACT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "retract-1")
        .attr(
            minidom::rxml::xml_ncname!("stamp").to_owned(),
            "2024-01-15T12:00:00Z",
        )
        .build();
    assert!(is_retracted_element(&retracted));

    let retract = Element::builder("retract", NS_MESSAGE_RETRACT).build();
    assert!(!is_retracted_element(&retract));
}

#[test]
fn test_is_retraction_message() {
    let xml = "<message xmlns='jabber:client' type='groupchat' id='r-1'>\
                    <body>Fallback text</body>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='orig-1'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(is_retraction_message(&msg));
    assert!(!is_tombstone_message(&msg));
}

#[test]
fn test_is_tombstone_message() {
    let xml = "<message xmlns='jabber:client' type='groupchat' id='orig-1'>\
                    <retracted xmlns='urn:xmpp:message-retract:1' id='retract-1' stamp='2024-01-15T12:00:00Z'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(is_tombstone_message(&msg));
    assert!(!is_retraction_message(&msg));
}

#[test]
fn test_extract_retraction_request() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Fallback</body>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='msg-42'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let kind = extract_retraction_from_message(&msg).expect("has retraction");
    assert_eq!(kind, RetractionKind::Request(Retraction::new("msg-42")));
}

#[test]
fn test_extract_retraction_tombstone() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retracted xmlns='urn:xmpp:message-retract:1' id='retract-7' stamp='2024-06-01T09:00:00Z'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let kind = extract_retraction_from_message(&msg).expect("has retraction");
    assert_eq!(
        kind,
        RetractionKind::Tombstone(Retracted::new(
            "retract-7",
            Some("2024-06-01T09:00:00Z".to_owned()),
        ))
    );
}

#[test]
fn test_extract_retraction_absent() {
    let msg = Message::new(None::<jid::Jid>);
    assert!(extract_retraction_from_message(&msg).is_none());
}

#[test]
fn test_extract_retraction_empty_id_ignored() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retract xmlns='urn:xmpp:message-retract:1' id=''/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(extract_retraction_from_message(&msg).is_none());
}

#[test]
fn test_extract_tombstone_missing_id_ignored() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retracted xmlns='urn:xmpp:message-retract:1' stamp='2024-06-01T09:00:00Z'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(extract_retraction_from_message(&msg).is_none());
}

#[test]
fn test_extract_tombstone_without_stamp() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retracted xmlns='urn:xmpp:message-retract:1' id='retract-8'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert_eq!(
        extract_retraction_from_message(&msg),
        Some(RetractionKind::Tombstone(Retracted::new("retract-8", None)))
    );
}

#[test]
fn test_extract_retracts_id() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='abc-123'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert_eq!(extract_retracts_id(&msg), Some("abc-123".to_owned()));
}

#[test]
fn test_build_retract_element() {
    let elem = build_retract_element("msg-99");
    assert_eq!(elem.name(), "retract");
    assert_eq!(elem.ns(), NS_MESSAGE_RETRACT);
    assert_eq!(elem.attr("id"), Some("msg-99"));
}

#[test]
fn test_build_retracted_element() {
    let elem = build_retracted_element("retract-1", Some("2024-01-15T12:00:00Z"));
    assert_eq!(elem.name(), "retracted");
    assert_eq!(elem.ns(), NS_MESSAGE_RETRACT);
    assert_eq!(elem.attr("id"), Some("retract-1"));
    assert_eq!(elem.attr("stamp"), Some("2024-01-15T12:00:00Z"));
}

#[test]
fn test_build_retraction_message() {
    let to: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let from: jid::Jid = "user@example.com".parse().expect("valid jid");
    let msg = build_retraction_message(to.clone(), from.clone(), "orig-1");

    assert_eq!(msg.to, Some(to));
    assert_eq!(msg.from, Some(from));
    assert_eq!(msg.type_, MessageType::Groupchat);
    assert!(msg.id.is_some());
    assert!(!msg.bodies.is_empty()); // Has fallback body
    assert_eq!(extract_retracts_id(&msg), Some("orig-1".to_owned()));
}

#[test]
fn test_build_tombstone_message() {
    let msg = build_tombstone_message(
        None::<jid::Jid>,
        None::<jid::Jid>,
        "orig-1",
        "retract-1",
        Some("2024-01-15T12:00:00Z"),
    );

    assert_eq!(msg.id.as_ref().map(|id| id.0.as_str()), Some("orig-1"));
    assert!(is_tombstone_message(&msg));
    match extract_retraction_from_message(&msg) {
        Some(RetractionKind::Tombstone(t)) => {
            assert_eq!(t.retraction_id, "retract-1");
            assert_eq!(t.stamp.as_deref(), Some("2024-01-15T12:00:00Z"));
        }
        other => panic!("Expected tombstone, got {:?}", other),
    }
}

#[test]
fn test_set_retraction() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_retraction(&mut msg, "orig-5");
    assert_eq!(extract_retracts_id(&msg), Some("orig-5".to_owned()));

    // Setting again replaces
    set_retraction(&mut msg, "orig-6");
    assert_eq!(extract_retracts_id(&msg), Some("orig-6".to_owned()));
    let count = msg
        .payloads
        .iter()
        .filter(|e| e.ns() == NS_MESSAGE_RETRACT)
        .count();
    assert_eq!(count, 1);
}

#[test]
fn test_strip_retraction() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Fallback</body>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='orig-1'/>\
                    </message>";
    let mut msg =
        Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    strip_retraction(&mut msg);
    assert!(!is_retraction_message(&msg));
    assert!(!msg.bodies.is_empty());
}

#[test]
fn test_retraction_carrier_trait() {
    // Retraction request
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='orig-1'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(msg.is_retraction());
    assert!(!msg.is_retracted());
    assert_eq!(msg.retracts_id(), Some("orig-1".to_owned()));

    // Tombstone
    let xml2 = "<message xmlns='jabber:client' type='groupchat'>\
                     <retracted xmlns='urn:xmpp:message-retract:1' id='retract-1' stamp='2024-01-01T00:00:00Z'/>\
                     </message>";
    let msg2 =
        Message::try_from(xml2.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(!msg2.is_retraction());
    assert!(msg2.is_retracted());
    assert_eq!(msg2.retracts_id(), None);

    // Plain message
    let plain = Message::new(None::<jid::Jid>);
    assert!(!plain.is_retraction());
    assert!(!plain.is_retracted());
}

#[test]
fn test_retraction_new() {
    let r = Retraction::new("abc");
    assert_eq!(r.retracts_id, "abc");
}

#[test]
fn test_retracted_new() {
    let t = Retracted::new("retract-9", Some("2024-06-01T00:00:00Z".to_owned()));
    assert_eq!(t.retraction_id, "retract-9");
    assert_eq!(t.stamp.as_deref(), Some("2024-06-01T00:00:00Z"));
}
