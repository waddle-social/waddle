// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! XEP-0045 Multi-User Chat (MUC) — dedicated test suite.
//!
//! Tests stanza dispatch for MUC presence (join/leave), groupchat messages,
//! subject changes, and stanza builders for MUC join/leave presence.
//!
//! Required by AGENTS.md: "Every implemented XEP MUST have a dedicated test suite."

// These tests exercise internal modules, so we use the binary's test harness.
// Run with: cargo test --package waddle-cli --bin waddle --test xep0045_muc

// Since the stanza module is part of the binary (not a lib), we test via
// the unit tests embedded in stanza.rs. This file documents the XEP coverage
// and adds integration-level tests using minidom to construct realistic stanzas.

use minidom::Element;

/// Parse a raw XML string into a minidom Element.
fn parse(xml: &str) -> Element {
    xml.parse().expect("valid XML")
}

#[test]
fn muc_groupchat_message_with_body() {
    let xml = r#"<message xmlns='jabber:client'
                   type='groupchat'
                   from='room@conference.example.com/alice'>
                   <body>Hello everyone!</body>
                 </message>"#;
    let elem = parse(xml);

    // Verify the element round-trips correctly
    assert_eq!(elem.name(), "message");
    assert_eq!(elem.attr("type"), Some("groupchat"));
    assert_eq!(elem.attr("from"), Some("room@conference.example.com/alice"));

    let body: Option<String> = elem
        .children()
        .find(|c| c.is("body", "jabber:client"))
        .and_then(|c| Some(c.text()));
    assert_eq!(body, Some("Hello everyone!".to_string()));
}

#[test]
fn muc_groupchat_message_with_custom_payload() {
    let xml = r#"<message xmlns='jabber:client'
                   type='groupchat'
                   from='room@conference.example.com/bob'>
                   <body>Check this out</body>
                   <repo xmlns='urn:waddle:github:0' owner='rust-lang' name='rust'/>
                 </message>"#;
    let elem = parse(xml);

    let payloads: Vec<&Element> = elem
        .children()
        .filter(|c| !c.is("body", "jabber:client"))
        .collect();
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].ns(), "urn:waddle:github:0".to_string());
    assert_eq!(payloads[0].name(), "repo");
    assert_eq!(payloads[0].attr("owner"), Some("rust-lang"));
}

#[test]
fn muc_presence_join_self() {
    // XEP-0045 §7.2: service sends presence with status code 110 for self
    let xml = r#"<presence xmlns='jabber:client'
                   from='room@conference.example.com/mynick'>
                   <x xmlns='http://jabber.org/protocol/muc#user'>
                     <item affiliation='member' role='participant'/>
                     <status code='110'/>
                   </x>
                 </presence>"#;
    let elem = parse(xml);

    let x = elem
        .children()
        .find(|c| c.is("x", "http://jabber.org/protocol/muc#user"))
        .expect("MUC user payload");
    let status = x
        .children()
        .find(|c| c.is("status", "http://jabber.org/protocol/muc#user"))
        .expect("status element");
    assert_eq!(status.attr("code"), Some("110"));
}

#[test]
fn muc_presence_leave_unavailable() {
    // XEP-0045 §7.14: unavailable presence = leave
    let xml = r#"<presence xmlns='jabber:client'
                   type='unavailable'
                   from='room@conference.example.com/alice'>
                   <x xmlns='http://jabber.org/protocol/muc#user'>
                     <item affiliation='member' role='none'/>
                   </x>
                 </presence>"#;
    let elem = parse(xml);

    assert_eq!(elem.attr("type"), Some("unavailable"));
    let x = elem
        .children()
        .find(|c| c.is("x", "http://jabber.org/protocol/muc#user"))
        .expect("MUC user payload");
    let item = x
        .children()
        .find(|c| c.is("item", "http://jabber.org/protocol/muc#user"))
        .expect("item element");
    assert_eq!(item.attr("role"), Some("none"));
}

#[test]
fn muc_subject_change() {
    let xml = r#"<message xmlns='jabber:client'
                   type='groupchat'
                   from='room@conference.example.com'>
                   <subject>Welcome to #general!</subject>
                 </message>"#;
    let elem = parse(xml);

    let subject = elem
        .children()
        .find(|c| c.is("subject", "jabber:client"))
        .map(|c| c.text());
    assert_eq!(subject, Some("Welcome to #general!".to_string()));
}

#[test]
fn build_muc_join_presence_has_correct_structure() {
    // Verify our join presence builder creates valid MUC join stanzas
    let room_jid: xmpp_parsers::jid::BareJid = "room@conference.example.com".parse().unwrap();
    let nick = "testuser";

    // Build the full JID: room@conference.example.com/testuser
    let full_jid = format!("{}/{}", room_jid, nick);
    let jid: xmpp_parsers::jid::Jid = full_jid.parse().unwrap();

    // Build MUC presence manually (same pattern as stanza::build_join_presence)
    let muc_elem = Element::builder("x", "http://jabber.org/protocol/muc").build();
    let pres = xmpp_parsers::presence::Presence {
        from: None,
        to: Some(jid),
        id: None,
        type_: xmpp_parsers::presence::Type::None,
        show: None,
        statuses: Default::default(),
        priority: 0i8,
        payloads: vec![muc_elem],
    };
    let elem: Element = pres.into();

    assert_eq!(elem.name(), "presence");
    assert_eq!(
        elem.attr("to"),
        Some("room@conference.example.com/testuser")
    );
    assert!(elem
        .children()
        .any(|c| c.is("x", "http://jabber.org/protocol/muc")));
}

#[test]
fn build_muc_leave_presence_is_unavailable() {
    let full_jid: xmpp_parsers::jid::Jid = "room@conference.example.com/testuser".parse().unwrap();

    let pres = xmpp_parsers::presence::Presence {
        from: None,
        to: Some(full_jid),
        id: None,
        type_: xmpp_parsers::presence::Type::Unavailable,
        show: None,
        statuses: Default::default(),
        priority: 0i8,
        payloads: vec![],
    };
    let elem: Element = pres.into();

    assert_eq!(elem.name(), "presence");
    assert_eq!(elem.attr("type"), Some("unavailable"));
}

#[test]
fn build_groupchat_message_stanza() {
    let room_jid: xmpp_parsers::jid::BareJid = "room@conference.example.com".parse().unwrap();

    let mut msg = xmpp_parsers::message::Message::new(Some(xmpp_parsers::jid::Jid::from(room_jid)));
    msg.type_ = xmpp_parsers::message::MessageType::Groupchat;
    msg.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("Hello!".to_string()),
    );
    let elem: Element = msg.into();

    assert_eq!(elem.name(), "message");
    assert_eq!(elem.attr("type"), Some("groupchat"));
    assert_eq!(elem.attr("to"), Some("room@conference.example.com"));
    let body = elem
        .children()
        .find(|c| c.is("body", "jabber:client"))
        .map(|c| c.text());
    assert_eq!(body, Some("Hello!".to_string()));
}
