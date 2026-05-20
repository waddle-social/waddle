//! XEP-0172: User Nickname
//!
//! Provides helpers for detecting, parsing, and building user nickname
//! elements. A nickname is a global, memorable, friendly name chosen
//! by the user as their preferred display name.
//!
//! ## XML Format
//!
//! In a message:
//! ```xml
//! <message from='romeo@example.com' to='juliet@example.com'>
//!   <body>Hello!</body>
//!   <nick xmlns='http://jabber.org/protocol/nick'>Romeo Montague</nick>
//! </message>
//! ```
//!
//! In presence:
//! ```xml
//! <presence from='romeo@example.com'>
//!   <nick xmlns='http://jabber.org/protocol/nick'>Romeo Montague</nick>
//! </presence>
//! ```
//!
//! ## Use Cases
//!
//! - Display a human-friendly name instead of a bare JID
//! - Included in MUC presence for room occupant display
//! - Published via PEP for contact list display
//! - Included in message stanzas for first-contact identification

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0172 User Nickname.
pub const NS_NICK: &str = "http://jabber.org/protocol/nick";

/// A user's chosen display nickname.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Nickname(pub String);

impl Nickname {
    /// Create a new nickname.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Get the nickname text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `true` if the nickname is empty or whitespace-only.
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl std::fmt::Display for Nickname {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Nickname {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Nickname {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// Trait for types that can carry a user nickname.
pub trait NicknameCarrier {
    /// Extract the nickname from this carrier, if present.
    fn nickname(&self) -> Option<Nickname>;

    /// Returns `true` if this carrier has a nickname element.
    fn has_nickname(&self) -> bool {
        self.nickname().is_some()
    }
}

impl NicknameCarrier for Message {
    fn nickname(&self) -> Option<Nickname> {
        extract_nickname_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<nick/>` element.
pub fn is_nick_element(elem: &Element) -> bool {
    elem.ns() == NS_NICK && elem.name() == "nick"
}

/// Check if a message contains a nickname element.
pub fn has_nick(msg: &Message) -> bool {
    msg.payloads.iter().any(is_nick_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract the nickname from a message's payloads.
pub fn extract_nickname_from_message(msg: &Message) -> Option<Nickname> {
    msg.payloads
        .iter()
        .find(|e| is_nick_element(e))
        .map(|e| e.text())
        .filter(|text| !text.trim().is_empty())
        .map(Nickname)
}

/// Extract the nickname from a presence stanza's payloads.
pub fn extract_nickname_from_presence(
    presence: &xmpp_parsers::presence::Presence,
) -> Option<Nickname> {
    presence
        .payloads
        .iter()
        .find(|e| is_nick_element(e))
        .map(|e| e.text())
        .filter(|text| !text.trim().is_empty())
        .map(Nickname)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<nick xmlns='http://jabber.org/protocol/nick'>...</nick>` element.
pub fn build_nick_element(nickname: &str) -> Element {
    let mut elem = Element::builder("nick", NS_NICK).build();
    elem.append_text_node(nickname);
    elem
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add or replace the nickname on a message.
pub fn set_nickname(msg: &mut Message, nickname: &str) {
    msg.payloads.retain(|e| e.ns() != NS_NICK);
    msg.payloads.push(build_nick_element(nickname));
}

/// Remove the nickname from a message.
pub fn strip_nickname(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_NICK);
}

// ── Conversion ───────────────────────────────────────────────────────

impl From<xmpp_parsers::nick::Nick> for Nickname {
    fn from(nick: xmpp_parsers::nick::Nick) -> Self {
        Self(nick.0)
    }
}

impl From<Nickname> for xmpp_parsers::nick::Nick {
    fn from(nick: Nickname) -> Self {
        Self(nick.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::Message;

    #[test]
    fn test_is_nick_element() {
        let elem = Element::builder("nick", NS_NICK).build();
        assert!(is_nick_element(&elem));

        let wrong_ns = Element::builder("nick", "jabber:client").build();
        assert!(!is_nick_element(&wrong_ns));

        let wrong_name = Element::builder("nickname", NS_NICK).build();
        assert!(!is_nick_element(&wrong_name));
    }

    #[test]
    fn test_extract_nickname_from_message() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Hello</body>\
                    <nick xmlns='http://jabber.org/protocol/nick'>Romeo Montague</nick>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let nick = extract_nickname_from_message(&msg).expect("has nick");
        assert_eq!(nick.as_str(), "Romeo Montague");
    }

    #[test]
    fn test_extract_nickname_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_nickname_from_message(&msg).is_none());
    }

    #[test]
    fn test_extract_nickname_empty_ignored() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <nick xmlns='http://jabber.org/protocol/nick'>   </nick>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(extract_nickname_from_message(&msg).is_none());
    }

    #[test]
    fn test_build_nick_element() {
        let elem = build_nick_element("Juliet Capulet");
        assert_eq!(elem.name(), "nick");
        assert_eq!(elem.ns(), NS_NICK);
        assert_eq!(elem.text(), "Juliet Capulet");
    }

    #[test]
    fn test_set_nickname() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.bodies
            .insert(xmpp_parsers::message::Lang::new(), "Hi".to_string());
        set_nickname(&mut msg, "Romeo");

        assert!(has_nick(&msg));
        assert_eq!(
            extract_nickname_from_message(&msg).map(|n| n.0),
            Some("Romeo".to_owned())
        );

        // Replace
        set_nickname(&mut msg, "Updated Romeo");
        assert_eq!(
            extract_nickname_from_message(&msg).map(|n| n.0),
            Some("Updated Romeo".to_owned())
        );
        assert_eq!(msg.payloads.iter().filter(|e| e.ns() == NS_NICK).count(), 1);
    }

    #[test]
    fn test_strip_nickname() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Hi</body>\
                    <nick xmlns='http://jabber.org/protocol/nick'>Romeo</nick>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        strip_nickname(&mut msg);
        assert!(!has_nick(&msg));
        assert!(!msg.bodies.is_empty());
    }

    #[test]
    fn test_nickname_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <nick xmlns='http://jabber.org/protocol/nick'>Juliet</nick>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_nickname());
        assert_eq!(msg.nickname(), Some(Nickname::new("Juliet")));

        let plain = Message::new(None::<jid::Jid>);
        assert!(!plain.has_nickname());
    }

    #[test]
    fn test_nickname_display() {
        let nick = Nickname::new("Romeo");
        assert_eq!(nick.to_string(), "Romeo");
    }

    #[test]
    fn test_nickname_is_empty() {
        assert!(Nickname::new("").is_empty());
        assert!(Nickname::new("  ").is_empty());
        assert!(!Nickname::new("Romeo").is_empty());
    }

    #[test]
    fn test_nickname_conversions() {
        let nick = Nickname::from("Romeo");
        assert_eq!(nick.as_str(), "Romeo");

        let nick2 = Nickname::from(String::from("Juliet"));
        assert_eq!(nick2.as_str(), "Juliet");
    }

    #[test]
    fn test_roundtrip_conversion() {
        let nick = Nickname::new("Test");
        let parser_nick: xmpp_parsers::nick::Nick = nick.clone().into();
        assert_eq!(parser_nick.0, "Test");
        let back: Nickname = parser_nick.into();
        assert_eq!(back, nick);
    }

    #[test]
    fn test_extract_from_presence() {
        let xml = "<presence xmlns='jabber:client'>\
                    <nick xmlns='http://jabber.org/protocol/nick'>Romeo</nick>\
                    </presence>";
        let elem: Element = xml.parse().expect("valid xml");
        let presence = xmpp_parsers::presence::Presence::try_from(elem).expect("valid presence");

        let nick = extract_nickname_from_presence(&presence).expect("has nick");
        assert_eq!(nick.as_str(), "Romeo");
    }
}
