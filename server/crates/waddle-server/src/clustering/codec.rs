//! Remote XML-text codec for the clustering transport (ADR-0017 element 3).
//!
//! Neither `xmpp-parsers` 0.22 nor `minidom` 0.18 implements serde, so every
//! remote payload crossing the kameo boundary carries its stanza as XML text
//! inside a serde-friendly newtype. Serialization goes through
//! `Stanza::to_element()` (which applies the `ensure_thread_element` fixup, so
//! RFC 6121 `<thread/>` survives the wire) and `element_to_string`.
//!
//! Deserialization is **bounded before tree construction**. (minidom 0.18's
//! own parse is an explicit-stack tree builder, so the parse itself cannot
//! overflow — but the recursion in nested `Element` `Drop`/`Clone` glue and
//! in `xmpp_parsers::*::try_from` is what the depth cap bounds, so the
//! pre-scan stays load-bearing; do not remove it because the parser looks
//! safe.) A non-recursive `quick-xml` pre-scan enforces a byte cap, a nesting
//! depth cap, and a per-element attribute/namespace-declaration cap first; a
//! payload failing any bound (or failing the re-parse) is a **typed error the
//! caller NACKs to the sender** — never a silent drop — and counts in the drop
//! metrics.

use super::metrics;
use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use waddle_xmpp::Stanza;

/// Byte cap on a single serialized stanza/element payload. Well below kameo's
/// request-size maximum so the codec bound, not the transport cap, is the
/// binding limit for one stanza.
pub const MAX_REMOTE_XML_BYTES: usize = 256 * 1024;

/// Nesting-depth cap enforced before minidom's recursive parse. Real XMPP
/// stanzas rarely exceed ~10 levels; 32 leaves generous headroom.
pub const MAX_REMOTE_XML_DEPTH: usize = 32;

/// Cap on attributes (including `xmlns` declarations) per element.
pub const MAX_REMOTE_XML_ATTRIBUTES: usize = 64;

/// Typed decode/encode failures at the remote codec boundary. The receiving
/// handler converts these into a NACK to the sender; they are never a silent
/// drop.
#[derive(Debug, thiserror::Error)]
pub enum RemoteCodecError {
    /// The stanza could not be serialized to XML text.
    #[error("remote codec failed to serialize stanza: {0}")]
    Serialize(String),
    /// The payload exceeds the byte cap.
    #[error("remote XML payload of {bytes} bytes exceeds the {max}-byte cap")]
    TooLarge { bytes: usize, max: usize },
    /// The payload nests deeper than the pre-parse depth cap.
    #[error("remote XML payload exceeds nesting depth cap of {max}")]
    TooDeep { max: usize },
    /// An element carries more attributes/namespace declarations than allowed.
    #[error("remote XML element exceeds the {max}-attribute cap")]
    TooManyAttributes { max: usize },
    /// The payload is not well-formed XML (pre-scan or minidom parse failed).
    #[error("remote XML payload failed to re-parse: {0}")]
    Malformed(String),
    /// The top-level element is not a message/presence/iq stanza.
    #[error("remote payload element '{name}' is not an XMPP stanza")]
    NotAStanza { name: String },
}

impl RemoteCodecError {
    /// Stable low-cardinality label for the drop metrics.
    fn reason(&self) -> metrics::RemoteCodecDropReason {
        match self {
            RemoteCodecError::Serialize(_) => metrics::RemoteCodecDropReason::Serialize,
            RemoteCodecError::TooLarge { .. } => metrics::RemoteCodecDropReason::TooLarge,
            RemoteCodecError::TooDeep { .. } => metrics::RemoteCodecDropReason::TooDeep,
            RemoteCodecError::TooManyAttributes { .. } => {
                metrics::RemoteCodecDropReason::TooManyAttributes
            }
            RemoteCodecError::Malformed(_) => metrics::RemoteCodecDropReason::Malformed,
            RemoteCodecError::NotAStanza { .. } => metrics::RemoteCodecDropReason::NotAStanza,
        }
    }
}

/// Encode a stanza as wire XML text (thread-preserving).
pub fn encode_stanza(stanza: &Stanza) -> Result<String, RemoteCodecError> {
    waddle_xmpp::parser::element_to_string(&stanza.to_element())
        .map_err(|error| RemoteCodecError::Serialize(error.to_string()))
}

/// Decode wire XML text into a typed stanza, enforcing all bounds first.
pub fn decode_stanza(xml: &str) -> Result<Stanza, RemoteCodecError> {
    let element = decode_element(xml)?;
    match element.name() {
        "message" => xmpp_parsers::message::Message::try_from(element)
            .map(Stanza::Message)
            .map_err(|error| RemoteCodecError::Malformed(error.to_string())),
        "presence" => xmpp_parsers::presence::Presence::try_from(element)
            .map(Stanza::Presence)
            .map_err(|error| RemoteCodecError::Malformed(error.to_string())),
        "iq" => xmpp_parsers::iq::Iq::try_from(element)
            .map(|iq| Stanza::Iq(Box::new(iq)))
            .map_err(|error| RemoteCodecError::Malformed(error.to_string())),
        other => Err(RemoteCodecError::NotAStanza {
            name: other.to_string(),
        }),
    }
}

/// Decode wire XML text into a minidom `Element`, enforcing the byte, depth,
/// and attribute caps with a non-recursive pre-scan **before** handing the
/// text to minidom's recursive parser.
pub fn decode_element(xml: &str) -> Result<minidom::Element, RemoteCodecError> {
    enforce_bounds(xml)?;
    xml.parse::<minidom::Element>()
        .map_err(|error: minidom::Error| RemoteCodecError::Malformed(error.to_string()))
}

/// Non-recursive bounds scan over the XML text.
fn enforce_bounds(xml: &str) -> Result<(), RemoteCodecError> {
    if xml.len() > MAX_REMOTE_XML_BYTES {
        return Err(RemoteCodecError::TooLarge {
            bytes: xml.len(),
            max: MAX_REMOTE_XML_BYTES,
        });
    }

    let mut reader = quick_xml::Reader::from_str(xml);
    let mut depth: usize = 0;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(start)) => {
                depth += 1;
                if depth > MAX_REMOTE_XML_DEPTH {
                    return Err(RemoteCodecError::TooDeep {
                        max: MAX_REMOTE_XML_DEPTH,
                    });
                }
                check_attribute_cap(&start)?;
            }
            Ok(quick_xml::events::Event::Empty(start)) => {
                // A self-closing element opens no lasting scope but still
                // occupies one nesting level of its own — count it against
                // the cap so the parsed tree's depth invariant is exact
                // (a leaf under MAX open elements must not slip through).
                if depth + 1 > MAX_REMOTE_XML_DEPTH {
                    return Err(RemoteCodecError::TooDeep {
                        max: MAX_REMOTE_XML_DEPTH,
                    });
                }
                check_attribute_cap(&start)?;
            }
            Ok(quick_xml::events::Event::End(_)) => {
                depth = depth.saturating_sub(1);
            }
            Ok(quick_xml::events::Event::Eof) => return Ok(()),
            Ok(_) => {}
            Err(error) => return Err(RemoteCodecError::Malformed(error.to_string())),
        }
    }
}

/// Count one element's attributes (namespace declarations included) against
/// the cap; a malformed attribute list is a malformed payload.
fn check_attribute_cap(start: &quick_xml::events::BytesStart<'_>) -> Result<(), RemoteCodecError> {
    let mut count = 0usize;
    for attribute in start.attributes() {
        attribute.map_err(|error| RemoteCodecError::Malformed(error.to_string()))?;
        count += 1;
        if count > MAX_REMOTE_XML_ATTRIBUTES {
            return Err(RemoteCodecError::TooManyAttributes {
                max: MAX_REMOTE_XML_ATTRIBUTES,
            });
        }
    }
    Ok(())
}

/// A `Stanza` carried across the kameo remote boundary as bounded XML text.
#[derive(Debug, Clone)]
pub struct RemoteStanza(pub Stanza);

impl PartialEq for RemoteStanza {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_element() == other.0.to_element()
    }
}

impl Eq for RemoteStanza {}

impl Serialize for RemoteStanza {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let xml = encode_stanza(&self.0).map_err(|error| {
            metrics::record_remote_codec_drop(error.reason());
            S::Error::custom(error)
        })?;
        serializer.serialize_str(&xml)
    }
}

impl<'de> Deserialize<'de> for RemoteStanza {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let xml = String::deserialize(deserializer)?;
        decode_stanza(&xml).map(RemoteStanza).map_err(|error| {
            metrics::record_remote_codec_drop(error.reason());
            D::Error::custom(error)
        })
    }
}

/// An arbitrary XML `Element` carried across the kameo remote boundary as
/// bounded XML text.
#[derive(Debug, Clone)]
pub struct RemoteElement(pub minidom::Element);

// Structural equality delegates to minidom's Element comparison; used by
// the remote-resource resync convergence recheck (#1680).
impl PartialEq for RemoteElement {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Serialize for RemoteElement {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let xml = waddle_xmpp::parser::element_to_string(&self.0).map_err(|error| {
            metrics::record_remote_codec_drop(metrics::RemoteCodecDropReason::Serialize);
            S::Error::custom(error)
        })?;
        serializer.serialize_str(&xml)
    }
}

impl<'de> Deserialize<'de> for RemoteElement {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let xml = String::deserialize(deserializer)?;
        decode_element(&xml).map(RemoteElement).map_err(|error| {
            metrics::record_remote_codec_drop(error.reason());
            D::Error::custom(error)
        })
    }
}

// NB on the XML-generation hard rule: several tests below assemble XML with
// `format!`/`push_str`. That is deliberate and necessary — they produce
// ADVERSARIAL payloads (nesting bombs, attribute bombs, oversized and
// malformed documents) that typed builders cannot emit by construction;
// builder-only XML is exactly the property the production encode path keeps.
// The rule protects wire/production XML, which never goes through these
// helpers.
#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn message_with_thread() -> Stanza {
        let mut message = xmpp_parsers::message::Message::new(Some(
            jid::Jid::from_str("romeo@example.net").expect("valid jid"),
        ));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "wherefore art thou".to_string(),
        );
        message.thread = Some(xmpp_parsers::message::Thread {
            id: "e0ffe42b28561960c6b12b944a092794b9683a38".to_string(),
            parent: None,
        });
        Stanza::Message(message)
    }

    #[test]
    fn stanza_round_trips_preserving_thread() {
        let stanza = message_with_thread();
        let xml = encode_stanza(&stanza).expect("encode");
        assert!(xml.contains("<thread"), "thread element on the wire: {xml}");
        let decoded = decode_stanza(&xml).expect("decode");
        match decoded {
            Stanza::Message(message) => {
                let thread = message.thread.expect("thread survives round-trip");
                assert_eq!(thread.id, "e0ffe42b28561960c6b12b944a092794b9683a38");
            }
            other => panic!("expected message, got {}", other.name()),
        }
    }

    #[test]
    fn iq_and_presence_round_trip() {
        let iq = Stanza::Iq(Box::new(xmpp_parsers::iq::Iq::from_get(
            "ping-1",
            xmpp_parsers::ping::Ping,
        )));
        let decoded = decode_stanza(&encode_stanza(&iq).expect("encode iq")).expect("decode iq");
        assert_eq!(decoded.name(), "iq");

        let presence = Stanza::Presence(xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::None,
        ));
        let decoded = decode_stanza(&encode_stanza(&presence).expect("encode presence"))
            .expect("decode presence");
        assert_eq!(decoded.name(), "presence");
    }

    #[test]
    fn deeply_nested_payload_is_a_typed_error_not_a_crash() {
        // 4096 nested elements: must be rejected by the pre-scan depth cap,
        // never handed to minidom's recursive parser.
        let depth = 4096;
        let mut xml = String::new();
        for _ in 0..depth {
            xml.push_str("<a xmlns='jabber:client'>");
        }
        for _ in 0..depth {
            xml.push_str("</a>");
        }
        let err = decode_element(&xml).expect_err("depth cap enforced");
        assert!(matches!(err, RemoteCodecError::TooDeep { .. }));
    }

    #[test]
    fn self_closing_leaf_counts_against_the_depth_cap() {
        // A self-closing element under MAX open elements is one level deeper
        // than the cap and must be rejected; the same leaf one level higher
        // must pass. (Open/close pairs at exactly MAX are the control.)
        let build = |open_depth: usize, with_leaf: bool| {
            let mut xml = String::from("<a xmlns='jabber:client'>");
            for _ in 1..open_depth {
                xml.push_str("<a>");
            }
            if with_leaf {
                xml.push_str("<leaf/>");
            }
            for _ in 0..open_depth {
                xml.push_str("</a>");
            }
            xml
        };
        let err = decode_element(&build(MAX_REMOTE_XML_DEPTH, true))
            .expect_err("leaf under MAX opens exceeds the cap");
        assert!(matches!(err, RemoteCodecError::TooDeep { .. }));
        decode_element(&build(MAX_REMOTE_XML_DEPTH - 1, true))
            .expect("leaf at exactly MAX depth is allowed");
        decode_element(&build(MAX_REMOTE_XML_DEPTH, false))
            .expect("MAX open/close pairs without the leaf are allowed");
    }

    #[test]
    fn attribute_bomb_is_a_typed_error() {
        let mut xml = String::from("<message xmlns='jabber:client'");
        for index in 0..(MAX_REMOTE_XML_ATTRIBUTES + 8) {
            xml.push_str(&format!(" a{index}='v'"));
        }
        xml.push_str("/>");
        let err = decode_element(&xml).expect_err("attribute cap enforced");
        assert!(matches!(err, RemoteCodecError::TooManyAttributes { .. }));
    }

    #[test]
    fn oversized_payload_is_a_typed_error() {
        let body = "x".repeat(MAX_REMOTE_XML_BYTES + 1);
        let xml = format!("<message xmlns='jabber:client'><body>{body}</body></message>");
        let err = decode_element(&xml).expect_err("byte cap enforced");
        assert!(matches!(err, RemoteCodecError::TooLarge { .. }));
    }

    #[test]
    fn malformed_and_non_stanza_payloads_are_typed_errors() {
        assert!(matches!(
            decode_element("<unclosed"),
            Err(RemoteCodecError::Malformed(_))
        ));
        assert!(matches!(
            decode_stanza("<stream xmlns='jabber:client'/>"),
            Err(RemoteCodecError::NotAStanza { .. })
        ));
    }

    #[test]
    fn serde_wrappers_round_trip_through_json() {
        // Exercise the Serialize/Deserialize impls the kameo transport uses
        // (rmp-serde in production; JSON here exercises the same code path).
        let wrapped = RemoteStanza(message_with_thread());
        let json = serde_json::to_string(&wrapped).expect("serialize");
        let back: RemoteStanza = serde_json::from_str(&json).expect("deserialize");
        match back.0 {
            Stanza::Message(message) => assert!(message.thread.is_some()),
            other => panic!("expected message, got {}", other.name()),
        }

        let element = RemoteElement(
            "<x xmlns='urn:example'><y/></x>"
                .parse::<minidom::Element>()
                .expect("parse element"),
        );
        let json = serde_json::to_string(&element).expect("serialize element");
        let back: RemoteElement = serde_json::from_str(&json).expect("deserialize element");
        assert_eq!(back.0.name(), "x");
    }

    #[test]
    fn serde_rejects_a_bounds_violating_payload() {
        let mut xml = String::new();
        for _ in 0..64 {
            xml.push_str("<a xmlns='urn:example'>");
        }
        for _ in 0..64 {
            xml.push_str("</a>");
        }
        let json = serde_json::to_string(&xml).expect("wrap as json string");
        let result: Result<RemoteElement, _> = serde_json::from_str(&json);
        assert!(result.is_err(), "bounds enforced through the serde path");
    }
}
