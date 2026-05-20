//! XEP-0333: Displayed Markers
//!
//! Provides helpers for detecting, parsing, and building displayed marker
//! elements within XMPP message stanzas. Displayed markers allow entities
//! to signal that messages have been displayed up to a certain point.
//!
//! ## Marker Types
//!
//! - **`<markable/>`**: Included in outgoing messages to request displayed markers.
//! - **`<displayed id='...'/>`**: The message identified by `id` was displayed.
//!
//! ## XML Format
//!
//! Request markers on an outgoing message:
//! ```xml
//! <message type='chat' to='romeo@example.com' id='msg-1'>
//!   <body>Hello!</body>
//!   <markable xmlns='urn:xmpp:chat-markers:0'/>
//! </message>
//! ```
//!
//! Send a displayed marker:
//! ```xml
//! <message type='chat' to='juliet@example.com'>
//!   <displayed xmlns='urn:xmpp:chat-markers:0' id='msg-1'/>
//! </message>
//! ```
//!
//! ## Server Behavior
//!
//! The server transparently routes displayed marker messages. Standalone markers
//! (body-less messages with only a `<displayed/>` element) should not be archived
//! but must be routed to the recipient.

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0333 Displayed Markers.
pub const NS_CHAT_MARKERS: &str = "urn:xmpp:chat-markers:0";

/// Errors that can occur when parsing displayed marker elements.
#[derive(Debug, Error)]
pub enum MarkerError {
    /// A marker element is missing its required `id` attribute.
    #[error("marker element missing id attribute")]
    MissingId,
}

/// Displayed marker types defined by XEP-0333.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Marker {
    /// Request that the recipient send displayed markers for this message.
    Markable,
    /// The message was displayed to the user.
    Displayed(String),
}

impl Marker {
    /// Returns the XML element name for this marker.
    pub fn element_name(&self) -> &'static str {
        match self {
            Self::Markable => "markable",
            Self::Displayed(_) => "displayed",
        }
    }

    /// Returns the referenced message id, if this is a displayed marker.
    pub fn referenced_id(&self) -> Option<&str> {
        match self {
            Self::Markable => None,
            Self::Displayed(id) => Some(id),
        }
    }

    /// Returns `true` if this is a `<markable/>` request.
    pub fn is_markable(&self) -> bool {
        matches!(self, Self::Markable)
    }

    /// Returns `true` if this is a `<displayed/>` marker.
    pub fn is_displayed(&self) -> bool {
        matches!(self, Self::Displayed(_))
    }
}

/// Trait for types that can carry displayed marker elements.
pub trait MarkerCarrier {
    /// Extract the marker from this carrier, if present.
    fn marker(&self) -> Option<Marker>;

    /// Returns `true` if this carrier requests markers (`<markable/>`).
    fn is_markable(&self) -> bool {
        matches!(self.marker(), Some(Marker::Markable))
    }

    /// Returns `true` if this carrier is a standalone marker (no body).
    fn is_standalone_marker(&self) -> bool;

    /// Returns the referenced message id if this is a displayed marker.
    fn marker_ref_id(&self) -> Option<String> {
        self.marker()
            .and_then(|m| m.referenced_id().map(|s| s.to_owned()))
    }
}

impl MarkerCarrier for Message {
    fn marker(&self) -> Option<Marker> {
        extract_marker_from_message(self)
    }

    fn is_standalone_marker(&self) -> bool {
        is_standalone_marker(self)
    }
}

fn message_has_id(msg: &Message) -> bool {
    msg.id
        .as_ref()
        .map(|id| id.0.as_str())
        .is_some_and(|id| !id.is_empty())
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a displayed marker element.
pub fn is_marker_element(elem: &Element) -> bool {
    elem.ns() == NS_CHAT_MARKERS && matches!(elem.name(), "markable" | "displayed")
}

/// Check if a message contains any displayed marker element.
pub fn has_marker(msg: &Message) -> bool {
    msg.payloads.iter().any(is_marker_element)
}

/// Check if a message validly requests displayed markers (`<markable/>`).
pub fn has_markable(msg: &Message) -> bool {
    message_has_id(msg)
        && msg
            .payloads
            .iter()
            .any(|e| e.ns() == NS_CHAT_MARKERS && e.name() == "markable")
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract the displayed marker from a message's payloads.
///
/// Prefers `<displayed/>` over `<markable/>` when both are present.
pub fn extract_marker_from_message(msg: &Message) -> Option<Marker> {
    let mut markable = false;

    for elem in &msg.payloads {
        if elem.ns() != NS_CHAT_MARKERS {
            continue;
        }
        match elem.name() {
            "markable" => markable = true,
            "displayed" => {
                let id = elem.attr("id").unwrap_or("").to_owned();
                if id.is_empty() {
                    continue;
                }
                return Some(Marker::Displayed(id));
            }
            _ => {}
        }
    }

    if markable && message_has_id(msg) {
        return Some(Marker::Markable);
    }
    None
}

/// Extract the referenced message id from a displayed marker.
pub fn extract_marker_id(msg: &Message) -> Option<String> {
    extract_marker_from_message(msg).and_then(|m| m.referenced_id().map(|s| s.to_owned()))
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<markable xmlns='urn:xmpp:chat-markers:0'/>` element.
pub fn build_markable_element() -> Element {
    Element::builder("markable", NS_CHAT_MARKERS).build()
}

/// Build a `<displayed xmlns='urn:xmpp:chat-markers:0' id='...'>` element.
pub fn build_displayed_element(id: &str) -> Element {
    Element::builder("displayed", NS_CHAT_MARKERS)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .build()
}

/// Build a standalone displayed marker message.
pub fn build_displayed_message(
    to: impl Into<Option<jid::Jid>>,
    from: impl Into<Option<jid::Jid>>,
    original_id: &str,
) -> Message {
    let mut msg = Message::new(to.into());
    msg.from = from.into();
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.payloads.push(build_displayed_element(original_id));
    msg
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a `<markable/>` element to a message when it has a traceable `id`.
pub fn add_markable(msg: &mut Message) {
    if message_has_id(msg) && !has_markable(msg) {
        msg.payloads.push(build_markable_element());
    }
}

/// Remove all displayed marker elements from a message.
pub fn strip_markers(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_CHAT_MARKERS);
}

/// Check if a message is a standalone displayed marker (no body).
/// These should be routed but not archived.
pub fn is_standalone_marker(msg: &Message) -> bool {
    msg.bodies.is_empty()
        && msg.payloads.iter().any(|e| {
            e.ns() == NS_CHAT_MARKERS
                && e.name() == "displayed"
                && e.attr("id").is_some_and(|id| !id.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Message, MessageType};

    #[test]
    fn test_is_marker_element() {
        for name in ["markable", "displayed"] {
            let elem = Element::builder(name, NS_CHAT_MARKERS).build();
            assert!(is_marker_element(&elem), "failed for {name}");
        }

        for name in ["received", "acknowledged", "read"] {
            let elem = Element::builder(name, NS_CHAT_MARKERS).build();
            assert!(!is_marker_element(&elem), "unexpected marker for {name}");
        }

        let wrong_ns = Element::builder("displayed", "jabber:client").build();
        assert!(!is_marker_element(&wrong_ns));
    }

    #[test]
    fn test_has_markable() {
        let xml = "<message xmlns='jabber:client' type='chat' id='msg-1'>\
                    <body>Hello</body>\
                    <markable xmlns='urn:xmpp:chat-markers:0'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(has_markable(&msg));
        assert!(has_marker(&msg));
    }

    #[test]
    fn test_markable_without_message_id_is_ignored() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <markable xmlns='urn:xmpp:chat-markers:0'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(!has_markable(&msg));
        assert_eq!(extract_marker_from_message(&msg), None);
    }

    #[test]
    fn test_extract_markable() {
        let xml = "<message xmlns='jabber:client' type='chat' id='msg-1'>\
                    <body>Hello</body>\
                    <markable xmlns='urn:xmpp:chat-markers:0'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert_eq!(extract_marker_from_message(&msg), Some(Marker::Markable));
        assert_eq!(extract_marker_id(&msg), None);
    }

    #[test]
    fn test_extract_displayed() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <displayed xmlns='urn:xmpp:chat-markers:0' id='msg-42'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let marker = extract_marker_from_message(&msg).expect("has marker");
        assert_eq!(marker, Marker::Displayed("msg-42".to_owned()));
        assert!(marker.is_displayed());
        assert_eq!(marker.referenced_id(), Some("msg-42"));
        assert_eq!(extract_marker_id(&msg), Some("msg-42".to_owned()));
    }

    #[test]
    fn test_extract_none() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_marker_from_message(&msg).is_none());
    }

    #[test]
    fn test_extract_empty_displayed_id_ignored() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <displayed xmlns='urn:xmpp:chat-markers:0' id=''/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(extract_marker_from_message(&msg).is_none());
    }

    #[test]
    fn test_displayed_marker_preferred_over_markable() {
        let xml = "<message xmlns='jabber:client' type='chat' id='msg-0'>\
                    <markable xmlns='urn:xmpp:chat-markers:0'/>\
                    <displayed xmlns='urn:xmpp:chat-markers:0' id='msg-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert_eq!(
            extract_marker_from_message(&msg),
            Some(Marker::Displayed("msg-1".to_owned()))
        );
    }

    #[test]
    fn test_build_markable() {
        let elem = build_markable_element();
        assert_eq!(elem.name(), "markable");
        assert_eq!(elem.ns(), NS_CHAT_MARKERS);
    }

    #[test]
    fn test_build_displayed() {
        let elem = build_displayed_element("msg-99");
        assert_eq!(elem.name(), "displayed");
        assert_eq!(elem.ns(), NS_CHAT_MARKERS);
        assert_eq!(elem.attr("id"), Some("msg-99"));
    }

    #[test]
    fn test_build_displayed_message() {
        let to: jid::Jid = "juliet@example.com".parse().expect("valid jid");
        let from: jid::Jid = "romeo@example.com".parse().expect("valid jid");
        let msg = build_displayed_message(to.clone(), from.clone(), "msg-1");

        assert_eq!(msg.to, Some(to));
        assert_eq!(msg.from, Some(from));
        assert_eq!(msg.type_, MessageType::Chat);
        assert!(msg.bodies.is_empty());
        assert_eq!(
            extract_marker_from_message(&msg),
            Some(Marker::Displayed("msg-1".to_owned()))
        );
    }

    #[test]
    fn test_add_markable_requires_message_id() {
        let mut msg = Message::new(None::<jid::Jid>);
        add_markable(&mut msg);
        assert!(!has_markable(&msg));
        assert!(msg.payloads.is_empty());

        msg.id = Some(xmpp_parsers::message::Id("msg-1".to_string()));
        add_markable(&mut msg);
        assert!(has_markable(&msg));

        add_markable(&mut msg);
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|e| e.ns() == NS_CHAT_MARKERS)
                .count(),
            1
        );
    }

    #[test]
    fn test_strip_markers() {
        let xml = "<message xmlns='jabber:client' type='chat' id='msg-1'>\
                    <body>Hello</body>\
                    <markable xmlns='urn:xmpp:chat-markers:0'/>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        strip_markers(&mut msg);
        assert!(!has_marker(&msg));
        assert!(!msg.bodies.is_empty());
    }

    #[test]
    fn test_is_standalone_marker() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <displayed xmlns='urn:xmpp:chat-markers:0' id='msg-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(is_standalone_marker(&msg));

        let invalid_xml = "<message xmlns='jabber:client' type='chat'>\
                            <displayed xmlns='urn:xmpp:chat-markers:0' id=''/>\
                            </message>";
        let invalid = Message::try_from(invalid_xml.parse::<Element>().expect("valid xml"))
            .expect("valid message");
        assert!(!is_standalone_marker(&invalid));

        let xml2 = "<message xmlns='jabber:client' type='chat' id='msg-1'>\
                     <body>Hello</body>\
                     <markable xmlns='urn:xmpp:chat-markers:0'/>\
                     </message>";
        let msg2 =
            Message::try_from(xml2.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(!is_standalone_marker(&msg2));
    }

    #[test]
    fn test_marker_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='chat' id='m1'>\
                    <body>Hi</body>\
                    <markable xmlns='urn:xmpp:chat-markers:0'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(msg.is_markable());
        assert!(!msg.is_standalone_marker());
        assert_eq!(msg.marker_ref_id(), None);

        let xml2 = "<message xmlns='jabber:client' type='chat'>\
                     <displayed xmlns='urn:xmpp:chat-markers:0' id='m1'/>\
                     </message>";
        let msg2 =
            Message::try_from(xml2.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(!msg2.is_markable());
        assert!(msg2.is_standalone_marker());
        assert_eq!(msg2.marker_ref_id(), Some("m1".to_owned()));
    }

    #[test]
    fn test_marker_element_name() {
        assert_eq!(Marker::Markable.element_name(), "markable");
        assert_eq!(Marker::Displayed("x".into()).element_name(), "displayed");
    }
}
