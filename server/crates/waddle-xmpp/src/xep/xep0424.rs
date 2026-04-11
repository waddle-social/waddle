//! XEP-0424: Message Retraction
//!
//! Provides helpers for detecting, parsing, and building message retraction
//! elements. A retraction removes a previously sent message.
//!
//! ## XML Format
//!
//! Retract a message:
//! ```xml
//! <message type='groupchat' to='room@muc.example.com' id='retract-1'>
//!   <retract id='original-msg-id' xmlns='urn:xmpp:message-retract:1'/>
//!   <body>This person attempted to retract a previous message.</body>
//! </message>
//! ```
//!
//! Tombstone (server-side replacement in archives):
//! ```xml
//! <message type='groupchat' from='room@muc.example.com/nick' id='original-msg-id'>
//!   <retracted stamp='2024-01-15T12:00:00Z' xmlns='urn:xmpp:message-retract:1'/>
//! </message>
//! ```
//!
//! ## Rules
//!
//! - Only the original sender may retract their own messages.
//! - Moderators may retract any message in a MUC (XEP-0425).
//! - The retraction message includes a `<body/>` as fallback for clients
//!   that don't support retractions.
//! - The server transparently routes retractions.

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0424 Message Retraction.
pub const NS_MESSAGE_RETRACT: &str = "urn:xmpp:message-retract:1";

/// Errors that can occur when parsing retraction elements.
#[derive(Debug, Error)]
pub enum RetractionError {
    /// A `<retract/>` element is missing its required `id` attribute.
    #[error("retract element missing id attribute")]
    MissingId,
    /// A `<retracted/>` element is missing its required `stamp` attribute.
    #[error("retracted element missing stamp attribute")]
    MissingStamp,
}

/// A message retraction request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retraction {
    /// The id of the message being retracted.
    pub retracts_id: String,
}

impl Retraction {
    /// Create a new retraction referencing the given message id.
    pub fn new(retracts_id: impl Into<String>) -> Self {
        Self {
            retracts_id: retracts_id.into(),
        }
    }
}

/// A tombstone indicating a message was retracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retracted {
    /// The timestamp when the message was retracted (ISO 8601 / XEP-0082).
    pub stamp: String,
}

impl Retracted {
    /// Create a new retracted tombstone.
    pub fn new(stamp: impl Into<String>) -> Self {
        Self {
            stamp: stamp.into(),
        }
    }
}

/// What kind of retraction element is present in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetractionKind {
    /// A `<retract id='...'>` requesting retraction of a message.
    Request(Retraction),
    /// A `<retracted stamp='...'>` tombstone replacing a retracted message.
    Tombstone(Retracted),
}

/// Trait for types that can carry retraction elements.
pub trait RetractionCarrier {
    /// Extract the retraction kind from this carrier, if present.
    fn retraction(&self) -> Option<RetractionKind>;

    /// Returns `true` if this carrier is a retraction request.
    fn is_retraction(&self) -> bool {
        matches!(self.retraction(), Some(RetractionKind::Request(_)))
    }

    /// Returns the id of the message being retracted, if this is a retraction request.
    fn retracts_id(&self) -> Option<String> {
        match self.retraction() {
            Some(RetractionKind::Request(r)) => Some(r.retracts_id),
            _ => None,
        }
    }

    /// Returns `true` if this carrier is a retraction tombstone.
    fn is_retracted(&self) -> bool {
        matches!(self.retraction(), Some(RetractionKind::Tombstone(_)))
    }
}

impl RetractionCarrier for Message {
    fn retraction(&self) -> Option<RetractionKind> {
        extract_retraction_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<retract/>` element.
pub fn is_retract_element(elem: &Element) -> bool {
    elem.ns() == NS_MESSAGE_RETRACT && elem.name() == "retract"
}

/// Check if an element is a `<retracted/>` tombstone element.
pub fn is_retracted_element(elem: &Element) -> bool {
    elem.ns() == NS_MESSAGE_RETRACT && elem.name() == "retracted"
}

/// Check if a message contains a retraction request.
pub fn is_retraction_message(msg: &Message) -> bool {
    msg.payloads.iter().any(is_retract_element)
}

/// Check if a message is a retraction tombstone.
pub fn is_tombstone_message(msg: &Message) -> bool {
    msg.payloads.iter().any(is_retracted_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract the retraction kind from a message.
pub fn extract_retraction_from_message(msg: &Message) -> Option<RetractionKind> {
    for elem in &msg.payloads {
        if elem.ns() != NS_MESSAGE_RETRACT {
            continue;
        }
        match elem.name() {
            "retract" => {
                let id = elem.attr("id").unwrap_or("").to_owned();
                if !id.is_empty() {
                    return Some(RetractionKind::Request(Retraction::new(id)));
                }
            }
            "retracted" => {
                let stamp = elem.attr("stamp").unwrap_or("").to_owned();
                if !stamp.is_empty() {
                    return Some(RetractionKind::Tombstone(Retracted::new(stamp)));
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract the retracted message id from a retraction request.
pub fn extract_retracts_id(msg: &Message) -> Option<String> {
    msg.payloads
        .iter()
        .find(|e| is_retract_element(e))
        .and_then(|e| e.attr("id"))
        .filter(|id| !id.is_empty())
        .map(|id| id.to_owned())
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<retract id='...' xmlns='urn:xmpp:message-retract:1'/>` element.
pub fn build_retract_element(original_id: &str) -> Element {
    Element::builder("retract", NS_MESSAGE_RETRACT)
        .attr("id", original_id)
        .build()
}

/// Build a `<retracted stamp='...' xmlns='urn:xmpp:message-retract:1'/>` tombstone element.
pub fn build_retracted_element(stamp: &str) -> Element {
    Element::builder("retracted", NS_MESSAGE_RETRACT)
        .attr("stamp", stamp)
        .build()
}

/// Build a retraction request message.
///
/// Includes a fallback `<body/>` for clients that don't support retractions.
pub fn build_retraction_message(
    to: impl Into<Option<jid::Jid>>,
    from: impl Into<Option<jid::Jid>>,
    original_id: &str,
) -> Message {
    use xmpp_parsers::message::Body;

    let mut msg = Message::new(to.into());
    msg.from = from.into();
    msg.type_ = xmpp_parsers::message::MessageType::Groupchat;
    msg.id = Some(uuid::Uuid::new_v4().to_string());
    msg.bodies.insert(
        String::new(),
        Body("This person attempted to retract a previous message.".to_owned()),
    );
    msg.payloads.push(build_retract_element(original_id));
    msg
}

/// Build a tombstone message (replaces retracted message in archives).
pub fn build_tombstone_message(
    to: impl Into<Option<jid::Jid>>,
    from: impl Into<Option<jid::Jid>>,
    original_id: &str,
    stamp: &str,
) -> Message {
    let mut msg = Message::new(to.into());
    msg.from = from.into();
    msg.type_ = xmpp_parsers::message::MessageType::Groupchat;
    msg.id = Some(original_id.to_owned());
    msg.payloads.push(build_retracted_element(stamp));
    msg
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a retraction request to a message, replacing any existing retraction elements.
pub fn set_retraction(msg: &mut Message, original_id: &str) {
    strip_retraction(msg);
    msg.payloads.push(build_retract_element(original_id));
}

/// Remove all retraction elements from a message.
pub fn strip_retraction(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_MESSAGE_RETRACT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Message, MessageType};

    #[test]
    fn test_is_retract_element() {
        let retract = Element::builder("retract", NS_MESSAGE_RETRACT)
            .attr("id", "orig-1")
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
            .attr("stamp", "2024-01-15T12:00:00Z")
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
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(is_retraction_message(&msg));
        assert!(!is_tombstone_message(&msg));
    }

    #[test]
    fn test_is_tombstone_message() {
        let xml = "<message xmlns='jabber:client' type='groupchat' id='orig-1'>\
                    <retracted xmlns='urn:xmpp:message-retract:1' stamp='2024-01-15T12:00:00Z'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(is_tombstone_message(&msg));
        assert!(!is_retraction_message(&msg));
    }

    #[test]
    fn test_extract_retraction_request() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Fallback</body>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='msg-42'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let kind = extract_retraction_from_message(&msg).expect("has retraction");
        assert_eq!(kind, RetractionKind::Request(Retraction::new("msg-42")));
    }

    #[test]
    fn test_extract_retraction_tombstone() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retracted xmlns='urn:xmpp:message-retract:1' stamp='2024-06-01T09:00:00Z'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let kind = extract_retraction_from_message(&msg).expect("has retraction");
        assert_eq!(
            kind,
            RetractionKind::Tombstone(Retracted::new("2024-06-01T09:00:00Z"))
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
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(extract_retraction_from_message(&msg).is_none());
    }

    #[test]
    fn test_extract_retracts_id() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='abc-123'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
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
        let elem = build_retracted_element("2024-01-15T12:00:00Z");
        assert_eq!(elem.name(), "retracted");
        assert_eq!(elem.ns(), NS_MESSAGE_RETRACT);
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
            "2024-01-15T12:00:00Z",
        );

        assert_eq!(msg.id.as_deref(), Some("orig-1"));
        assert!(is_tombstone_message(&msg));
        match extract_retraction_from_message(&msg) {
            Some(RetractionKind::Tombstone(t)) => {
                assert_eq!(t.stamp, "2024-01-15T12:00:00Z");
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
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(msg.is_retraction());
        assert!(!msg.is_retracted());
        assert_eq!(msg.retracts_id(), Some("orig-1".to_owned()));

        // Tombstone
        let xml2 = "<message xmlns='jabber:client' type='groupchat'>\
                     <retracted xmlns='urn:xmpp:message-retract:1' stamp='2024-01-01T00:00:00Z'/>\
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
        let t = Retracted::new("2024-06-01T00:00:00Z");
        assert_eq!(t.stamp, "2024-06-01T00:00:00Z");
    }
}
