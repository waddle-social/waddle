use super::*;

#[test]
fn test_stream_header_parsing() {
    let header_xml = r#"<?xml version='1.0'?><stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' to='example.com' from='user@example.com' version='1.0'>"#;

    let header = StreamHeader::parse(header_xml).unwrap();

    assert_eq!(header.to, Some("example.com".to_string()));
    assert_eq!(header.from, Some("user@example.com".to_string()));
    assert_eq!(header.version, Some("1.0".to_string()));
}

#[test]
fn test_stream_header_with_double_quotes() {
    let header_xml = r#"<stream:stream xmlns="jabber:client" to="localhost" version="1.0">"#;

    let header = StreamHeader::parse(header_xml).unwrap();

    assert_eq!(header.to, Some("localhost".to_string()));
    assert_eq!(header.version, Some("1.0".to_string()));
}

#[test]
fn test_parser_auth() {
    let mut parser = XmlParser::new();
    parser.feed(b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>AGFsaWNlAHNlY3JldA==</auth>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    if let Some(ParsedStanza::SaslAuth { mechanism, data }) = stanza {
        assert_eq!(mechanism, "PLAIN");
        assert_eq!(data, "AGFsaWNlAHNlY3JldA==");
    } else {
        panic!("Expected SaslAuth");
    }
}

#[test]
fn test_parser_message() {
    let mut parser = XmlParser::new();
    // Include xmlns='jabber:client' as minidom requires namespace declarations
    parser.feed(b"<message xmlns='jabber:client' to='bob@example.com' type='chat'><body>Hello!</body></message>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    assert!(matches!(stanza, Some(ParsedStanza::Message(_))));
}

#[test]
fn test_parser_iq() {
    let mut parser = XmlParser::new();
    // Include xmlns='jabber:client' as minidom requires namespace declarations
    parser.feed(b"<iq xmlns='jabber:client' type='get' id='bind_1'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/></iq>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    assert!(matches!(stanza, Some(ParsedStanza::Iq(_))));
}

#[test]
fn test_parser_iq_without_namespace_declaration() {
    let mut parser = XmlParser::new();
    parser
        .feed(b"<iq type='set' id='bind_1'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/></iq>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    match stanza {
        Some(ParsedStanza::Iq(element)) => {
            assert_eq!(element.name(), "iq");
            assert_eq!(element.ns(), ns::JABBER_CLIENT);
        }
        _ => panic!("Expected IQ stanza"),
    }
}

#[test]
fn test_parser_stream_end() {
    let mut parser = XmlParser::new();
    parser.feed(b"</stream:stream>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    assert!(matches!(stanza, Some(ParsedStanza::StreamEnd)));
}

#[test]
fn test_element_to_string_roundtrip() {
    let xml = "<message to='bob@example.com' type='chat' xmlns='jabber:client'><body>Hello!</body></message>";
    let element = parse_element(xml).unwrap();
    let output = element_to_string(&element).unwrap();

    // Parse again to verify
    let element2 = parse_element(&output).unwrap();
    assert_eq!(element.name(), element2.name());
    assert_eq!(element.attr("to"), element2.attr("to"));
}

// XEP-0198 Stream Management parsing tests

#[test]
fn test_parser_sm_enable() {
    let mut parser = XmlParser::new();
    parser.feed(b"<enable xmlns='urn:xmpp:sm:3'/>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    if let Some(ParsedStanza::SmEnable { resume, max }) = stanza {
        assert!(!resume);
        assert!(max.is_none());
    } else {
        panic!("Expected SmEnable");
    }
}

#[test]
fn test_parser_sm_enable_with_resume() {
    let mut parser = XmlParser::new();
    parser.feed(b"<enable xmlns='urn:xmpp:sm:3' resume='true' max='300'/>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    if let Some(ParsedStanza::SmEnable { resume, max }) = stanza {
        assert!(resume);
        assert_eq!(max, Some(300));
    } else {
        panic!("Expected SmEnable with resume");
    }
}

#[test]
fn test_parser_sm_enable_accepts_xs_boolean_canonical_true() {
    let mut parser = XmlParser::new();
    parser.feed(b"<enable xmlns='urn:xmpp:sm:3' resume='1' max='300'/>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    if let Some(ParsedStanza::SmEnable { resume, max }) = stanza {
        assert!(resume);
        assert_eq!(max, Some(300));
    } else {
        panic!("Expected SmEnable with canonical xs:boolean true");
    }
}

#[test]
fn test_parser_sm_request() {
    let mut parser = XmlParser::new();
    parser.feed(b"<r xmlns='urn:xmpp:sm:3'/>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    assert!(matches!(stanza, Some(ParsedStanza::SmRequest)));
}

#[test]
fn test_parser_sm_ack() {
    let mut parser = XmlParser::new();
    parser.feed(b"<a xmlns='urn:xmpp:sm:3' h='5'/>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    if let Some(ParsedStanza::SmAck { h }) = stanza {
        assert_eq!(h, 5);
    } else {
        panic!("Expected SmAck");
    }
}

#[test]
fn test_parser_sm_resume() {
    let mut parser = XmlParser::new();
    parser.feed(b"<resume xmlns='urn:xmpp:sm:3' previd='stream-123' h='10'/>");

    assert!(parser.has_complete_stanza());

    let stanza = parser.next_stanza().unwrap();
    if let Some(ParsedStanza::SmResume { previd, h }) = stanza {
        assert_eq!(previd, "stream-123");
        assert_eq!(h, 10);
    } else {
        panic!("Expected SmResume");
    }
}

#[test]
fn test_parser_sm_request_rejects_bare_nonza_without_namespace() {
    let mut parser = XmlParser::new();
    parser.feed(b"<r/>");

    assert!(parser.has_complete_stanza());
    assert!(
        parser.next_stanza().is_err(),
        "bare <r/> must not be treated as XEP-0198 without SM namespace"
    );
}

#[test]
fn test_parser_presence_then_iq_in_order() {
    let mut parser = XmlParser::new();
    parser.feed(
        b"<presence xmlns='jabber:client' from='alice@example.com' type='available'/>\
          <iq xmlns='jabber:client' type='get' id='q1'><query xmlns='jabber:iq:roster'/></iq>",
    );

    // First call must return the presence (earliest in buffer)
    let first = parser.next_stanza().unwrap();
    match first {
        Some(ParsedStanza::Presence(ref el)) => {
            assert_eq!(el.name(), "presence");
            assert_eq!(el.attr("from"), Some("alice@example.com"));
        }
        other => panic!("Expected Presence, got {:?}", other),
    }

    // Second call must return the iq
    let second = parser.next_stanza().unwrap();
    match second {
        Some(ParsedStanza::Iq(ref el)) => {
            assert_eq!(el.name(), "iq");
            assert_eq!(el.attr("id"), Some("q1"));
        }
        other => panic!("Expected Iq, got {:?}", other),
    }

    // Buffer should now be empty
    assert!(parser.next_stanza().unwrap().is_none());
}

#[test]
fn test_parser_earliest_position_wins_over_pattern_order() {
    // iq appears before presence in the pattern list, but presence
    // appears first in the buffer - presence must be returned first.
    let mut parser = XmlParser::new();
    parser.feed(
        b"<presence xmlns='jabber:client' type='unavailable'/>\
          <iq xmlns='jabber:client' type='result' id='x1'/>",
    );

    let first = parser.next_stanza().unwrap();
    assert!(
        matches!(first, Some(ParsedStanza::Presence(_))),
        "Expected Presence first, got {:?}",
        first
    );

    let second = parser.next_stanza().unwrap();
    assert!(
        matches!(second, Some(ParsedStanza::Iq(_))),
        "Expected Iq second, got {:?}",
        second
    );
}
