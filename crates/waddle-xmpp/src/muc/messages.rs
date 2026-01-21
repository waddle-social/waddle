//! MUC Message Types
//!
//! Types for handling MUC groupchat message routing and broadcasting.

use jid::{BareJid, FullJid, Jid};
use tracing::debug;
use xmpp_parsers::message::{Message, MessageType};

use crate::XmppError;

/// Represents a parsed MUC message ready for routing.
#[derive(Debug, Clone)]
pub struct MucMessage {
    /// The room this message is destined for (bare JID)
    pub room_jid: BareJid,
    /// The sender's full JID (user@domain/resource)
    pub sender_jid: FullJid,
    /// The original message
    pub message: Message,
}

impl MucMessage {
    /// Create a MUC message from an XMPP message.
    ///
    /// Validates that the message is a groupchat type destined for a MUC room.
    pub fn from_message(msg: Message, sender_jid: FullJid) -> Result<Self, XmppError> {
        // Validate message type is groupchat
        if msg.type_ != MessageType::Groupchat {
            return Err(XmppError::bad_request(Some(
                "Expected groupchat message type".to_string(),
            )));
        }

        // Extract the room JID from the 'to' attribute
        let room_jid = msg
            .to
            .as_ref()
            .ok_or_else(|| XmppError::bad_request(Some("Message missing 'to' attribute".to_string())))?
            .clone();

        // Convert to bare JID (strip resource if present)
        let room_bare_jid = match room_jid.try_into_full() {
            Ok(full) => full.to_bare(),
            Err(bare) => bare,
        };

        debug!(
            room = %room_bare_jid,
            sender = %sender_jid,
            "Parsed MUC message"
        );

        Ok(Self {
            room_jid: room_bare_jid,
            sender_jid,
            message: msg,
        })
    }

    /// Check if this message has a body (text content).
    pub fn has_body(&self) -> bool {
        !self.message.bodies.is_empty()
    }

    /// Get the message body text (first body if multiple languages).
    pub fn body_text(&self) -> Option<&str> {
        self.message.bodies.iter().next().map(|b| b.0.as_str())
    }

    /// Get the message ID.
    pub fn id(&self) -> Option<&str> {
        self.message.id.as_deref()
    }
}

/// An outbound MUC message to send to an occupant.
#[derive(Debug, Clone)]
pub struct OutboundMucMessage {
    /// The recipient's full JID
    pub to: FullJid,
    /// The message to send
    pub message: Message,
}

impl OutboundMucMessage {
    /// Create a new outbound message.
    pub fn new(to: FullJid, message: Message) -> Self {
        Self { to, message }
    }
}

/// Result of routing a message through a MUC room.
#[derive(Debug)]
pub struct MessageRouteResult {
    /// Messages to send to occupants (including sender echo per XEP-0045)
    pub outbound_messages: Vec<OutboundMucMessage>,
    /// Whether the message was successfully routed
    pub success: bool,
    /// Error message if routing failed
    pub error: Option<String>,
}

impl MessageRouteResult {
    /// Create a successful route result.
    pub fn success(outbound_messages: Vec<OutboundMucMessage>) -> Self {
        Self {
            outbound_messages,
            success: true,
            error: None,
        }
    }

    /// Create a failed route result.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            outbound_messages: Vec::new(),
            success: false,
            error: Some(error.into()),
        }
    }
}

/// Check if a message is a MUC groupchat message.
pub fn is_muc_groupchat(msg: &Message) -> bool {
    msg.type_ == MessageType::Groupchat
}

/// Check if a JID appears to be a MUC room JID.
///
/// This is a heuristic check based on the domain containing "muc." or "conference.".
/// For accurate checks, use MucRoomRegistry::is_muc_jid().
pub fn looks_like_muc_jid(jid: &BareJid) -> bool {
    let domain = jid.domain().as_str();
    domain.starts_with("muc.") || domain.starts_with("conference.")
}

/// Create a groupchat message for broadcasting.
///
/// Sets up the message with appropriate attributes for MUC broadcast:
/// - Type set to groupchat
/// - From set to the room JID with sender's nick
/// - Original message ID preserved
pub fn create_broadcast_message(
    original: &Message,
    from_room_jid: FullJid,
    to_occupant: FullJid,
) -> Message {
    let mut broadcast = original.clone();
    broadcast.type_ = MessageType::Groupchat;
    broadcast.from = Some(Jid::from(from_room_jid));
    broadcast.to = Some(Jid::from(to_occupant));
    broadcast
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::Body;

    fn make_groupchat_message(to: &str, body: &str) -> Message {
        let bare_jid: BareJid = to.parse().unwrap();
        let mut msg = Message::new(Some(Jid::from(bare_jid)));
        msg.type_ = MessageType::Groupchat;
        msg.id = Some("msg-1".to_string());
        msg.bodies.insert(String::new(), Body(body.to_string()));
        msg
    }

    #[test]
    fn test_muc_message_from_groupchat() {
        let msg = make_groupchat_message("room@muc.example.com", "Hello!");
        let sender: FullJid = "user@example.com/resource".parse().unwrap();

        let muc_msg = MucMessage::from_message(msg, sender.clone()).unwrap();

        assert_eq!(muc_msg.room_jid.to_string(), "room@muc.example.com");
        assert_eq!(muc_msg.sender_jid, sender);
        assert!(muc_msg.has_body());
        assert_eq!(muc_msg.body_text(), Some("Hello!"));
    }

    #[test]
    fn test_muc_message_rejects_non_groupchat() {
        let mut msg = make_groupchat_message("room@muc.example.com", "Hello!");
        msg.type_ = MessageType::Chat; // Wrong type!

        let sender: FullJid = "user@example.com/resource".parse().unwrap();
        let result = MucMessage::from_message(msg, sender);

        assert!(result.is_err());
    }

    #[test]
    fn test_muc_message_rejects_missing_to() {
        let mut msg = Message::new(None::<Jid>);
        msg.type_ = MessageType::Groupchat;

        let sender: FullJid = "user@example.com/resource".parse().unwrap();
        let result = MucMessage::from_message(msg, sender);

        assert!(result.is_err());
    }

    #[test]
    fn test_is_muc_groupchat() {
        let groupchat = make_groupchat_message("room@muc.example.com", "Hello!");
        assert!(is_muc_groupchat(&groupchat));

        let bare_jid: BareJid = "user@example.com".parse().unwrap();
        let mut chat = Message::new(Some(Jid::from(bare_jid)));
        chat.type_ = MessageType::Chat;
        assert!(!is_muc_groupchat(&chat));
    }

    #[test]
    fn test_looks_like_muc_jid() {
        let muc_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let conf_jid: BareJid = "room@conference.example.com".parse().unwrap();
        let user_jid: BareJid = "user@example.com".parse().unwrap();

        assert!(looks_like_muc_jid(&muc_jid));
        assert!(looks_like_muc_jid(&conf_jid));
        assert!(!looks_like_muc_jid(&user_jid));
    }

    #[test]
    fn test_create_broadcast_message() {
        let original = make_groupchat_message("room@muc.example.com", "Hello!");
        let from: FullJid = "room@muc.example.com/sender_nick".parse().unwrap();
        let to: FullJid = "user@example.com/resource".parse().unwrap();

        let broadcast = create_broadcast_message(&original, from.clone(), to.clone());

        assert_eq!(broadcast.type_, MessageType::Groupchat);
        assert_eq!(broadcast.from, Some(Jid::from(from)));
        assert_eq!(broadcast.to, Some(Jid::from(to)));
        assert_eq!(broadcast.id, Some("msg-1".to_string()));
    }

    #[test]
    fn test_message_route_result() {
        let success = MessageRouteResult::success(vec![]);
        assert!(success.success);
        assert!(success.error.is_none());

        let failure = MessageRouteResult::failure("Room not found");
        assert!(!failure.success);
        assert_eq!(failure.error, Some("Room not found".to_string()));
    }
}
