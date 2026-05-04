//! XEP-0308: Last Message Correction
//!
//! Provides helpers for detecting, parsing, and building message correction
//! elements. A correction replaces a previously sent message.
//!
//! ## XML Format
//!
//! ```xml
//! <message type='chat' to='romeo@example.com' id='new-id'>
//!   <body>Corrected text</body>
//!   <replace xmlns='urn:xmpp:message-correct:0' id='original-id'/>
//! </message>
//! ```
//!
//! ## Rules
//!
//! - Only the original sender may correct their own messages.
//! - A correction replaces the most recent message with the given id.
//! - The correction message MUST have a new unique id and a `<body/>`.
//! - Multiple corrections to the same message are allowed (last wins).
//!
//! ## Server Behavior
//!
//! The server transparently routes corrections. Archival should store
//! corrections as new messages referencing the original via the replace id.

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0308 Last Message Correction.
pub const NS_MESSAGE_CORRECT: &str = "urn:xmpp:message-correct:0";

/// Errors that can occur when parsing correction elements.
#[derive(Debug, Error)]
pub enum CorrectionError {
    /// A `<replace/>` element is missing its required `id` attribute.
    #[error("replace element missing id attribute")]
    MissingId,
}

/// A message correction reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    /// The id of the original message being replaced.
    pub replaces_id: String,
}

impl Correction {
    /// Create a new correction referencing the given message id.
    pub fn new(replaces_id: impl Into<String>) -> Self {
        Self {
            replaces_id: replaces_id.into(),
        }
    }
}

/// Trait for types that can carry message corrections.
///
/// Enables both `Message` and future MUC message wrappers
/// to participate in correction-aware processing.
pub trait CorrectionCarrier {
    /// Extract the correction reference from this carrier, if present.
    fn correction(&self) -> Option<Correction>;

    /// Returns `true` if this carrier is a message correction.
    fn is_correction(&self) -> bool {
        self.correction().is_some()
    }

    /// Returns the id of the message being replaced, if this is a correction.
    fn replaces_id(&self) -> Option<String> {
        self.correction().map(|c| c.replaces_id)
    }
}

impl CorrectionCarrier for Message {
    fn correction(&self) -> Option<Correction> {
        extract_correction_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<replace/>` correction element.
pub fn is_replace_element(elem: &Element) -> bool {
    elem.ns() == NS_MESSAGE_CORRECT && elem.name() == "replace"
}

/// Check if a message contains a `<replace/>` correction.
pub fn is_correction_message(msg: &Message) -> bool {
    msg.payloads.iter().any(is_replace_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse the correction from a message's payloads.
///
/// Returns `Ok(Some(Correction))` when the message contains a conformant
/// `<replace/>` element, `Ok(None)` when no correction payload is present, and
/// `Err(CorrectionError::MissingId)` when any `<replace/>` omits the required
/// non-empty `id` attribute mandated by XEP-0308.
pub fn parse_correction_from_message(msg: &Message) -> Result<Option<Correction>, CorrectionError> {
    let Some(elem) = msg.payloads.iter().find(|elem| is_replace_element(elem)) else {
        return Ok(None);
    };

    let id = elem
        .attr("id")
        .filter(|id| !id.is_empty())
        .ok_or(CorrectionError::MissingId)?;

    Ok(Some(Correction::new(id.to_owned())))
}

/// Extract the correction from a message's payloads.
///
/// Returns `Some(Correction)` if the message contains a conformant `<replace/>`
/// element with a non-empty `id` attribute. Malformed correction payloads are
/// ignored here; use [`parse_correction_from_message`] when they must be
/// surfaced as protocol errors.
pub fn extract_correction_from_message(msg: &Message) -> Option<Correction> {
    parse_correction_from_message(msg).ok().flatten()
}

/// Extract the replaced message id from a correction message.
pub fn extract_replaces_id(msg: &Message) -> Option<String> {
    extract_correction_from_message(msg).map(|c| c.replaces_id)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<replace xmlns='urn:xmpp:message-correct:0' id='...'/>` element.
pub fn build_replace_element(original_id: &str) -> Element {
    Element::builder("replace", NS_MESSAGE_CORRECT)
        .attr("id", original_id)
        .build()
}

/// Build a correction message.
///
/// Creates a `<message type='chat'>` with a new body and a `<replace/>` element
/// referencing the original message id.
pub fn build_correction_message(
    to: impl Into<Option<jid::Jid>>,
    from: impl Into<Option<jid::Jid>>,
    new_body: &str,
    original_id: &str,
) -> Message {
    use xmpp_parsers::message::Body;

    let mut msg = Message::new(to.into());
    msg.from = from.into();
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.id = Some(uuid::Uuid::new_v4().to_string());
    msg.bodies.insert(String::new(), Body(new_body.to_owned()));
    msg.payloads.push(build_replace_element(original_id));
    msg
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Set or replace the correction reference on a message.
pub fn set_correction(msg: &mut Message, original_id: &str) {
    strip_correction(msg);
    msg.payloads.push(build_replace_element(original_id));
}

/// Remove any correction reference from a message.
pub fn strip_correction(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_MESSAGE_CORRECT);
}

// ── Conversion ───────────────────────────────────────────────────────

impl From<xmpp_parsers::message_correct::Replace> for Correction {
    fn from(replace: xmpp_parsers::message_correct::Replace) -> Self {
        Self::new(replace.id)
    }
}

impl From<Correction> for xmpp_parsers::message_correct::Replace {
    fn from(correction: Correction) -> Self {
        Self {
            id: correction.replaces_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Body, Message, MessageType};

    #[test]
    fn test_is_replace_element() {
        let replace = Element::builder("replace", NS_MESSAGE_CORRECT)
            .attr("id", "orig-1")
            .build();
        assert!(is_replace_element(&replace));

        let wrong_ns = Element::builder("replace", "jabber:client").build();
        assert!(!is_replace_element(&wrong_ns));

        let wrong_name = Element::builder("correct", NS_MESSAGE_CORRECT).build();
        assert!(!is_replace_element(&wrong_name));
    }

    #[test]
    fn test_is_correction_message() {
        let xml = "<message xmlns='jabber:client' type='chat' id='new-1'>\
                    <body>Fixed typo</body>\
                    <replace xmlns='urn:xmpp:message-correct:0' id='orig-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(is_correction_message(&msg));

        let plain = Message::new(None::<jid::Jid>);
        assert!(!is_correction_message(&plain));
    }

    #[test]
    fn test_parse_correction() {
        let xml = "<message xmlns='jabber:client' type='chat' id='new-1'>\
                    <body>Corrected text</body>\
                    <replace xmlns='urn:xmpp:message-correct:0' id='orig-42'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let correction = parse_correction_from_message(&msg)
            .expect("valid correction")
            .expect("has correction");
        assert_eq!(correction.replaces_id, "orig-42");
        assert_eq!(extract_correction_from_message(&msg), Some(correction));
    }

    #[test]
    fn test_extract_correction_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_correction_from_message(&msg).is_none());
    }

    #[test]
    fn test_parse_correction_missing_id_errors() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Bad</body>\
                    <replace xmlns='urn:xmpp:message-correct:0'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(matches!(
            parse_correction_from_message(&msg),
            Err(CorrectionError::MissingId)
        ));
        assert!(extract_correction_from_message(&msg).is_none());
    }

    #[test]
    fn test_parse_correction_empty_id_errors() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Bad</body>\
                    <replace xmlns='urn:xmpp:message-correct:0' id=''/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(matches!(
            parse_correction_from_message(&msg),
            Err(CorrectionError::MissingId)
        ));
        assert!(extract_correction_from_message(&msg).is_none());
    }

    #[test]
    fn test_parse_correction_ignores_later_malformed_replace() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Fixed</body>\
                    <replace xmlns='urn:xmpp:message-correct:0' id='orig-1'/>\
                    <replace xmlns='urn:xmpp:message-correct:0' id=''/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        let correction = parse_correction_from_message(&msg)
            .expect("valid correction")
            .expect("has correction");
        assert_eq!(correction.replaces_id, "orig-1");
    }

    #[test]
    fn test_extract_replaces_id() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Fix</body>\
                    <replace xmlns='urn:xmpp:message-correct:0' id='abc-123'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert_eq!(extract_replaces_id(&msg), Some("abc-123".to_owned()));
    }

    #[test]
    fn test_build_replace_element() {
        let elem = build_replace_element("msg-99");
        assert_eq!(elem.name(), "replace");
        assert_eq!(elem.ns(), NS_MESSAGE_CORRECT);
        assert_eq!(elem.attr("id"), Some("msg-99"));
    }

    #[test]
    fn test_build_correction_message() {
        let to: jid::Jid = "romeo@example.com".parse().expect("valid jid");
        let from: jid::Jid = "juliet@example.com".parse().expect("valid jid");
        let msg = build_correction_message(to.clone(), from.clone(), "Fixed text", "orig-1");

        assert_eq!(msg.to, Some(to));
        assert_eq!(msg.from, Some(from));
        assert_eq!(msg.type_, MessageType::Chat);
        assert!(msg.id.is_some());
        assert_eq!(msg.bodies.get("").map(|b| b.0.as_str()), Some("Fixed text"));
        assert_eq!(extract_replaces_id(&msg), Some("orig-1".to_owned()));
    }

    #[test]
    fn test_set_correction() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.bodies
            .insert(String::new(), Body("Updated".to_string()));

        set_correction(&mut msg, "orig-5");
        assert_eq!(extract_replaces_id(&msg), Some("orig-5".to_owned()));

        // Setting again replaces
        set_correction(&mut msg, "orig-6");
        assert_eq!(extract_replaces_id(&msg), Some("orig-6".to_owned()));
        let count = msg
            .payloads
            .iter()
            .filter(|e| e.ns() == NS_MESSAGE_CORRECT)
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_strip_correction() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Fix</body>\
                    <replace xmlns='urn:xmpp:message-correct:0' id='orig-1'/>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        strip_correction(&mut msg);
        assert!(!is_correction_message(&msg));
        assert!(!msg.bodies.is_empty()); // body preserved
    }

    #[test]
    fn test_correction_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='chat' id='new-1'>\
                    <body>Fixed</body>\
                    <replace xmlns='urn:xmpp:message-correct:0' id='orig-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.is_correction());
        assert_eq!(msg.replaces_id(), Some("orig-1".to_owned()));
        assert_eq!(msg.correction(), Some(Correction::new("orig-1")));

        // Non-correction
        let plain = Message::new(None::<jid::Jid>);
        assert!(!plain.is_correction());
        assert_eq!(plain.replaces_id(), None);
    }

    #[test]
    fn test_roundtrip_conversion() {
        let correction = Correction::new("test-id");
        let replace: xmpp_parsers::message_correct::Replace = correction.clone().into();
        assert_eq!(replace.id, "test-id");

        let back: Correction = replace.into();
        assert_eq!(back, correction);
    }

    #[test]
    fn test_correction_new() {
        let c = Correction::new("abc");
        assert_eq!(c.replaces_id, "abc");

        let c2 = Correction::new(String::from("def"));
        assert_eq!(c2.replaces_id, "def");
    }
}
