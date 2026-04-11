//! XEP-0297: Stanza Forwarding
//!
//! Provides helpers for wrapping and unwrapping forwarded stanzas.
//! Used by Message Carbons (XEP-0280) and MAM (XEP-0313) to embed
//! original messages within wrapper stanzas.
//!
//! ## XML Format
//!
//! ```xml
//! <forwarded xmlns='urn:xmpp:forward:0'>
//!   <delay xmlns='urn:xmpp:delay' stamp='2024-01-15T12:00:00Z'/>
//!   <message xmlns='jabber:client' from='romeo@example.com' to='juliet@example.com'>
//!     <body>Hello!</body>
//!   </message>
//! </forwarded>
//! ```
//!
//! ## Use Cases
//!
//! - **Message Carbons (XEP-0280)**: Wraps sent/received carbon copies.
//! - **MAM (XEP-0313)**: Wraps archived messages in query results.
//! - **Message forwarding**: General-purpose stanza forwarding.

use chrono::{DateTime, Utc};
use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0297 Stanza Forwarding.
pub const NS_FORWARD: &str = "urn:xmpp:forward:0";

/// Namespace for XEP-0203 Delayed Delivery (used within forwarded elements).
const NS_DELAY: &str = "urn:xmpp:delay";

/// A forwarded stanza wrapper.
#[derive(Debug, Clone)]
pub struct ForwardedMessage {
    /// The original message being forwarded.
    pub message: Message,
    /// Optional delay timestamp (when the original was sent).
    pub stamp: Option<DateTime<Utc>>,
    /// Optional entity that originally sent/delayed the stanza.
    pub delay_from: Option<String>,
}

impl ForwardedMessage {
    /// Create a new forwarded message with the current timestamp.
    pub fn new(message: Message) -> Self {
        Self {
            message,
            stamp: Some(Utc::now()),
            delay_from: None,
        }
    }

    /// Create with a specific timestamp.
    pub fn with_stamp(message: Message, stamp: DateTime<Utc>) -> Self {
        Self {
            message,
            stamp: Some(stamp),
            delay_from: None,
        }
    }

    /// Set the delay from entity.
    pub fn with_delay_from(mut self, from: impl Into<String>) -> Self {
        self.delay_from = Some(from.into());
        self
    }
}

/// Trait for types that can carry forwarded stanzas.
pub trait ForwardingCarrier {
    /// Extract a forwarded message from this carrier's payloads.
    fn forwarded_message(&self) -> Option<ForwardedMessage>;

    /// Returns `true` if this carrier contains a forwarded stanza.
    fn has_forwarded(&self) -> bool {
        self.forwarded_message().is_some()
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<forwarded/>` element.
pub fn is_forwarded_element(elem: &Element) -> bool {
    elem.ns() == NS_FORWARD && elem.name() == "forwarded"
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<forwarded/>` element containing a message and optional delay.
pub fn build_forwarded_element(fwd: &ForwardedMessage) -> Element {
    let mut builder = Element::builder("forwarded", NS_FORWARD);

    if let Some(stamp) = fwd.stamp {
        let mut delay_builder =
            Element::builder("delay", NS_DELAY).attr("stamp", stamp.to_rfc3339());
        if let Some(ref from) = fwd.delay_from {
            delay_builder = delay_builder.attr("from", from.as_str());
        }
        builder = builder.append(delay_builder.build());
    }

    let msg_elem: Element = fwd.message.clone().into();
    builder = builder.append(msg_elem);

    builder.build()
}

/// Build a simple forwarded element with current timestamp (no delay from).
///
/// Convenience for carbons and similar use cases.
pub fn build_forwarded_now(message: &Message) -> Element {
    build_forwarded_element(&ForwardedMessage::new(message.clone()))
}

/// Build a forwarded element with a specific timestamp and from entity.
///
/// Convenience for MAM results.
pub fn build_forwarded_with_delay(message: &Message, stamp: DateTime<Utc>, from: &str) -> Element {
    build_forwarded_element(
        &ForwardedMessage::with_stamp(message.clone(), stamp).with_delay_from(from),
    )
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract a forwarded message from a `<forwarded/>` element.
pub fn parse_forwarded_element(elem: &Element) -> Option<ForwardedMessage> {
    if !is_forwarded_element(elem) {
        return None;
    }

    // Extract the inner message
    let msg_elem = elem.children().find(|c| c.name() == "message")?;
    let message = Message::try_from(msg_elem.clone()).ok()?;

    // Extract optional delay
    let delay_elem = elem
        .children()
        .find(|c| c.name() == "delay" && c.ns() == NS_DELAY);
    let stamp = delay_elem
        .and_then(|d| d.attr("stamp"))
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());
    let delay_from = delay_elem
        .and_then(|d| d.attr("from"))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    Some(ForwardedMessage {
        message,
        stamp,
        delay_from,
    })
}

/// Extract a forwarded message from a message's payloads.
///
/// Looks for a `<forwarded/>` element among the message's child payloads.
pub fn extract_forwarded_from_message(msg: &Message) -> Option<ForwardedMessage> {
    msg.payloads
        .iter()
        .find(|e| is_forwarded_element(e))
        .and_then(parse_forwarded_element)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use xmpp_parsers::message::{Body, MessageType};

    fn test_stamp() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    fn make_test_message() -> Message {
        let mut msg = Message::new(Some(
            "juliet@example.com".parse::<jid::Jid>().expect("valid jid"),
        ));
        msg.from = Some("romeo@example.com".parse::<jid::Jid>().expect("valid jid"));
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(String::new(), Body("Hello!".to_string()));
        msg
    }

    #[test]
    fn test_is_forwarded_element() {
        let elem = Element::builder("forwarded", NS_FORWARD).build();
        assert!(is_forwarded_element(&elem));

        let wrong = Element::builder("forwarded", "jabber:client").build();
        assert!(!is_forwarded_element(&wrong));
    }

    #[test]
    fn test_build_forwarded_element() {
        let msg = make_test_message();
        let fwd = ForwardedMessage::with_stamp(msg, test_stamp()).with_delay_from("example.com");
        let elem = build_forwarded_element(&fwd);

        assert_eq!(elem.name(), "forwarded");
        assert_eq!(elem.ns(), NS_FORWARD);

        // Has delay child
        let delay = elem
            .children()
            .find(|c| c.name() == "delay")
            .expect("delay present");
        assert!(delay.attr("stamp").is_some());
        assert_eq!(delay.attr("from"), Some("example.com"));

        // Has message child
        let inner = elem
            .children()
            .find(|c| c.name() == "message")
            .expect("message present");
        assert!(inner.attr("from").is_some());
    }

    #[test]
    fn test_build_forwarded_now() {
        let msg = make_test_message();
        let elem = build_forwarded_now(&msg);

        assert_eq!(elem.name(), "forwarded");
        assert!(elem.children().any(|c| c.name() == "delay"));
        assert!(elem.children().any(|c| c.name() == "message"));
    }

    #[test]
    fn test_build_forwarded_with_delay() {
        let msg = make_test_message();
        let elem = build_forwarded_with_delay(&msg, test_stamp(), "muc.example.com");

        let delay = elem
            .children()
            .find(|c| c.name() == "delay")
            .expect("delay");
        assert_eq!(delay.attr("from"), Some("muc.example.com"));
    }

    #[test]
    fn test_parse_forwarded_element() {
        let msg = make_test_message();
        let fwd = ForwardedMessage::with_stamp(msg, test_stamp()).with_delay_from("example.com");
        let elem = build_forwarded_element(&fwd);

        let parsed = parse_forwarded_element(&elem).expect("parseable");
        assert_eq!(parsed.stamp, Some(test_stamp()));
        assert_eq!(parsed.delay_from.as_deref(), Some("example.com"));
        assert_eq!(
            parsed.message.bodies.get("").map(|b| b.0.as_str()),
            Some("Hello!")
        );
    }

    #[test]
    fn test_parse_forwarded_no_delay() {
        let msg = make_test_message();
        let msg_elem: Element = msg.into();
        let elem = Element::builder("forwarded", NS_FORWARD)
            .append(msg_elem)
            .build();

        let parsed = parse_forwarded_element(&elem).expect("parseable");
        assert_eq!(parsed.stamp, None);
        assert_eq!(parsed.delay_from, None);
    }

    #[test]
    fn test_parse_forwarded_wrong_ns() {
        let elem = Element::builder("forwarded", "jabber:client").build();
        assert!(parse_forwarded_element(&elem).is_none());
    }

    #[test]
    fn test_parse_forwarded_no_message() {
        let elem = Element::builder("forwarded", NS_FORWARD)
            .append(
                Element::builder("delay", NS_DELAY)
                    .attr("stamp", "2024-01-01T00:00:00Z")
                    .build(),
            )
            .build();
        assert!(parse_forwarded_element(&elem).is_none());
    }

    #[test]
    fn test_forwarded_message_builder() {
        let msg = make_test_message();
        let fwd = ForwardedMessage::new(msg.clone());
        assert!(fwd.stamp.is_some());
        assert!(fwd.delay_from.is_none());

        let fwd2 =
            ForwardedMessage::with_stamp(msg, test_stamp()).with_delay_from("srv.example.com");
        assert_eq!(fwd2.delay_from.as_deref(), Some("srv.example.com"));
    }

    #[test]
    fn test_roundtrip() {
        let msg = make_test_message();
        let original =
            ForwardedMessage::with_stamp(msg, test_stamp()).with_delay_from("room@muc.example.com");
        let elem = build_forwarded_element(&original);
        let parsed = parse_forwarded_element(&elem).expect("roundtrip");

        assert_eq!(parsed.stamp, original.stamp);
        assert_eq!(parsed.delay_from, original.delay_from);
        assert_eq!(
            parsed.message.bodies.get("").map(|b| b.0.as_str()),
            Some("Hello!")
        );
    }
}
