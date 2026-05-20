//! XEP-0203: Delayed Delivery
//!
//! Provides helpers for detecting, parsing, and building delay elements.
//! A `<delay/>` element indicates when and by whom a message was stored
//! for later delivery (e.g., offline messages, MUC history, MAM results).
//!
//! ## XML Format
//!
//! ```xml
//! <message from='room@muc.example.com/nick' type='groupchat'>
//!   <body>Hello</body>
//!   <delay xmlns='urn:xmpp:delay'
//!          from='room@muc.example.com'
//!          stamp='2024-01-15T12:00:00Z'/>
//! </message>
//! ```
//!
//! With optional reason text:
//! ```xml
//! <delay xmlns='urn:xmpp:delay'
//!        from='example.com'
//!        stamp='2024-01-15T12:00:00Z'>Offline storage</delay>
//! ```
//!
//! ## Use Cases
//!
//! - MUC room history on join
//! - MAM archived message results
//! - Offline message delivery
//! - Message forwarding (XEP-0297)

use chrono::{DateTime, SecondsFormat, Utc};
use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0203 Delayed Delivery.
pub const NS_DELAY: &str = "urn:xmpp:delay";

/// Errors that can occur when parsing delay elements.
#[derive(Debug, Error)]
pub enum DelayError {
    /// The `stamp` attribute is missing or invalid.
    #[error("delay element has invalid or missing stamp: {0}")]
    InvalidStamp(String),
}

/// A delay annotation indicating when/why a message was held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelayInfo {
    /// The entity that delayed the message (server, MUC service, etc.).
    pub from: Option<String>,
    /// The timestamp when the message was originally sent or stored.
    pub stamp: DateTime<Utc>,
    /// Optional reason for the delay.
    pub reason: Option<String>,
}

impl DelayInfo {
    /// Create a new delay info with just a timestamp.
    pub fn new(stamp: DateTime<Utc>) -> Self {
        Self {
            from: None,
            stamp,
            reason: None,
        }
    }

    /// Set the `from` entity.
    pub fn with_from(mut self, from: impl Into<String>) -> Self {
        self.from = Some(from.into());
        self
    }

    /// Set the reason text.
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Trait for types that can carry delay information.
pub trait DelayCarrier {
    /// Extract the delay info from this carrier, if present.
    fn delay(&self) -> Option<DelayInfo>;

    /// Returns `true` if this carrier has a delay element.
    fn is_delayed(&self) -> bool {
        self.delay().is_some()
    }

    /// Returns the delay timestamp if present.
    fn delay_stamp(&self) -> Option<DateTime<Utc>> {
        self.delay().map(|d| d.stamp)
    }
}

impl DelayCarrier for Message {
    fn delay(&self) -> Option<DelayInfo> {
        extract_delay_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<delay/>` element.
pub fn is_delay_element(elem: &Element) -> bool {
    elem.ns() == NS_DELAY && elem.name() == "delay"
}

/// Check if a message has a delay element.
pub fn has_delay(msg: &Message) -> bool {
    msg.payloads.iter().any(is_delay_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract delay info from a message.
pub fn extract_delay_from_message(msg: &Message) -> Option<DelayInfo> {
    let elem = msg.payloads.iter().find(|e| is_delay_element(e))?;
    parse_delay_element(elem).ok()
}

/// Parse a `<delay/>` element into `DelayInfo`.
pub fn parse_delay_element(elem: &Element) -> Result<DelayInfo, DelayError> {
    let stamp_str = elem
        .attr("stamp")
        .ok_or_else(|| DelayError::InvalidStamp("missing stamp attribute".into()))?;

    let stamp = stamp_str
        .parse::<DateTime<Utc>>()
        .map_err(|e| DelayError::InvalidStamp(e.to_string()))?;

    let from = elem
        .attr("from")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    let text = elem.text();
    let reason = if text.is_empty() { None } else { Some(text) };

    Ok(DelayInfo {
        from,
        stamp,
        reason,
    })
}

/// Extract the delay timestamp from a message.
pub fn extract_delay_stamp(msg: &Message) -> Option<DateTime<Utc>> {
    extract_delay_from_message(msg).map(|d| d.stamp)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<delay/>` element.
///
/// XEP-0082 §3.2 BNF requires UTC datetimes use the literal `Z` suffix
/// (e.g. `2002-09-10T23:08:25Z`), not the `+00:00` form. `chrono`'s
/// default `to_rfc3339` emits `+00:00` for `DateTime<Utc>`, so we use
/// the explicit `to_rfc3339_opts(_, true)` to force the `Z` form for
/// strict-client compatibility (Conversations, gajim, Movim all expect
/// the canonical XEP-0082 stamp).
pub fn build_delay_element(info: &DelayInfo) -> Element {
    let stamp = info.stamp.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut builder = Element::builder("delay", NS_DELAY)
        .attr(minidom::rxml::xml_ncname!("stamp").to_owned(), stamp);

    if let Some(ref from) = info.from {
        builder = builder.attr(minidom::rxml::xml_ncname!("from").to_owned(), from.as_str());
    }

    let mut elem = builder.build();

    if let Some(ref reason) = info.reason {
        elem.append_text_node(reason);
    }

    elem
}

/// Build a simple delay element with just a timestamp and from.
pub fn build_delay_element_simple(stamp: DateTime<Utc>, from: &str) -> Element {
    Element::builder("delay", NS_DELAY)
        .attr(
            minidom::rxml::xml_ncname!("stamp").to_owned(),
            stamp.to_rfc3339_opts(SecondsFormat::Secs, true),
        )
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), from)
        .build()
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a delay element to a message.
pub fn add_delay(msg: &mut Message, info: &DelayInfo) {
    msg.payloads.push(build_delay_element(info));
}

/// Add a simple delay with timestamp and from entity.
pub fn add_delay_stamp(msg: &mut Message, stamp: DateTime<Utc>, from: &str) {
    msg.payloads.push(build_delay_element_simple(stamp, from));
}

/// Remove all delay elements from a message.
pub fn strip_delay(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_DELAY);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use xmpp_parsers::message::Message;

    fn utc(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, min, sec)
            .unwrap()
    }

    #[test]
    fn test_is_delay_element() {
        let elem = Element::builder("delay", NS_DELAY)
            .attr(
                minidom::rxml::xml_ncname!("stamp").to_owned(),
                "2024-01-15T12:00:00Z",
            )
            .build();
        assert!(is_delay_element(&elem));

        let wrong = Element::builder("delay", "jabber:client").build();
        assert!(!is_delay_element(&wrong));
    }

    #[test]
    fn test_extract_delay_full() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <delay xmlns='urn:xmpp:delay' from='room@muc.example.com' stamp='2024-06-01T09:30:00Z'>Offline</delay>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let delay = extract_delay_from_message(&msg).expect("has delay");
        assert_eq!(delay.from.as_deref(), Some("room@muc.example.com"));
        assert_eq!(delay.stamp, utc(2024, 6, 1, 9, 30, 0));
        assert_eq!(delay.reason.as_deref(), Some("Offline"));
    }

    #[test]
    fn test_extract_delay_minimal() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Hi</body>\
                    <delay xmlns='urn:xmpp:delay' stamp='2024-01-15T12:00:00Z'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let delay = extract_delay_from_message(&msg).expect("has delay");
        assert_eq!(delay.from, None);
        assert_eq!(delay.stamp, utc(2024, 1, 15, 12, 0, 0));
        assert_eq!(delay.reason, None);
    }

    #[test]
    fn test_extract_delay_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_delay_from_message(&msg).is_none());
    }

    #[test]
    fn test_extract_delay_stamp() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <delay xmlns='urn:xmpp:delay' stamp='2024-03-20T15:45:00Z'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert_eq!(extract_delay_stamp(&msg), Some(utc(2024, 3, 20, 15, 45, 0)));
    }

    #[test]
    fn test_parse_delay_element_invalid_stamp() {
        let elem = Element::builder("delay", NS_DELAY)
            .attr(minidom::rxml::xml_ncname!("stamp").to_owned(), "not-a-date")
            .build();
        let err = parse_delay_element(&elem).expect_err("should fail");
        assert!(err.to_string().contains("invalid"));
    }

    #[test]
    fn test_parse_delay_element_missing_stamp() {
        let elem = Element::builder("delay", NS_DELAY).build();
        let err = parse_delay_element(&elem).expect_err("should fail");
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn test_build_delay_element_full() {
        let info = DelayInfo::new(utc(2024, 6, 1, 9, 0, 0))
            .with_from("example.com")
            .with_reason("Offline storage");
        let elem = build_delay_element(&info);

        assert_eq!(elem.name(), "delay");
        assert_eq!(elem.ns(), NS_DELAY);
        assert!(elem.attr("stamp").is_some());
        assert_eq!(elem.attr("from"), Some("example.com"));
        assert_eq!(elem.text(), "Offline storage");
    }

    #[test]
    fn test_build_delay_element_minimal() {
        let info = DelayInfo::new(utc(2024, 1, 1, 0, 0, 0));
        let elem = build_delay_element(&info);

        assert_eq!(elem.attr("from"), None);
        assert_eq!(elem.text(), "");
    }

    #[test]
    fn test_build_delay_element_simple() {
        let elem = build_delay_element_simple(utc(2024, 6, 1, 12, 0, 0), "muc.example.com");
        assert_eq!(elem.attr("from"), Some("muc.example.com"));
        assert!(elem.attr("stamp").is_some());
    }

    #[test]
    fn test_add_delay() {
        let mut msg = Message::new(None::<jid::Jid>);
        let info = DelayInfo::new(utc(2024, 1, 1, 0, 0, 0)).with_from("example.com");
        add_delay(&mut msg, &info);

        assert!(has_delay(&msg));
        let extracted = extract_delay_from_message(&msg).expect("has delay");
        assert_eq!(extracted.from.as_deref(), Some("example.com"));
    }

    #[test]
    fn test_add_delay_stamp() {
        let mut msg = Message::new(None::<jid::Jid>);
        add_delay_stamp(&mut msg, utc(2024, 6, 1, 12, 0, 0), "room@muc.example.com");

        assert!(has_delay(&msg));
    }

    #[test]
    fn test_strip_delay() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Hello</body>\
                    <delay xmlns='urn:xmpp:delay' stamp='2024-01-15T12:00:00Z'/>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        strip_delay(&mut msg);
        assert!(!has_delay(&msg));
        assert!(!msg.bodies.is_empty());
    }

    #[test]
    fn test_delay_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>History</body>\
                    <delay xmlns='urn:xmpp:delay' from='room@muc.example.com' stamp='2024-06-01T09:00:00Z'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.is_delayed());
        assert_eq!(msg.delay_stamp(), Some(utc(2024, 6, 1, 9, 0, 0)));
        let info = msg.delay().expect("has delay");
        assert_eq!(info.from.as_deref(), Some("room@muc.example.com"));
    }

    #[test]
    fn test_delay_info_builder() {
        let info = DelayInfo::new(utc(2024, 1, 1, 0, 0, 0))
            .with_from("srv.example.com")
            .with_reason("stored offline");
        assert_eq!(info.from.as_deref(), Some("srv.example.com"));
        assert_eq!(info.reason.as_deref(), Some("stored offline"));
    }

    #[test]
    fn test_has_delay() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <delay xmlns='urn:xmpp:delay' stamp='2024-01-01T00:00:00Z'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(has_delay(&msg));

        let plain = Message::new(None::<jid::Jid>);
        assert!(!has_delay(&plain));
    }
}
