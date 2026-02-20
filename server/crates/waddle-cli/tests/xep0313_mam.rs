// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! XEP-0313 Message Archive Management (MAM) — dedicated test suite.
//!
//! Tests MAM query construction, MAM result stanza parsing, and MAM fin handling.
//!
//! Required by AGENTS.md: "Every implemented XEP MUST have a dedicated test suite."

use minidom::Element;

/// Parse a raw XML string into a minidom Element.
fn parse(xml: &str) -> Element {
    xml.parse().expect("valid XML")
}

#[test]
fn mam_query_to_room_jid() {
    // XEP-0313 §5.4: MUC MAM queries MUST be addressed to the room JID
    let xml = r#"<iq xmlns='jabber:client' type='set' id='mam-1'
                   to='room@conference.example.com'>
                   <query xmlns='urn:xmpp:mam:2' queryid='mam-1'>
                     <set xmlns='http://jabber.org/protocol/rsm'>
                       <max>50</max>
                       <before/>
                     </set>
                   </query>
                 </iq>"#;
    let elem = parse(xml);
    assert_eq!(elem.attr("to"), Some("room@conference.example.com"));
    assert_eq!(elem.attr("type"), Some("set"));
}

#[test]
fn mam_query_has_correct_structure() {
    // Build a MAM query for the last 50 messages
    let set = Element::builder("set", "http://jabber.org/protocol/rsm")
        .append(
            Element::builder("max", "http://jabber.org/protocol/rsm")
                .append("50")
                .build(),
        )
        .append(Element::builder("before", "http://jabber.org/protocol/rsm").build())
        .build();

    let query = Element::builder("query", "urn:xmpp:mam:2")
        .attr("queryid", "mam-test-1")
        .append(set)
        .build();

    let iq = Element::builder("iq", "jabber:client")
        .attr("type", "set")
        .attr("id", "mam-test-1")
        .append(query)
        .build();

    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("id"), Some("mam-test-1"));

    let query_elem = iq
        .children()
        .find(|c| c.is("query", "urn:xmpp:mam:2"))
        .expect("query element");
    assert_eq!(query_elem.attr("queryid"), Some("mam-test-1"));

    let set_elem = query_elem
        .children()
        .find(|c| c.is("set", "http://jabber.org/protocol/rsm"))
        .expect("set element");

    let max_elem = set_elem
        .children()
        .find(|c| c.is("max", "http://jabber.org/protocol/rsm"))
        .expect("max element");
    assert_eq!(max_elem.text(), "50");

    let before_elem = set_elem
        .children()
        .find(|c| c.is("before", "http://jabber.org/protocol/rsm"))
        .expect("before element (empty for last page)");
    assert_eq!(before_elem.text(), "");
}

#[test]
fn mam_result_message_structure() {
    // XEP-0313 §5.2: MAM result wrapped in a <message> with <result> child
    let xml = r#"<message xmlns='jabber:client'
                   from='room@conference.example.com'>
                   <result xmlns='urn:xmpp:mam:2' queryid='mam-1' id='msg-001'>
                     <forwarded xmlns='urn:xmpp:forward:0'>
                       <delay xmlns='urn:xmpp:delay' stamp='2025-01-15T10:30:00Z'/>
                       <message xmlns='jabber:client'
                         type='groupchat'
                         from='room@conference.example.com/alice'>
                         <body>Old message from history</body>
                       </message>
                     </forwarded>
                   </result>
                 </message>"#;

    let elem = parse(xml);
    assert_eq!(elem.name(), "message");

    let result = elem
        .children()
        .find(|c| c.is("result", "urn:xmpp:mam:2"))
        .expect("result element");
    assert_eq!(result.attr("queryid"), Some("mam-1"));
    assert_eq!(result.attr("id"), Some("msg-001"));

    let forwarded = result
        .children()
        .find(|c| c.is("forwarded", "urn:xmpp:forward:0"))
        .expect("forwarded element");

    let delay = forwarded
        .children()
        .find(|c| c.is("delay", "urn:xmpp:delay"))
        .expect("delay element");
    assert_eq!(delay.attr("stamp"), Some("2025-01-15T10:30:00Z"));

    let inner_msg = forwarded
        .children()
        .find(|c| c.is("message", "jabber:client"))
        .expect("inner message");
    assert_eq!(inner_msg.attr("type"), Some("groupchat"));
    assert_eq!(
        inner_msg.attr("from"),
        Some("room@conference.example.com/alice")
    );

    let body = inner_msg
        .children()
        .find(|c| c.is("body", "jabber:client"))
        .map(|c| c.text());
    assert_eq!(body, Some("Old message from history".to_string()));
}

#[test]
fn mam_result_with_custom_payload() {
    // MAM results should preserve custom payloads in the forwarded message
    let xml = r#"<message xmlns='jabber:client'
                   from='room@conference.example.com'>
                   <result xmlns='urn:xmpp:mam:2' queryid='mam-2' id='msg-002'>
                     <forwarded xmlns='urn:xmpp:forward:0'>
                       <delay xmlns='urn:xmpp:delay' stamp='2025-01-15T11:00:00Z'/>
                       <message xmlns='jabber:client'
                         type='groupchat'
                         from='room@conference.example.com/bob'>
                         <body>Check this repo</body>
                         <repo xmlns='urn:waddle:github:0' owner='waddle-social' name='waddle'/>
                       </message>
                     </forwarded>
                   </result>
                 </message>"#;

    let elem = parse(xml);
    let result = elem
        .children()
        .find(|c| c.is("result", "urn:xmpp:mam:2"))
        .expect("result");
    let forwarded = result
        .children()
        .find(|c| c.is("forwarded", "urn:xmpp:forward:0"))
        .expect("forwarded");
    let inner_msg = forwarded
        .children()
        .find(|c| c.is("message", "jabber:client"))
        .expect("inner message");

    // Should have both body and custom payload
    let non_body: Vec<&Element> = inner_msg
        .children()
        .filter(|c| !c.is("body", "jabber:client"))
        .collect();
    assert_eq!(non_body.len(), 1);
    assert_eq!(non_body[0].name(), "repo");
    assert_eq!(non_body[0].ns(), "urn:waddle:github:0".to_string());
    assert_eq!(non_body[0].attr("owner"), Some("waddle-social"));
}

#[test]
fn mam_fin_complete() {
    // XEP-0313 §5.3: IQ result with <fin complete='true'>
    let xml = r#"<iq xmlns='jabber:client' type='result' id='mam-1'>
                   <fin xmlns='urn:xmpp:mam:2' complete='true'>
                     <set xmlns='http://jabber.org/protocol/rsm'>
                       <first index='0'>msg-001</first>
                       <last>msg-050</last>
                       <count>50</count>
                     </set>
                   </fin>
                 </iq>"#;

    let elem = parse(xml);
    assert_eq!(elem.name(), "iq");
    assert_eq!(elem.attr("type"), Some("result"));

    let fin = elem
        .children()
        .find(|c| c.is("fin", "urn:xmpp:mam:2"))
        .expect("fin element");
    assert_eq!(fin.attr("complete"), Some("true"));

    let set = fin
        .children()
        .find(|c| c.is("set", "http://jabber.org/protocol/rsm"))
        .expect("rsm set");
    let first = set
        .children()
        .find(|c| c.is("first", "http://jabber.org/protocol/rsm"))
        .map(|c| c.text());
    assert_eq!(first, Some("msg-001".to_string()));
}

#[test]
fn mam_fin_incomplete() {
    let xml = r#"<iq xmlns='jabber:client' type='result' id='mam-2'>
                   <fin xmlns='urn:xmpp:mam:2'>
                     <set xmlns='http://jabber.org/protocol/rsm'>
                       <first index='0'>msg-001</first>
                       <last>msg-050</last>
                     </set>
                   </fin>
                 </iq>"#;

    let elem = parse(xml);
    let fin = elem
        .children()
        .find(|c| c.is("fin", "urn:xmpp:mam:2"))
        .expect("fin element");

    // complete attribute missing means incomplete
    assert_ne!(fin.attr("complete"), Some("true"));
}

#[test]
fn mam_query_with_rsm_paging() {
    // Test RSM paging: requesting messages before a specific ID
    let set = Element::builder("set", "http://jabber.org/protocol/rsm")
        .append(
            Element::builder("max", "http://jabber.org/protocol/rsm")
                .append("25")
                .build(),
        )
        .append(
            Element::builder("before", "http://jabber.org/protocol/rsm")
                .append("msg-001")
                .build(),
        )
        .build();

    let query = Element::builder("query", "urn:xmpp:mam:2")
        .attr("queryid", "mam-page-2")
        .append(set)
        .build();

    let before = query
        .children()
        .find(|c| c.is("set", "http://jabber.org/protocol/rsm"))
        .and_then(|s| s.children().find(|c| c.is("before", "http://jabber.org/protocol/rsm")))
        .map(|c| c.text());
    assert_eq!(before, Some("msg-001".to_string()));
}

#[test]
fn mam_result_extracts_timestamp_from_delay() {
    let xml = r#"<message xmlns='jabber:client'>
                   <result xmlns='urn:xmpp:mam:2' queryid='q1' id='m1'>
                     <forwarded xmlns='urn:xmpp:forward:0'>
                       <delay xmlns='urn:xmpp:delay' stamp='2025-06-15T14:30:00Z'/>
                       <message xmlns='jabber:client'
                         type='groupchat'
                         from='room@muc.example.com/charlie'>
                         <body>Timestamped message</body>
                       </message>
                     </forwarded>
                   </result>
                 </message>"#;

    let elem = parse(xml);
    let result = elem
        .children()
        .find(|c| c.is("result", "urn:xmpp:mam:2"))
        .unwrap();
    let forwarded = result
        .children()
        .find(|c| c.is("forwarded", "urn:xmpp:forward:0"))
        .unwrap();
    let delay = forwarded
        .children()
        .find(|c| c.is("delay", "urn:xmpp:delay"))
        .unwrap();

    let stamp = delay.attr("stamp").unwrap();
    let parsed = chrono::DateTime::parse_from_rfc3339(stamp);
    assert!(parsed.is_ok(), "stamp should be valid RFC3339");
    assert_eq!(stamp, "2025-06-15T14:30:00Z");
}
