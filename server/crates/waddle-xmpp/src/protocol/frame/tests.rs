use super::*;

#[test]
fn parses_open_frame() {
    let xml =
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" version="1.0" to="waddle.social"/>"#;
    assert!(matches!(parse_frame(xml), Ok(InboundFrame::Open)));
}

#[test]
fn rejects_open_frame_with_wrong_namespace() {
    let xml = r#"<open xmlns="jabber:client" version="1.0"/>"#;
    assert!(matches!(parse_frame(xml), Err(ParseError::InvalidXml(_))));
}

#[test]
fn rejects_truncated_open_frame() {
    assert!(matches!(
        parse_frame("<open"),
        Err(ParseError::InvalidXml(_))
    ));
}

#[test]
fn rejects_open_frame_with_payload() {
    let xml = r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing"><iq/></open>"#;
    assert!(matches!(parse_frame(xml), Err(ParseError::InvalidXml(_))));
}

#[test]
fn rejects_open_frame_with_whitespace_payload() {
    let xml = "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing'>\n</open>";
    assert!(matches!(parse_frame(xml), Err(ParseError::InvalidXml(_))));
}

#[test]
fn parses_close_frame() {
    let xml = r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#;
    assert!(matches!(parse_frame(xml), Ok(InboundFrame::Close)));
}

#[test]
fn rejects_close_frame_with_payload() {
    let xml = r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing">bye</close>"#;
    assert!(matches!(parse_frame(xml), Err(ParseError::InvalidXml(_))));
}

#[test]
fn rejects_close_frame_with_whitespace_payload() {
    let xml = "<close xmlns='urn:ietf:params:xml:ns:xmpp-framing'> </close>";
    assert!(matches!(parse_frame(xml), Err(ParseError::InvalidXml(_))));
}

#[test]
fn parses_scram_auth_frame() {
    let xml = r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl" mechanism="SCRAM-SHA-256">bixhPWFsaWNlLHI9cmFuZG9t</auth>"#;
    match parse_frame(xml).expect("auth should parse") {
        InboundFrame::Auth { mechanism, data } => {
            assert_eq!(mechanism, "SCRAM-SHA-256");
            assert_eq!(data, "bixhPWFsaWNlLHI9cmFuZG9t");
        }
        other => panic!("expected Auth, got {other:?}"),
    }
}

#[test]
fn rejects_auth_frame_with_wrong_namespace() {
    let xml = r#"<auth xmlns="jabber:client" mechanism="SCRAM-SHA-256">x</auth>"#;
    assert!(matches!(parse_frame(xml), Err(ParseError::InvalidXml(_))));
}

#[test]
fn parses_oauthbearer_auth_frame() {
    let xml = r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl" mechanism="OAUTHBEARER">biwsdG9rZW49YWJjMTIz</auth>"#;
    match parse_frame(xml).expect("oauthbearer should parse") {
        InboundFrame::Auth { mechanism, .. } => assert_eq!(mechanism, "OAUTHBEARER"),
        other => panic!("expected Auth, got {other:?}"),
    }
}

#[test]
fn rejects_auth_frame_with_child_payload() {
    let xml = r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl" mechanism="SCRAM-SHA-256">YWJj<foo/>ZA==</auth>"#;
    assert!(matches!(parse_frame(xml), Err(ParseError::InvalidXml(_))));
}

#[test]
fn parses_sasl_response_frame() {
    let xml = r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">Yz1iaXdzLHI9...</response>"#;
    match parse_frame(xml).expect("response should parse") {
        InboundFrame::SaslResponse(data) => assert_eq!(data, "Yz1iaXdzLHI9..."),
        other => panic!("expected SaslResponse, got {other:?}"),
    }
}

#[test]
fn rejects_sasl_response_with_child_payload() {
    let xml = r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">Yz1i<foo/>aXdz</response>"#;
    assert!(matches!(parse_frame(xml), Err(ParseError::InvalidXml(_))));
}

#[test]
fn parses_iq_without_xmlns() {
    // WebSocket clients omit xmlns on stanzas and rely on stream
    // inheritance. parse_frame must reintroduce it.
    let xml = r#"<iq type="get" id="ping-1"><ping xmlns="urn:xmpp:ping"/></iq>"#;
    let frame = parse_frame(xml).expect("iq should parse");
    let InboundFrame::Stanza(stanza) = frame else {
        panic!("expected Stanza, got {frame:?}");
    };
    match *stanza {
        Stanza::Iq(iq) => assert_eq!(iq.id(), "ping-1"),
        other => panic!("expected Iq, got {other:?}"),
    }
}

#[test]
fn parses_iq_with_explicit_xmlns() {
    let xml =
        r#"<iq xmlns="jabber:client" type="get" id="ping-2"><ping xmlns="urn:xmpp:ping"/></iq>"#;
    let frame = parse_frame(xml).expect("iq should parse");
    let InboundFrame::Stanza(stanza) = frame else {
        panic!("expected Stanza");
    };
    assert!(matches!(*stanza, Stanza::Iq(_)));
}

#[test]
fn parses_message_stanza() {
    let xml = r#"<message type="chat" to="bob@waddle.social" id="m-1"><body>hi</body></message>"#;
    let frame = parse_frame(xml).expect("message should parse");
    let InboundFrame::Stanza(stanza) = frame else {
        panic!("expected Stanza");
    };
    assert!(matches!(*stanza, Stanza::Message(_)));
}

#[test]
fn nested_message_element_does_not_truncate_the_outer_stanza() {
    let xml = r#"<message type="chat" to="bob@waddle.social" id="outer">
        <body>outer body</body>
        <forwarded xmlns="urn:xmpp:forward:0">
            <message xmlns="jabber:client" type="chat" id="inner">
                <body>inner body</body>
            </message>
        </forwarded>
    </message>"#;

    let frame = parse_frame(xml).expect("outer message should parse as one complete frame");
    let InboundFrame::Stanza(stanza) = frame else {
        panic!("expected Stanza");
    };
    let Stanza::Message(message) = *stanza else {
        panic!("expected Message stanza");
    };

    assert_eq!(
        message.bodies.values().next().map(String::as_str),
        Some("outer body")
    );
    let forwarded = message
        .payloads
        .iter()
        .find(|payload| payload.is("forwarded", "urn:xmpp:forward:0"))
        .expect("forwarded payload should survive typed parsing");
    let nested = forwarded
        .get_child("message", CLIENT_STANZA_NS)
        .expect("nested message should remain inside the forwarded payload");
    assert_eq!(nested.attr("id"), Some("inner"));
    assert_eq!(
        nested
            .get_child("body", CLIENT_STANZA_NS)
            .map(Element::text)
            .as_deref(),
        Some("inner body")
    );
}

#[test]
fn xep_0201_message_thread_parent_survives_inbound_parse() {
    // RFC 6121 / XEP-0201: `<thread parent='X'>id</thread>` is the
    // wire shape for nested threads. xmpp_parsers 0.21 silently drops
    // the `parent` attribute at typed parse, so the inbound boundary
    // calls `extract_thread_parent` + `reattach_thread_parent` to move
    // the typed thread element into `msg.payloads` (with parent
    // intact) and clear `msg.thread`. Downstream consumers read both
    // id and parent via `xep0201::thread_info_from_message`.
    let xml = r#"<message type="chat" to="bob@waddle.social" id="m-2"><body>hi</body><thread parent="root-1">child-2</thread></message>"#;
    let frame = parse_frame(xml).expect("message should parse");
    let InboundFrame::Stanza(stanza) = frame else {
        panic!("expected Stanza");
    };
    let Stanza::Message(msg) = *stanza else {
        panic!("expected Message stanza");
    };
    // Post-reattach invariant: typed field cleared, payload form set.
    assert!(msg.thread.is_none(), "typed thread field should be cleared");
    let info = waddle_xmpp_core::xep0201::thread_info_from_message(&msg)
        .expect("thread info recoverable from payload form");
    assert_eq!(info.id.as_str(), "child-2");
    assert_eq!(info.parent.as_ref().map(|t| t.as_str()), Some("root-1"));
}

#[test]
fn xep_0201_message_root_thread_parses_without_parent_payload() {
    // Root thread (no parent attribute). The typed Message::thread
    // field carries the id; reattach is skipped because there's no
    // parent to preserve. `thread_info_from_message` falls back to
    // the typed field.
    let xml = r#"<message type="chat" to="bob@waddle.social" id="m-3"><body>hi</body><thread>root-only</thread></message>"#;
    let frame = parse_frame(xml).expect("message should parse");
    let InboundFrame::Stanza(stanza) = frame else {
        panic!("expected Stanza");
    };
    let Stanza::Message(msg) = *stanza else {
        panic!("expected Message stanza");
    };
    let info =
        waddle_xmpp_core::xep0201::thread_info_from_message(&msg).expect("thread info recoverable");
    assert_eq!(info.id.as_str(), "root-only");
    assert_eq!(info.parent, None);
}

#[test]
fn parses_presence_stanza() {
    let xml = r#"<presence to="room@muc.waddle.social/alice"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#;
    let frame = parse_frame(xml).expect("presence should parse");
    let InboundFrame::Stanza(stanza) = frame else {
        panic!("expected Stanza");
    };
    assert!(matches!(*stanza, Stanza::Presence(_)));
}

#[test]
fn leading_and_trailing_whitespace_is_trimmed() {
    let xml = "\n   <close xmlns='urn:ietf:params:xml:ns:xmpp-framing'/>\n  ";
    assert!(matches!(parse_frame(xml), Ok(InboundFrame::Close)));
}

#[test]
fn rejects_empty_frame() {
    assert!(matches!(parse_frame(""), Err(ParseError::Empty)));
    assert!(matches!(parse_frame("   \n  "), Err(ParseError::Empty)));
}

#[test]
fn rejects_unknown_root_element() {
    match parse_frame("<wibble/>") {
        Err(ParseError::UnknownRoot(name)) => assert_eq!(name, "wibble"),
        other => panic!("expected UnknownRoot, got {other:?}"),
    }
}

#[test]
fn rejects_auth_without_mechanism_attribute() {
    let xml = r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl">data</auth>"#;
    assert!(matches!(
        parse_frame(xml),
        Err(ParseError::MalformedSasl(_))
    ));
}

#[test]
fn rejects_malformed_xml() {
    match parse_frame("<iq type='get' id='broken'") {
        Err(ParseError::InvalidXml(_)) => {}
        other => panic!("expected InvalidXml, got {other:?}"),
    }
}

#[test]
fn rejects_iq_with_garbage_payload() {
    // Payload element has a malformed IQ shape (no `type` attribute).
    let xml = r#"<iq id="x"><nope/></iq>"#;
    match parse_frame(xml) {
        Err(ParseError::InvalidStanza { kind, .. }) => assert_eq!(kind, "iq"),
        other => panic!("expected InvalidStanza, got {other:?}"),
    }
}

#[test]
fn peek_root_name_rejects_xml_declaration() {
    // We never expect a `<?xml ?>` header in a WebSocket frame; guard
    // against peek_root_name confusing it for a tag name.
    assert!(peek_root_name("<?xml version='1.0'?>").is_none());
}

#[test]
fn inject_default_ns_is_noop_when_xmlns_present() {
    let input = r#"<iq xmlns="jabber:client" id="x"/>"#;
    assert_eq!(inject_client_ns_if_missing(input), input);
}

#[test]
fn inject_default_ns_is_noop_when_xmlns_has_spaces_around_equals() {
    let input = r#"<iq xmlns = "jabber:client" id="x"/>"#;
    assert_eq!(inject_client_ns_if_missing(input), input);
}

#[test]
fn inject_default_ns_adds_attr_after_tag_name() {
    let input = r#"<iq id="x"/>"#;
    assert_eq!(
        inject_client_ns_if_missing(input),
        r#"<iq xmlns="jabber:client" id="x"/>"#
    );
}

#[test]
fn inject_default_ns_ignores_xmlns_like_attribute_values() {
    let input = r#"<iq id="x" data="xmlns=bogus"/>"#;
    assert_eq!(
        inject_client_ns_if_missing(input),
        r#"<iq xmlns="jabber:client" id="x" data="xmlns=bogus"/>"#
    );
}

#[test]
fn inject_default_ns_ignores_xmlns_suffix_attribute_names() {
    let input = r#"<iq data-xmlns="bogus" id="x"/>"#;
    assert_eq!(
        inject_client_ns_if_missing(input),
        r#"<iq xmlns="jabber:client" data-xmlns="bogus" id="x"/>"#
    );
}

#[test]
fn inject_default_ns_handles_unquoted_attr_with_slash_before_xmlns() {
    let input = r#"<iq bogus=http://x xmlns = "jabber:client" id="x"/>"#;
    assert_eq!(inject_client_ns_if_missing(input), input);
}

#[test]
fn inject_default_ns_handles_unquoted_attr_with_equals_and_slash_before_xmlns() {
    let input = r#"<iq bogus=http://x=y xmlns = "jabber:client" id="x"/>"#;
    assert_eq!(inject_client_ns_if_missing(input), input);
}

#[test]
fn inject_default_ns_handles_gt_inside_attribute_value() {
    let input = r#"<iq id="x" data="1 > 0"><ping xmlns="urn:xmpp:ping"/></iq>"#;
    let patched = inject_client_ns_if_missing(input);
    assert_eq!(
        patched,
        r#"<iq xmlns="jabber:client" id="x" data="1 > 0"><ping xmlns="urn:xmpp:ping"/></iq>"#
    );
    assert!(Element::from_str(&patched).is_ok());
}

#[test]
fn rejects_oversized_frame() {
    let huge = format!("<iq id='x'>{}</iq>", "a".repeat(MAX_FRAME_SIZE));
    assert!(matches!(parse_frame(&huge), Err(ParseError::TooLarge)));
}

#[test]
fn rejects_oversized_frame_with_whitespace_padding() {
    let huge = format!("{}<iq id='x'/>", " ".repeat(MAX_FRAME_SIZE));
    assert!(matches!(parse_frame(&huge), Err(ParseError::TooLarge)));
}

/// RFC 6121 §5.2.2: a message whose 'type' value is not understood
/// MUST be treated as type 'normal' — not rejected as a parse failure
/// (which silently dropped the stanza, #1266 item 6).
#[test]
fn rfc6121_unknown_message_type_normalizes_to_normal() {
    let frame = r#"<message to="bob@example.com" type="subscribe"><body>hi</body></message>"#;
    let parsed = parse_frame(frame).expect("unknown message type must parse");
    let InboundFrame::Stanza(stanza) = parsed else {
        panic!("expected stanza frame");
    };
    let Stanza::Message(message) = *stanza else {
        panic!("expected message stanza");
    };
    assert_eq!(message.type_, xmpp_parsers::message::MessageType::Normal);
    assert_eq!(
        message.bodies.values().next().map(String::as_str),
        Some("hi"),
        "payload survives normalization"
    );
}

/// Known types are untouched by the normalization.
#[test]
fn rfc6121_known_message_types_are_preserved() {
    for (wire, expected) in [
        ("chat", xmpp_parsers::message::MessageType::Chat),
        ("headline", xmpp_parsers::message::MessageType::Headline),
        ("error", xmpp_parsers::message::MessageType::Error),
    ] {
        let frame = format!(r#"<message to="bob@example.com" type="{wire}"/>"#);
        let parsed = parse_frame(&frame).expect("known message type must parse");
        let InboundFrame::Stanza(stanza) = parsed else {
            panic!("expected stanza frame");
        };
        let Stanza::Message(message) = *stanza else {
            panic!("expected message stanza");
        };
        assert_eq!(message.type_, expected);
    }
}
