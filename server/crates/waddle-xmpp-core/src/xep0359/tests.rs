use super::*;
use xmpp_parsers::message::Message;

#[test]
fn test_is_stanza_id_element() {
    let elem = Element::builder("stanza-id", NS_SID)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
        .attr(
            minidom::rxml::xml_ncname!("by").to_owned(),
            "room@muc.example.com",
        )
        .build();
    assert!(is_stanza_id_element(&elem));

    let wrong = Element::builder("origin-id", NS_SID).build();
    assert!(!is_stanza_id_element(&wrong));
}

#[test]
fn test_is_origin_id_element() {
    let elem = Element::builder("origin-id", NS_SID)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
        .build();
    assert!(is_origin_id_element(&elem));

    let wrong = Element::builder("stanza-id", NS_SID).build();
    assert!(!is_origin_id_element(&wrong));
}

#[test]
fn test_extract_stanza_ids() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                <body>Hello</body>\
                <stanza-id xmlns='urn:xmpp:sid:0' id='archive-1' by='room@muc.example.com'/>\
                <stanza-id xmlns='urn:xmpp:sid:0' id='archive-2' by='example.com'/>\
                </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let ids = extract_stanza_ids(&msg);
    assert_eq!(ids.len(), 2);
    assert_eq!(
        ids[0],
        StanzaId::new(
            "archive-1",
            "room@muc.example.com".parse().expect("valid jid")
        )
    );
    assert_eq!(
        ids[1],
        StanzaId::new("archive-2", "example.com".parse().expect("valid jid"))
    );
}

#[test]
fn test_extract_stanza_id_by() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                <body>Hello</body>\
                <stanza-id xmlns='urn:xmpp:sid:0' id='arc-1' by='room@muc.example.com'/>\
                </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let other: jid::Jid = "other@example.com".parse().expect("valid jid");
    assert_eq!(extract_stanza_id_by(&msg, &room), Some("arc-1".to_owned()));
    assert_eq!(extract_stanza_id_by(&msg, &other), None);
}

#[test]
fn test_extract_stanza_id_by_matches_case_folded_bare_jid() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                <body>Hello</body>\
                <stanza-id xmlns='urn:xmpp:sid:0' id='arc-1' by='room@muc.example.COM'/>\
                </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let lookup: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    assert_eq!(
        extract_stanza_id_by(&msg, &lookup),
        Some("arc-1".to_owned())
    );
    assert_eq!(msg.stanza_id_by(&lookup), Some("arc-1".to_owned()));
}

#[test]
fn test_extract_origin_id() {
    let xml = "<message xmlns='jabber:client' type='chat'>\
                <body>Hello</body>\
                <origin-id xmlns='urn:xmpp:sid:0' id='client-uuid-1'/>\
                </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let oid = extract_origin_id(&msg).expect("has origin-id");
    assert_eq!(oid.id, "client-uuid-1");
    assert_eq!(
        extract_origin_id_str(&msg),
        Some("client-uuid-1".to_owned())
    );
}

#[test]
fn test_extract_origin_id_absent() {
    let msg = Message::new(None::<jid::Jid>);
    assert!(extract_origin_id(&msg).is_none());
}

#[test]
fn test_extract_stanza_id_empty_attrs_ignored() {
    let xml = "<message xmlns='jabber:client' type='chat'>\
                <stanza-id xmlns='urn:xmpp:sid:0' id='' by='example.com'/>\
                </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(extract_stanza_ids(&msg).is_empty());
}

#[test]
fn test_extract_stanza_id_invalid_by_ignored() {
    // A `by=` attribute that fails JID parsing (here: a bare slash,
    // i.e. an empty resource on no domain) is silently dropped,
    // mirroring the existing "filter empty `by`" defensive behavior.
    let xml = "<message xmlns='jabber:client' type='chat'>\
                <stanza-id xmlns='urn:xmpp:sid:0' id='arc-1' by='/just-resource'/>\
                </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(
        extract_stanza_ids(&msg).is_empty(),
        "stanza-id with unparseable by= must be skipped"
    );
}

#[test]
fn test_build_stanza_id_element() {
    let by: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let elem = build_stanza_id_element("arc-99", &by);
    assert_eq!(elem.name(), "stanza-id");
    assert_eq!(elem.ns(), NS_SID);
    assert_eq!(elem.attr("id"), Some("arc-99"));
    assert_eq!(elem.attr("by"), Some("room@muc.example.com"));
}

#[test]
fn test_build_origin_id_element() {
    let elem = build_origin_id_element("client-1");
    assert_eq!(elem.name(), "origin-id");
    assert_eq!(elem.ns(), NS_SID);
    assert_eq!(elem.attr("id"), Some("client-1"));
}

#[test]
fn test_add_stanza_id() {
    let mut msg = Message::new(None::<jid::Jid>);
    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let server: jid::Jid = "example.com".parse().expect("valid jid");
    add_stanza_id(&mut msg, &StanzaId::new("arc-1", room));
    add_stanza_id(&mut msg, &StanzaId::new("arc-2", server));

    let ids = extract_stanza_ids(&msg);
    assert_eq!(ids.len(), 2);
}

#[test]
fn test_add_stanza_id_replaces_existing_same_by() {
    let mut msg = Message::new(None::<jid::Jid>);
    let alice: jid::Jid = "alice@example.com".parse().expect("valid jid");
    msg.payloads
        .push(build_stanza_id_element("spoofed", &alice));

    add_stanza_id(&mut msg, &StanzaId::new("fresh", alice.clone()));

    let ids = extract_stanza_ids(&msg);
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], StanzaId::new("fresh", alice));
}

#[test]
fn test_add_origin_id() {
    let mut msg = Message::new(None::<jid::Jid>);
    add_origin_id(&mut msg, "client-1");
    assert!(has_origin_id(&msg));

    // Adding again is no-op
    add_origin_id(&mut msg, "client-2");
    assert_eq!(
        msg.payloads
            .iter()
            .filter(|e| is_origin_id_element(e))
            .count(),
        1
    );
}

#[test]
fn test_remove_stanza_ids_by() {
    let mut msg = Message::new(None::<jid::Jid>);
    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let server: jid::Jid = "example.com".parse().expect("valid jid");
    add_stanza_id(&mut msg, &StanzaId::new("arc-1", room.clone()));
    add_stanza_id(&mut msg, &StanzaId::new("arc-2", server.clone()));

    remove_stanza_ids_by(&mut msg, &room);
    let ids = extract_stanza_ids(&msg);
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].by, server);
}

#[test]
fn test_remove_stanza_ids_by_matches_case_folded_bare_jid() {
    let mut msg = Message::new(None::<jid::Jid>);
    let room_uppercase: jid::Jid = "room@muc.example.COM".parse().expect("valid jid");
    let server: jid::Jid = "example.com".parse().expect("valid jid");
    msg.payloads
        .push(build_stanza_id_element("arc-1", &room_uppercase));
    msg.payloads.push(build_stanza_id_element("arc-2", &server));

    let room_lookup: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    remove_stanza_ids_by(&mut msg, &room_lookup);
    let ids = extract_stanza_ids(&msg);
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].by, server);
}

#[test]
fn test_strip_all_ids() {
    let mut msg = Message::new(None::<jid::Jid>);
    let server: jid::Jid = "example.com".parse().expect("valid jid");
    add_stanza_id(&mut msg, &StanzaId::new("arc-1", server));
    add_origin_id(&mut msg, "client-1");
    msg.payloads
        .push(Element::builder("body", "jabber:client").build());

    strip_all_ids(&mut msg);
    assert!(!has_stanza_id(&msg));
    assert!(!has_origin_id(&msg));
    // Non-SID payloads preserved
    assert_eq!(msg.payloads.len(), 1);
}

#[test]
fn test_stanza_id_carrier_trait() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                <body>Test</body>\
                <stanza-id xmlns='urn:xmpp:sid:0' id='arc-1' by='room@muc.example.com'/>\
                <origin-id xmlns='urn:xmpp:sid:0' id='client-1'/>\
                </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    assert!(msg.has_stanza_id());
    assert_eq!(msg.stanza_id_by(&room), Some("arc-1".to_owned()));
    assert_eq!(msg.origin_id(), Some(OriginId::new("client-1")));
}

#[test]
fn test_conversion_from_xmpp_parsers() {
    let sid: StanzaId = xmpp_parsers::stanza_id::StanzaId {
        id: "abc".to_owned(),
        by: "room@muc.example.com".parse().expect("valid jid"),
    }
    .into();
    assert_eq!(sid.id, "abc");
    assert_eq!(
        sid.by,
        "room@muc.example.com"
            .parse::<jid::Jid>()
            .expect("valid jid")
    );

    let oid: OriginId = xmpp_parsers::stanza_id::OriginId {
        id: "def".to_owned(),
    }
    .into();
    assert_eq!(oid.id, "def");
}
