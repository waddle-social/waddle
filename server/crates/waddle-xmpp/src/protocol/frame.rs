//! Parsed XMPP frames at the transport boundary.
//!
//! Replaces the string-based `starts_with`/`contains` routing in the
//! existing WebSocket handler with a single typed classifier. Frames are
//! parsed once; everything downstream works with typed Rust values.
//!
//! [`parse_frame`] is the entry point. It accepts the raw text of one RFC
//! 7395 WebSocket payload and returns a typed [`InboundFrame`], or a
//! [`ParseError`] if the frame is malformed. The same function is intended
//! for the TCP transport: once the XML stream chunker has assembled a
//! complete top-level element, `parse_frame` classifies it.

use crate::connection::Stanza;
use std::str::FromStr;
use thiserror::Error;
use xmpp_parsers::minidom::Element;

/// A fully parsed inbound XMPP frame, ready for typed dispatch.
///
/// Covers the RFC 7395 framing layer plus SASL negotiation frames. IQ-bind
/// is deliberately *not* a separate variant — it is an `<iq>` that carries
/// `urn:ietf:params:xml:ns:xmpp-bind` in its payload and is dispatched by
/// namespace via [`super::dispatch::StanzaDispatcher`].
#[derive(Debug, Clone)]
pub enum InboundFrame {
    /// RFC 7395 `<open>` stream header.
    Open,
    /// RFC 7395 `<close>` stream termination.
    Close,
    /// SASL `<auth>` initial client message.
    ///
    /// `mechanism` is the SASL mechanism requested (e.g. `SCRAM-SHA-256`,
    /// `OAUTHBEARER`, `PLAIN`). `data` is the base64-encoded initial
    /// response payload; empty when the mechanism doesn't need one.
    Auth { mechanism: String, data: String },
    /// SASL `<response>` continuation message (e.g. SCRAM
    /// client-final-message). The body is the base64-encoded payload.
    SaslResponse(String),
    /// A typed XMPP stanza (IQ, message, or presence).
    ///
    /// Uses [`crate::connection::Stanza`] for uniformity with the TCP
    /// handler. Boxed because `Stanza` is ~300 bytes and the other
    /// variants are unit — without the box every `InboundFrame` pays
    /// that cost on every move (clippy `large_enum_variant`).
    Stanza(Box<Stanza>),
}

/// Reasons a raw WebSocket payload can fail to classify into an
/// [`InboundFrame`].
///
/// `parse_frame` is expected to be fallible: clients sometimes send
/// malformed XML, unknown root elements (e.g. a client experimenting with a
/// non-standard XEP), or SASL frames missing required attributes. The
/// transport adapter decides how to react (typically: log and drop for
/// WebSocket; send a stream error for TCP).
#[derive(Debug, Error)]
pub enum ParseError {
    /// The payload was empty or contained only whitespace.
    #[error("frame is empty")]
    Empty,
    /// The payload was not well-formed XML.
    #[error("invalid XML: {0}")]
    InvalidXml(String),
    /// The payload's root element is not one of the five frames we know
    /// how to handle (`open`, `close`, `auth`, `response`, `iq`, `message`,
    /// `presence`).
    #[error("unknown root element: <{0}>")]
    UnknownRoot(String),
    /// A SASL frame was recognised but was missing a required attribute or
    /// was otherwise malformed.
    #[error("malformed SASL frame: {0}")]
    MalformedSasl(&'static str),
    /// The payload parsed as XML but `xmpp_parsers` could not convert it
    /// into the expected stanza type.
    #[error("invalid {kind} stanza: {err}")]
    InvalidStanza { kind: &'static str, err: String },
}

/// Classify a raw inbound frame into a typed [`InboundFrame`].
///
/// Called once per WebSocket text payload (or per TCP top-level element).
/// Fast-paths trivial frames (`<open>`/`<close>`) by peeking at the root
/// element name without invoking the XML parser; all non-trivial frames
/// then go through `xmpp_parsers::minidom::Element::from_str` for strict
/// parsing.
///
/// Stanzas (`<iq>`, `<message>`, `<presence>`) may arrive with the
/// stream-level `xmlns="jabber:client"` omitted, because the WebSocket
/// client inherited it from the stream `<open>`. This function injects it
/// when missing so `xmpp_parsers` can convert the element into a typed
/// stanza.
pub fn parse_frame(frame: &str) -> Result<InboundFrame, ParseError> {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }

    let root = peek_root_name(trimmed)
        .ok_or_else(|| ParseError::InvalidXml("missing root element".to_string()))?;

    match root {
        "open" => Ok(InboundFrame::Open),
        "close" => Ok(InboundFrame::Close),
        "auth" => parse_auth(trimmed),
        "response" => parse_response(trimmed),
        "iq" | "message" | "presence" => parse_stanza(trimmed, root),
        other => Err(ParseError::UnknownRoot(other.to_string())),
    }
}

/// Sniff the root element's tag name without invoking a full XML parse.
///
/// Returns `None` if the string does not start with `<` or has no
/// recognisable tag name. This is a classifier only — it does not validate
/// XML.
fn peek_root_name(xml: &str) -> Option<&str> {
    let rest = xml.strip_prefix('<')?;
    // Skip XML comments/declarations we don't care about.
    if rest.starts_with('?') || rest.starts_with('!') {
        return None;
    }
    let name_end = rest
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    if name_end == 0 {
        return None;
    }
    Some(&rest[..name_end])
}

fn parse_auth(frame: &str) -> Result<InboundFrame, ParseError> {
    let element =
        Element::from_str(frame).map_err(|err| ParseError::InvalidXml(err.to_string()))?;
    let mechanism = element
        .attr("mechanism")
        .ok_or(ParseError::MalformedSasl("missing mechanism attribute"))?
        .to_string();
    Ok(InboundFrame::Auth {
        mechanism,
        data: element.text().trim().to_string(),
    })
}

fn parse_response(frame: &str) -> Result<InboundFrame, ParseError> {
    let element =
        Element::from_str(frame).map_err(|err| ParseError::InvalidXml(err.to_string()))?;
    Ok(InboundFrame::SaslResponse(
        element.text().trim().to_string(),
    ))
}

fn parse_stanza(frame: &str, kind: &str) -> Result<InboundFrame, ParseError> {
    let patched = inject_default_ns(frame, "jabber:client");
    let element =
        Element::from_str(&patched).map_err(|err| ParseError::InvalidXml(err.to_string()))?;

    let stanza = match kind {
        "iq" => Stanza::Iq(xmpp_parsers::iq::Iq::try_from(element).map_err(|err| {
            ParseError::InvalidStanza {
                kind: "iq",
                err: format!("{err:?}"),
            }
        })?),
        "message" => Stanza::Message(xmpp_parsers::message::Message::try_from(element).map_err(
            |err| ParseError::InvalidStanza {
                kind: "message",
                err: format!("{err:?}"),
            },
        )?),
        "presence" => {
            Stanza::Presence(xmpp_parsers::presence::Presence::try_from(element).map_err(
                |err| ParseError::InvalidStanza {
                    kind: "presence",
                    err: format!("{err:?}"),
                },
            )?)
        }
        // Only reachable values are checked by the dispatcher above.
        _ => {
            return Err(ParseError::UnknownRoot(kind.to_string()));
        }
    };
    Ok(InboundFrame::Stanza(Box::new(stanza)))
}

/// Insert `xmlns="{ns}"` into the opening tag of a stanza when absent.
///
/// WebSocket clients frequently omit the default namespace on stanzas,
/// relying on the stream-level `<open>` to scope them. Since we parse each
/// frame in isolation, we reintroduce the namespace here so that
/// `xmpp_parsers` accepts the element.
fn inject_default_ns(xml: &str, ns: &str) -> String {
    let open_end = match xml.find('>') {
        Some(idx) => idx,
        None => return xml.to_string(),
    };
    let open_tag = &xml[..open_end];
    if open_tag.contains("xmlns=") {
        return xml.to_string();
    }
    let tag_end = xml[1..]
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .map(|idx| idx + 1)
        .unwrap_or(open_end);
    format!(r#"{} xmlns="{ns}"{}"#, &xml[..tag_end], &xml[tag_end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_frame() {
        let xml = r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" version="1.0" to="waddle.social"/>"#;
        assert!(matches!(parse_frame(xml), Ok(InboundFrame::Open)));
    }

    #[test]
    fn parses_close_frame() {
        let xml = r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#;
        assert!(matches!(parse_frame(xml), Ok(InboundFrame::Close)));
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
    fn parses_oauthbearer_auth_frame() {
        let xml = r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl" mechanism="OAUTHBEARER">biwsdG9rZW49YWJjMTIz</auth>"#;
        match parse_frame(xml).expect("oauthbearer should parse") {
            InboundFrame::Auth { mechanism, .. } => assert_eq!(mechanism, "OAUTHBEARER"),
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn parses_sasl_response_frame() {
        let xml =
            r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">Yz1iaXdzLHI9...</response>"#;
        match parse_frame(xml).expect("response should parse") {
            InboundFrame::SaslResponse(data) => assert_eq!(data, "Yz1iaXdzLHI9..."),
            other => panic!("expected SaslResponse, got {other:?}"),
        }
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
            Stanza::Iq(iq) => assert_eq!(iq.id, "ping-1"),
            other => panic!("expected Iq, got {other:?}"),
        }
    }

    #[test]
    fn parses_iq_with_explicit_xmlns() {
        let xml = r#"<iq xmlns="jabber:client" type="get" id="ping-2"><ping xmlns="urn:xmpp:ping"/></iq>"#;
        let frame = parse_frame(xml).expect("iq should parse");
        let InboundFrame::Stanza(stanza) = frame else {
            panic!("expected Stanza");
        };
        assert!(matches!(*stanza, Stanza::Iq(_)));
    }

    #[test]
    fn parses_message_stanza() {
        let xml =
            r#"<message type="chat" to="bob@waddle.social" id="m-1"><body>hi</body></message>"#;
        let frame = parse_frame(xml).expect("message should parse");
        let InboundFrame::Stanza(stanza) = frame else {
            panic!("expected Stanza");
        };
        assert!(matches!(*stanza, Stanza::Message(_)));
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
        let xml = "\n   <close xmlns=\"urn:ietf:params:xml:ns:xmpp-framing\"/>\n  ";
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
        match parse_frame("<iq type=\"get\" id=\"broken\"") {
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
        assert!(peek_root_name("<?xml version=\"1.0\"?>").is_none());
    }

    #[test]
    fn inject_default_ns_is_noop_when_xmlns_present() {
        let input = r#"<iq xmlns="jabber:client" id="x"/>"#;
        assert_eq!(inject_default_ns(input, "jabber:client"), input);
    }

    #[test]
    fn inject_default_ns_adds_attr_after_tag_name() {
        let input = r#"<iq id="x"/>"#;
        assert_eq!(
            inject_default_ns(input, "jabber:client"),
            r#"<iq xmlns="jabber:client" id="x"/>"#
        );
    }
}
