//! XEP-0184: Message Delivery Receipts
//!
//! Provides helpers for detecting, parsing, and building delivery receipt
//! elements within XMPP message stanzas.
//!
//! ## Protocol Flow
//!
//! 1. Sender includes `<request xmlns='urn:xmpp:receipts'/>` in a message
//! 2. Recipient receives the message and sends back a receipt:
//!    `<received xmlns='urn:xmpp:receipts' id='original-message-id'/>`
//!
//! ## XML Format
//!
//! Request a receipt:
//! ```xml
//! <message id='msg-1' type='chat' to='romeo@example.com'>
//!   <body>Hello!</body>
//!   <request xmlns='urn:xmpp:receipts'/>
//! </message>
//! ```
//!
//! Acknowledge delivery:
//! ```xml
//! <message type='chat' to='juliet@example.com'>
//!   <received xmlns='urn:xmpp:receipts' id='msg-1'/>
//! </message>
//! ```
//!
//! ## Service Discovery
//!
//! Clients advertise support via `urn:xmpp:receipts` in disco#info.
//! The server transparently routes receipt requests and acknowledgments.
//! Receipts are primarily for 1:1 chat messages.

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0184 Message Delivery Receipts.
pub const NS_RECEIPTS: &str = "urn:xmpp:receipts";

/// Errors that can occur when parsing delivery receipt elements.
#[derive(Debug, Error)]
pub enum ReceiptError {
    /// A `<received/>` element is missing its required `id` attribute.
    #[error("received element missing id attribute")]
    MissingId,
}

/// Represents the type of receipt element found in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptKind {
    /// A `<request/>` element asking the recipient to acknowledge delivery.
    Request,
    /// A `<received id='...'>` element acknowledging receipt of a specific message.
    Received(String),
}

/// Trait for types that can carry delivery receipt elements.
///
/// Abstracts the ability to detect and extract receipt information,
/// enabling future message wrapper types to participate in receipt processing.
pub trait ReceiptCarrier {
    /// Extract the receipt kind from this carrier, if present.
    fn receipt(&self) -> Option<ReceiptKind>;

    /// Returns `true` if this carrier requests a delivery receipt.
    fn requests_receipt(&self) -> bool {
        matches!(self.receipt(), Some(ReceiptKind::Request))
    }

    /// Returns `true` if this carrier is a delivery receipt acknowledgment.
    fn is_receipt_ack(&self) -> bool {
        matches!(self.receipt(), Some(ReceiptKind::Received(_)))
    }

    /// Returns the acknowledged message ID if this is a receipt acknowledgment.
    fn receipt_ack_id(&self) -> Option<String> {
        match self.receipt() {
            Some(ReceiptKind::Received(id)) => Some(id),
            _ => None,
        }
    }

    /// Returns `true` if this is a standalone receipt (no body).
    fn is_standalone_receipt(&self) -> bool;
}

impl ReceiptCarrier for Message {
    fn receipt(&self) -> Option<ReceiptKind> {
        extract_receipt_from_message(self)
    }

    fn is_standalone_receipt(&self) -> bool {
        self.bodies.is_empty() && self.is_receipt_ack()
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a receipt `<request/>`.
pub fn is_receipt_request_element(elem: &Element) -> bool {
    elem.ns() == NS_RECEIPTS && elem.name() == "request"
}

/// Check if an element is a receipt `<received/>`.
pub fn is_receipt_received_element(elem: &Element) -> bool {
    elem.ns() == NS_RECEIPTS && elem.name() == "received"
}

/// Check if a message contains a `<request/>` for delivery receipt.
pub fn has_receipt_request(msg: &Message) -> bool {
    msg.payloads.iter().any(is_receipt_request_element)
}

/// Check if a message contains a `<received/>` delivery acknowledgment.
pub fn has_receipt_received(msg: &Message) -> bool {
    msg.payloads.iter().any(is_receipt_received_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract the receipt kind from a message's payloads.
///
/// Checks for `<received/>` first (it's more specific), then `<request/>`.
/// A message should not contain both, but if it does, `received` takes priority.
pub fn extract_receipt_from_message(msg: &Message) -> Option<ReceiptKind> {
    for elem in &msg.payloads {
        if elem.ns() != NS_RECEIPTS {
            continue;
        }
        match elem.name() {
            "received" => {
                let id = elem.attr("id").unwrap_or("").to_owned();
                if !id.is_empty() {
                    return Some(ReceiptKind::Received(id));
                }
            }
            "request" => return Some(ReceiptKind::Request),
            _ => {}
        }
    }
    None
}

/// Extract the acknowledged message ID from a `<received/>` element.
pub fn extract_received_id(msg: &Message) -> Option<String> {
    msg.payloads
        .iter()
        .find(|e| is_receipt_received_element(e))
        .and_then(|e| e.attr("id"))
        .filter(|id| !id.is_empty())
        .map(|id| id.to_owned())
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<request xmlns='urn:xmpp:receipts'/>` element.
pub fn build_receipt_request_element() -> Element {
    Element::builder("request", NS_RECEIPTS).build()
}

/// Build a `<received xmlns='urn:xmpp:receipts' id='...'/>` element.
pub fn build_receipt_received_element(id: &str) -> Element {
    Element::builder("received", NS_RECEIPTS)
        .attr("id", id)
        .build()
}

/// Build a standalone delivery receipt message.
///
/// Creates a `<message type='chat'>` containing only a `<received/>` element.
/// The `id` references the original message being acknowledged.
pub fn build_receipt_message(
    to: impl Into<Option<jid::Jid>>,
    from: impl Into<Option<jid::Jid>>,
    original_message_id: &str,
) -> Message {
    let mut msg = Message::new(to.into());
    msg.from = from.into();
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.payloads
        .push(build_receipt_received_element(original_message_id));
    msg
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a `<request/>` element to a message, replacing any existing receipt elements.
pub fn set_receipt_request(msg: &mut Message) {
    strip_receipts(msg);
    msg.payloads.push(build_receipt_request_element());
}

/// Add a `<received/>` element to a message, replacing any existing receipt elements.
pub fn set_receipt_received(msg: &mut Message, original_message_id: &str) {
    strip_receipts(msg);
    msg.payloads
        .push(build_receipt_received_element(original_message_id));
}

/// Remove all receipt elements from a message.
pub fn strip_receipts(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_RECEIPTS);
}

/// Check if a message is a standalone delivery receipt (no body, only `<received/>`).
///
/// These messages should not be archived but must be routed to the sender.
pub fn is_standalone_receipt(msg: &Message) -> bool {
    msg.bodies.is_empty() && has_receipt_received(msg)
}

// ── Conversion ───────────────────────────────────────────────────────

/// Convert from `xmpp_parsers::receipts::Request`.
impl From<xmpp_parsers::receipts::Request> for ReceiptKind {
    fn from(_: xmpp_parsers::receipts::Request) -> Self {
        Self::Request
    }
}

/// Convert from `xmpp_parsers::receipts::Received`.
impl From<xmpp_parsers::receipts::Received> for ReceiptKind {
    fn from(received: xmpp_parsers::receipts::Received) -> Self {
        Self::Received(received.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Body, Message, MessageType};

    #[test]
    fn test_is_receipt_request_element() {
        let request = Element::builder("request", NS_RECEIPTS).build();
        assert!(is_receipt_request_element(&request));

        let wrong_ns = Element::builder("request", "jabber:client").build();
        assert!(!is_receipt_request_element(&wrong_ns));

        let wrong_name = Element::builder("received", NS_RECEIPTS).build();
        assert!(!is_receipt_request_element(&wrong_name));
    }

    #[test]
    fn test_is_receipt_received_element() {
        let received = Element::builder("received", NS_RECEIPTS)
            .attr("id", "msg-1")
            .build();
        assert!(is_receipt_received_element(&received));

        let request = Element::builder("request", NS_RECEIPTS).build();
        assert!(!is_receipt_received_element(&request));
    }

    #[test]
    fn test_has_receipt_request() {
        let xml = "<message xmlns='jabber:client' type='chat' id='msg-1'>\
                    <body>Hello</body>\
                    <request xmlns='urn:xmpp:receipts'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(has_receipt_request(&msg));
        assert!(!has_receipt_received(&msg));
    }

    #[test]
    fn test_has_receipt_received() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <received xmlns='urn:xmpp:receipts' id='msg-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(has_receipt_received(&msg));
        assert!(!has_receipt_request(&msg));
    }

    #[test]
    fn test_extract_receipt_request() {
        let xml = "<message xmlns='jabber:client' type='chat' id='msg-2'>\
                    <body>Test</body>\
                    <request xmlns='urn:xmpp:receipts'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert_eq!(
            extract_receipt_from_message(&msg),
            Some(ReceiptKind::Request)
        );
    }

    #[test]
    fn test_extract_receipt_received() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <received xmlns='urn:xmpp:receipts' id='original-42'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert_eq!(
            extract_receipt_from_message(&msg),
            Some(ReceiptKind::Received("original-42".to_owned()))
        );
    }

    #[test]
    fn test_extract_received_id() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <received xmlns='urn:xmpp:receipts' id='abc-123'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert_eq!(extract_received_id(&msg), Some("abc-123".to_owned()));
    }

    #[test]
    fn test_extract_received_id_missing() {
        let msg = Message::new(None::<jid::Jid>);
        assert_eq!(extract_received_id(&msg), None);
    }

    #[test]
    fn test_build_receipt_request_element() {
        let elem = build_receipt_request_element();
        assert_eq!(elem.name(), "request");
        assert_eq!(elem.ns(), NS_RECEIPTS);
    }

    #[test]
    fn test_build_receipt_received_element() {
        let elem = build_receipt_received_element("msg-99");
        assert_eq!(elem.name(), "received");
        assert_eq!(elem.ns(), NS_RECEIPTS);
        assert_eq!(elem.attr("id"), Some("msg-99"));
    }

    #[test]
    fn test_build_receipt_message() {
        let to: jid::Jid = "juliet@example.com".parse().expect("valid jid");
        let from: jid::Jid = "romeo@example.com".parse().expect("valid jid");
        let msg = build_receipt_message(to.clone(), from.clone(), "original-1");

        assert_eq!(msg.to, Some(to));
        assert_eq!(msg.from, Some(from));
        assert_eq!(msg.type_, MessageType::Chat);
        assert!(msg.bodies.is_empty());
        assert_eq!(extract_received_id(&msg), Some("original-1".to_owned()));
    }

    #[test]
    fn test_set_receipt_request() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_receipt_request(&mut msg);

        assert!(has_receipt_request(&msg));
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|e| e.ns() == NS_RECEIPTS)
                .count(),
            1
        );
    }

    #[test]
    fn test_set_receipt_received_replaces_request() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_receipt_request(&mut msg);
        assert!(has_receipt_request(&msg));

        set_receipt_received(&mut msg, "msg-1");
        assert!(!has_receipt_request(&msg));
        assert!(has_receipt_received(&msg));
        assert_eq!(extract_received_id(&msg), Some("msg-1".to_owned()));
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|e| e.ns() == NS_RECEIPTS)
                .count(),
            1
        );
    }

    #[test]
    fn test_strip_receipts() {
        let xml = "<message xmlns='jabber:client' type='chat' id='msg-1'>\
                    <body>Hello</body>\
                    <request xmlns='urn:xmpp:receipts'/>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        strip_receipts(&mut msg);

        assert!(!has_receipt_request(&msg));
        assert!(msg.payloads.iter().all(|e| e.ns() != NS_RECEIPTS));
    }

    #[test]
    fn test_is_standalone_receipt() {
        // Receipt-only, no body → standalone
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <received xmlns='urn:xmpp:receipts' id='msg-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(is_standalone_receipt(&msg));

        // Receipt + body → not standalone
        let mut msg_with_body = Message::new(None::<jid::Jid>);
        msg_with_body
            .bodies
            .insert(String::new(), Body("Hello".to_string()));
        msg_with_body
            .payloads
            .push(build_receipt_received_element("msg-1"));
        assert!(!is_standalone_receipt(&msg_with_body));

        // Request only → not standalone receipt
        let xml2 = "<message xmlns='jabber:client' type='chat'>\
                     <request xmlns='urn:xmpp:receipts'/>\
                     </message>";
        let msg2 =
            Message::try_from(xml2.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(!is_standalone_receipt(&msg2));
    }

    #[test]
    fn test_receipt_carrier_trait() {
        // Request
        let xml = "<message xmlns='jabber:client' type='chat' id='msg-1'>\
                    <body>Hello</body>\
                    <request xmlns='urn:xmpp:receipts'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(msg.requests_receipt());
        assert!(!msg.is_receipt_ack());
        assert_eq!(msg.receipt_ack_id(), None);
        assert!(!msg.is_standalone_receipt());

        // Received
        let xml2 = "<message xmlns='jabber:client' type='chat'>\
                     <received xmlns='urn:xmpp:receipts' id='msg-1'/>\
                     </message>";
        let msg2 =
            Message::try_from(xml2.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(!msg2.requests_receipt());
        assert!(msg2.is_receipt_ack());
        assert_eq!(msg2.receipt_ack_id(), Some("msg-1".to_owned()));
        assert!(msg2.is_standalone_receipt());
    }

    #[test]
    fn test_conversion_from_xmpp_parsers() {
        let request_kind: ReceiptKind = xmpp_parsers::receipts::Request.into();
        assert_eq!(request_kind, ReceiptKind::Request);

        let received_kind: ReceiptKind = xmpp_parsers::receipts::Received {
            id: "abc".to_owned(),
        }
        .into();
        assert_eq!(received_kind, ReceiptKind::Received("abc".to_owned()));
    }

    #[test]
    fn test_no_receipt_in_plain_message() {
        let msg = Message::new(None::<jid::Jid>);
        assert_eq!(extract_receipt_from_message(&msg), None);
        assert!(!msg.requests_receipt());
        assert!(!msg.is_receipt_ack());
    }

    #[test]
    fn test_received_with_empty_id_ignored() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <received xmlns='urn:xmpp:receipts' id=''/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        // Empty id should be ignored
        assert_eq!(extract_receipt_from_message(&msg), None);
    }
}
