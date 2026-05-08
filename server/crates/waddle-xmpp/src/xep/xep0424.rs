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
//!   <retracted xmlns='urn:xmpp:message-retract:1' id='retract-1' stamp='2024-01-15T12:00:00Z'/>
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
    /// A `<retracted/>` element is missing its required `id` attribute.
    #[error("retracted element missing retraction id attribute")]
    MissingRetractionId,
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
    /// The id of the retraction message that produced this tombstone.
    pub retraction_id: String,
    /// The timestamp when the message was retracted (ISO 8601 / XEP-0082).
    pub stamp: Option<String>,
}

impl Retracted {
    /// Create a new retracted tombstone.
    pub fn new(retraction_id: impl Into<String>, stamp: Option<String>) -> Self {
        Self {
            retraction_id: retraction_id.into(),
            stamp,
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
                let retraction_id = elem.attr("id").unwrap_or("").to_owned();
                if !retraction_id.is_empty() {
                    return Some(RetractionKind::Tombstone(Retracted::new(
                        retraction_id,
                        elem.attr("stamp").map(ToOwned::to_owned),
                    )));
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

/// Build a `<retracted id='...' xmlns='urn:xmpp:message-retract:1'/>` tombstone element.
pub fn build_retracted_element(retraction_id: &str, stamp: Option<&str>) -> Element {
    let mut builder = Element::builder("retracted", NS_MESSAGE_RETRACT).attr("id", retraction_id);
    if let Some(stamp) = stamp {
        builder = builder.attr("stamp", stamp);
    }
    builder.build()
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
    retraction_id: &str,
    stamp: Option<&str>,
) -> Message {
    let mut msg = Message::new(to.into());
    msg.from = from.into();
    msg.type_ = xmpp_parsers::message::MessageType::Groupchat;
    msg.id = Some(original_id.to_owned());
    msg.payloads
        .push(build_retracted_element(retraction_id, stamp));
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
mod tests;
