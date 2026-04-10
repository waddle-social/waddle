//! XEP-0085: Chat State Notifications
//!
//! Provides helpers for detecting, parsing, and building chat state notification
//! elements within XMPP message stanzas.
//!
//! ## Chat States
//!
//! Five chat states are defined:
//! - **Active**: User is participating in the conversation
//! - **Composing**: User is actively typing a message
//! - **Paused**: User was composing but stopped
//! - **Inactive**: User has not interacted recently
//! - **Gone**: User has left the conversation
//!
//! ## XML Format
//!
//! Chat state elements are empty child elements within `<message/>` stanzas:
//! ```xml
//! <message type='chat' to='romeo@example.com'>
//!   <composing xmlns='http://jabber.org/protocol/chatstates'/>
//! </message>
//! ```
//!
//! A message with body text SHOULD also include an `<active/>` state:
//! ```xml
//! <message type='chat' to='romeo@example.com'>
//!   <body>Hello!</body>
//!   <active xmlns='http://jabber.org/protocol/chatstates'/>
//! </message>
//! ```
//!
//! ## Service Discovery
//!
//! Per the specification, entities advertise support via `http://jabber.org/protocol/chatstates`
//! in disco#info. The *server* does not advertise this feature; it is a client-to-client
//! negotiation. The server transparently routes chat state notifications.

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0085 Chat State Notifications.
pub const NS_CHATSTATES: &str = "http://jabber.org/protocol/chatstates";

/// Chat state notification variants.
///
/// Mirrors the five states defined in XEP-0085. This is a local enum that
/// decouples our API from `xmpp_parsers::chatstates::ChatState` while
/// providing lossless conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatState {
    /// User is actively participating in the conversation.
    Active,
    /// User is composing a message.
    Composing,
    /// User was composing but has stopped.
    Paused,
    /// User has not interacted recently.
    Inactive,
    /// User has effectively ended the conversation.
    Gone,
}

impl ChatState {
    /// Returns the XML element name for this chat state.
    pub fn element_name(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Composing => "composing",
            Self::Paused => "paused",
            Self::Inactive => "inactive",
            Self::Gone => "gone",
        }
    }

    /// Returns `true` if this state indicates the user is typing.
    pub fn is_composing(self) -> bool {
        matches!(self, Self::Composing)
    }

    /// Returns `true` if this is a terminal state (gone).
    pub fn is_gone(self) -> bool {
        matches!(self, Self::Gone)
    }
}

impl std::fmt::Display for ChatState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.element_name())
    }
}

/// Errors that can occur when parsing chat state notifications.
#[derive(Debug, Error)]
pub enum ChatStateError {
    /// The element belongs to the chatstates namespace but has an unrecognized name.
    #[error("unknown chat state element: {0}")]
    UnknownState(String),
}

/// Trait for types that can carry chat state notifications.
///
/// This trait abstracts the ability to extract and attach chat state information,
/// allowing future message types (e.g., MUC-specific wrappers) to participate
/// in chat state processing.
pub trait ChatStateCarrier {
    /// Extract the chat state from this carrier, if present.
    fn chat_state(&self) -> Option<ChatState>;

    /// Returns `true` if this carrier contains a chat state element.
    fn has_chat_state(&self) -> bool {
        self.chat_state().is_some()
    }

    /// Returns `true` if this is a standalone chat state notification (no body).
    fn is_standalone_chat_state(&self) -> bool;
}

impl ChatStateCarrier for Message {
    fn chat_state(&self) -> Option<ChatState> {
        extract_chat_state_from_message(self)
    }

    fn is_standalone_chat_state(&self) -> bool {
        self.chat_state().is_some() && self.bodies.is_empty()
    }
}

/// Parse a chat state from a namespace-qualified element name.
pub fn parse_chat_state(element_name: &str) -> Result<ChatState, ChatStateError> {
    match element_name {
        "active" => Ok(ChatState::Active),
        "composing" => Ok(ChatState::Composing),
        "paused" => Ok(ChatState::Paused),
        "inactive" => Ok(ChatState::Inactive),
        "gone" => Ok(ChatState::Gone),
        other => Err(ChatStateError::UnknownState(other.to_owned())),
    }
}

/// Check if an element is a chat state notification.
pub fn is_chat_state_element(elem: &Element) -> bool {
    elem.ns() == NS_CHATSTATES
        && matches!(
            elem.name(),
            "active" | "composing" | "paused" | "inactive" | "gone"
        )
}

/// Extract the chat state from a message's payloads.
///
/// Returns the first valid chat state found. Per XEP-0085, a message SHOULD
/// contain at most one chat state element.
pub fn extract_chat_state_from_message(msg: &Message) -> Option<ChatState> {
    msg.payloads
        .iter()
        .filter(|elem| elem.ns() == NS_CHATSTATES)
        .find_map(|elem| parse_chat_state(elem.name()).ok())
}

/// Build a chat state element.
pub fn build_chat_state_element(state: ChatState) -> Element {
    Element::builder(state.element_name(), NS_CHATSTATES).build()
}

/// Build a standalone chat state notification message.
///
/// Creates a `<message type='chat'>` containing only the specified chat state
/// element (no body). Used for sending typing indicators and presence-like
/// state changes within a conversation.
pub fn build_chat_state_message(
    to: impl Into<Option<jid::Jid>>,
    from: impl Into<Option<jid::Jid>>,
    state: ChatState,
) -> Message {
    let mut msg = Message::new(to.into());
    msg.from = from.into();
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.payloads.push(build_chat_state_element(state));
    msg
}

/// Set the chat state on an existing message, replacing any existing chat state.
pub fn set_chat_state(msg: &mut Message, state: ChatState) {
    msg.payloads.retain(|elem| elem.ns() != NS_CHATSTATES);
    msg.payloads.push(build_chat_state_element(state));
}

/// Remove all chat state elements from a message.
pub fn strip_chat_states(msg: &mut Message) {
    msg.payloads.retain(|elem| elem.ns() != NS_CHATSTATES);
}

/// Check if a message is a body-less chat state notification.
///
/// These messages carry only conversational state (e.g., typing indicators)
/// and should not be archived or counted as real messages.
pub fn is_standalone_notification(msg: &Message) -> bool {
    msg.bodies.is_empty() && msg.payloads.iter().any(|elem| is_chat_state_element(elem))
}

/// Convert from `xmpp_parsers::chatstates::ChatState` to our local enum.
impl From<xmpp_parsers::chatstates::ChatState> for ChatState {
    fn from(state: xmpp_parsers::chatstates::ChatState) -> Self {
        match state {
            xmpp_parsers::chatstates::ChatState::Active => Self::Active,
            xmpp_parsers::chatstates::ChatState::Composing => Self::Composing,
            xmpp_parsers::chatstates::ChatState::Gone => Self::Gone,
            xmpp_parsers::chatstates::ChatState::Inactive => Self::Inactive,
            xmpp_parsers::chatstates::ChatState::Paused => Self::Paused,
        }
    }
}

/// Convert from our local enum to `xmpp_parsers::chatstates::ChatState`.
impl From<ChatState> for xmpp_parsers::chatstates::ChatState {
    fn from(state: ChatState) -> Self {
        match state {
            ChatState::Active => Self::Active,
            ChatState::Composing => Self::Composing,
            ChatState::Gone => Self::Gone,
            ChatState::Inactive => Self::Inactive,
            ChatState::Paused => Self::Paused,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Body, Message, MessageType};

    #[test]
    fn test_parse_all_states() {
        assert_eq!(
            parse_chat_state("active").expect("valid"),
            ChatState::Active
        );
        assert_eq!(
            parse_chat_state("composing").expect("valid"),
            ChatState::Composing
        );
        assert_eq!(
            parse_chat_state("paused").expect("valid"),
            ChatState::Paused
        );
        assert_eq!(
            parse_chat_state("inactive").expect("valid"),
            ChatState::Inactive
        );
        assert_eq!(parse_chat_state("gone").expect("valid"), ChatState::Gone);
    }

    #[test]
    fn test_parse_unknown_state() {
        let err = parse_chat_state("typing").expect_err("should fail");
        assert!(err.to_string().contains("typing"));
    }

    #[test]
    fn test_is_chat_state_element() {
        let active = Element::builder("active", NS_CHATSTATES).build();
        assert!(is_chat_state_element(&active));

        let composing = Element::builder("composing", NS_CHATSTATES).build();
        assert!(is_chat_state_element(&composing));

        let wrong_ns = Element::builder("active", "jabber:client").build();
        assert!(!is_chat_state_element(&wrong_ns));

        let wrong_name = Element::builder("typing", NS_CHATSTATES).build();
        assert!(!is_chat_state_element(&wrong_name));
    }

    #[test]
    fn test_extract_chat_state_from_message() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <composing xmlns='http://jabber.org/protocol/chatstates'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert_eq!(
            extract_chat_state_from_message(&msg),
            Some(ChatState::Composing)
        );
    }

    #[test]
    fn test_extract_chat_state_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert_eq!(extract_chat_state_from_message(&msg), None);
    }

    #[test]
    fn test_build_chat_state_element() {
        let elem = build_chat_state_element(ChatState::Composing);
        assert_eq!(elem.name(), "composing");
        assert_eq!(elem.ns(), NS_CHATSTATES);
    }

    #[test]
    fn test_build_chat_state_message() {
        let to: jid::Jid = "romeo@example.com".parse().expect("valid jid");
        let from: jid::Jid = "juliet@example.com".parse().expect("valid jid");
        let msg = build_chat_state_message(to.clone(), from.clone(), ChatState::Composing);

        assert_eq!(msg.to, Some(to));
        assert_eq!(msg.from, Some(from));
        assert_eq!(msg.type_, MessageType::Chat);
        assert!(msg.bodies.is_empty());
        assert_eq!(
            extract_chat_state_from_message(&msg),
            Some(ChatState::Composing)
        );
    }

    #[test]
    fn test_set_chat_state_replaces_existing() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <composing xmlns='http://jabber.org/protocol/chatstates'/>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        set_chat_state(&mut msg, ChatState::Paused);

        assert_eq!(
            extract_chat_state_from_message(&msg),
            Some(ChatState::Paused)
        );
        // Only one chat state element should remain
        let count = msg
            .payloads
            .iter()
            .filter(|e| e.ns() == NS_CHATSTATES)
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_strip_chat_states() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <active xmlns='http://jabber.org/protocol/chatstates'/>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        strip_chat_states(&mut msg);

        assert_eq!(extract_chat_state_from_message(&msg), None);
        assert!(msg.payloads.iter().all(|e| e.ns() != NS_CHATSTATES));
    }

    #[test]
    fn test_is_standalone_notification() {
        // Chat state only, no body → standalone
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <composing xmlns='http://jabber.org/protocol/chatstates'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(is_standalone_notification(&msg));

        // Chat state + body → not standalone
        let mut msg_with_body = Message::new(None::<jid::Jid>);
        msg_with_body
            .bodies
            .insert(String::new(), Body("Hello".to_string()));
        msg_with_body
            .payloads
            .push(build_chat_state_element(ChatState::Active));
        assert!(!is_standalone_notification(&msg_with_body));

        // No chat state at all → not standalone
        let plain_msg = Message::new(None::<jid::Jid>);
        assert!(!is_standalone_notification(&plain_msg));
    }

    #[test]
    fn test_chat_state_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <gone xmlns='http://jabber.org/protocol/chatstates'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        // Trait methods
        assert!(msg.has_chat_state());
        assert_eq!(msg.chat_state(), Some(ChatState::Gone));
        assert!(msg.is_standalone_chat_state());
    }

    #[test]
    fn test_chat_state_display() {
        assert_eq!(ChatState::Active.to_string(), "active");
        assert_eq!(ChatState::Composing.to_string(), "composing");
        assert_eq!(ChatState::Paused.to_string(), "paused");
        assert_eq!(ChatState::Inactive.to_string(), "inactive");
        assert_eq!(ChatState::Gone.to_string(), "gone");
    }

    #[test]
    fn test_roundtrip_conversion() {
        use xmpp_parsers::chatstates::ChatState as ParsersChatState;

        let states = [
            (ChatState::Active, ParsersChatState::Active),
            (ChatState::Composing, ParsersChatState::Composing),
            (ChatState::Paused, ParsersChatState::Paused),
            (ChatState::Inactive, ParsersChatState::Inactive),
            (ChatState::Gone, ParsersChatState::Gone),
        ];

        for (local, parser) in states {
            let converted: ParsersChatState = local.into();
            assert_eq!(converted, parser);
            let back: ChatState = converted.into();
            assert_eq!(back, local);
        }
    }

    #[test]
    fn test_state_predicates() {
        assert!(ChatState::Composing.is_composing());
        assert!(!ChatState::Active.is_composing());
        assert!(ChatState::Gone.is_gone());
        assert!(!ChatState::Paused.is_gone());
    }
}
