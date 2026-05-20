//! XEP-0452: MUC Mention Notifications
//!
//! Provides mention notification elements for MUC rooms. When a user
//! is @mentioned in a room, the server can send a notification message
//! to the mentioned user even if they aren't actively viewing the room.
//!
//! ## XML Format
//!
//! Notification message sent to a mentioned user:
//! ```xml
//! <message from='room@muc.example.com' to='alice@example.com' type='groupchat'>
//!   <mention xmlns='urn:xmpp:mmn:0'
//!            id='original-msg-id'
//!            by='room@muc.example.com/nick'/>
//!   <body>@alice check this out!</body>
//! </message>
//! ```
//!
//! ## Use Cases
//!
//! - Show mention badges on rooms in the sidebar
//! - Deliver push notifications for @mentions
//! - Track unread mentions per room
//!
//! ## Server Behavior
//!
//! When a message with a mention reference (XEP-0372) is broadcast:
//! 1. Detect mentioned JIDs from `<reference type='mention'/>` elements
//! 2. For each mentioned user, check if they're in the room
//! 3. Generate a mention notification for delivery/push

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0452 MUC Mention Notifications.
pub const NS_MENTION_NOTIFICATION: &str = "urn:xmpp:mmn:0";

/// A mention notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionNotification {
    /// The ID of the message containing the mention.
    pub message_id: String,
    /// The MUC occupant JID of who sent the mention.
    pub mentioned_by: Option<String>,
    /// The room JID where the mention occurred.
    pub room_jid: Option<String>,
}

impl MentionNotification {
    /// Create a new mention notification.
    pub fn new(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            mentioned_by: None,
            room_jid: None,
        }
    }

    /// Set who mentioned the user.
    pub fn with_by(mut self, by: impl Into<String>) -> Self {
        self.mentioned_by = Some(by.into());
        self
    }

    /// Set the room where the mention occurred.
    pub fn with_room(mut self, room: impl Into<String>) -> Self {
        self.room_jid = Some(room.into());
        self
    }
}

/// Trait for types that can carry mention notifications.
pub trait MentionNotificationCarrier {
    /// Extract the mention notification from this carrier.
    fn mention_notification(&self) -> Option<MentionNotification>;

    /// Returns `true` if this carrier has a mention notification.
    fn has_mention_notification(&self) -> bool {
        self.mention_notification().is_some()
    }
}

impl MentionNotificationCarrier for Message {
    fn mention_notification(&self) -> Option<MentionNotification> {
        extract_mention_notification(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<mention/>` notification element.
pub fn is_mention_notification_element(elem: &Element) -> bool {
    elem.ns() == NS_MENTION_NOTIFICATION && elem.name() == "mention"
}

/// Check if a message contains a mention notification.
pub fn has_mention_notification(msg: &Message) -> bool {
    msg.payloads.iter().any(is_mention_notification_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract a mention notification from a message.
pub fn extract_mention_notification(msg: &Message) -> Option<MentionNotification> {
    let elem = msg
        .payloads
        .iter()
        .find(|e| is_mention_notification_element(e))?;

    let message_id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();
    let mentioned_by = elem
        .attr("by")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());
    let room_jid = msg.from.as_ref().map(|j| j.to_string());

    Some(MentionNotification {
        message_id,
        mentioned_by,
        room_jid,
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<mention/>` notification element.
pub fn build_mention_notification_element(notification: &MentionNotification) -> Element {
    let mut builder = Element::builder("mention", NS_MENTION_NOTIFICATION).attr(
        minidom::rxml::xml_ncname!("id").to_owned(),
        notification.message_id.as_str(),
    );

    if let Some(ref by) = notification.mentioned_by {
        builder = builder.attr(minidom::rxml::xml_ncname!("by").to_owned(), by.as_str());
    }

    builder.build()
}

/// Build a mention notification message.
///
/// Creates a message to notify a user they were mentioned in a room.
pub fn build_mention_notification_message(
    to: jid::Jid,
    from_room: jid::Jid,
    notification: &MentionNotification,
    body_preview: Option<&str>,
) -> Message {
    let mut msg = Message::new(Some(to));
    msg.from = Some(from_room);
    msg.type_ = xmpp_parsers::message::MessageType::Groupchat;
    msg.payloads
        .push(build_mention_notification_element(notification));

    if let Some(preview) = body_preview {
        msg.bodies
            .insert(xmpp_parsers::message::Lang::new(), preview.to_owned());
    }

    msg
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a mention notification to a message.
pub fn set_mention_notification(msg: &mut Message, notification: &MentionNotification) {
    msg.payloads.retain(|e| e.ns() != NS_MENTION_NOTIFICATION);
    msg.payloads
        .push(build_mention_notification_element(notification));
}

/// Remove mention notification from a message.
pub fn strip_mention_notification(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_MENTION_NOTIFICATION);
}

// ── Mention tracking ─────────────────────────────────────────────────

/// Tracks unread mention counts per room for a user.
#[derive(Debug, Default)]
pub struct MentionCounter {
    /// Room JID → unread mention count.
    counts: std::collections::HashMap<String, u32>,
}

impl MentionCounter {
    /// Create a new counter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the mention count for a room.
    pub fn increment(&mut self, room_jid: &str) {
        *self.counts.entry(room_jid.to_owned()).or_insert(0) += 1;
    }

    /// Get the mention count for a room.
    pub fn count(&self, room_jid: &str) -> u32 {
        self.counts.get(room_jid).copied().unwrap_or(0)
    }

    /// Clear mentions for a room (user viewed it).
    pub fn clear_room(&mut self, room_jid: &str) {
        self.counts.remove(room_jid);
    }

    /// Clear all mentions.
    pub fn clear_all(&mut self) {
        self.counts.clear();
    }

    /// Get all rooms with unread mentions.
    pub fn rooms_with_mentions(&self) -> Vec<(&str, u32)> {
        self.counts
            .iter()
            .filter(|(_, &count)| count > 0)
            .map(|(room, &count)| (room.as_str(), count))
            .collect()
    }

    /// Total unread mentions across all rooms.
    pub fn total(&self) -> u32 {
        self.counts.values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::MessageType;

    #[test]
    fn test_is_mention_notification_element() {
        let elem = Element::builder("mention", NS_MENTION_NOTIFICATION)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "msg-1")
            .build();
        assert!(is_mention_notification_element(&elem));

        let wrong = Element::builder("mention", "jabber:client").build();
        assert!(!is_mention_notification_element(&wrong));
    }

    #[test]
    fn test_extract_mention_notification() {
        let xml = "<message xmlns='jabber:client' type='groupchat' from='room@muc.example.com'>\
                    <body>@alice hey!</body>\
                    <mention xmlns='urn:xmpp:mmn:0' id='msg-42' by='room@muc.example.com/bob'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let notif = extract_mention_notification(&msg).expect("has notification");
        assert_eq!(notif.message_id, "msg-42");
        assert_eq!(
            notif.mentioned_by.as_deref(),
            Some("room@muc.example.com/bob")
        );
        assert_eq!(notif.room_jid.as_deref(), Some("room@muc.example.com"));
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_mention_notification(&msg).is_none());
    }

    #[test]
    fn test_build_mention_notification() {
        let notif = MentionNotification::new("msg-1")
            .with_by("room@muc/nick")
            .with_room("room@muc");
        let elem = build_mention_notification_element(&notif);

        assert_eq!(elem.name(), "mention");
        assert_eq!(elem.ns(), NS_MENTION_NOTIFICATION);
        assert_eq!(elem.attr("id"), Some("msg-1"));
        assert_eq!(elem.attr("by"), Some("room@muc/nick"));
    }

    #[test]
    fn test_build_notification_message() {
        let to: jid::Jid = "alice@example.com".parse().expect("valid");
        let room: jid::Jid = "room@muc.example.com".parse().expect("valid");
        let notif = MentionNotification::new("msg-1").with_by("room@muc.example.com/bob");

        let msg = build_mention_notification_message(
            to.clone(),
            room.clone(),
            &notif,
            Some("@alice hey!"),
        );

        assert_eq!(msg.to, Some(to));
        assert_eq!(msg.from, Some(room));
        assert_eq!(msg.type_, MessageType::Groupchat);
        assert!(has_mention_notification(&msg));
        assert!(!msg.bodies.is_empty());
    }

    #[test]
    fn test_set_mention_notification() {
        let mut msg = Message::new(None::<jid::Jid>);
        let notif = MentionNotification::new("msg-1");
        set_mention_notification(&mut msg, &notif);
        assert!(has_mention_notification(&msg));

        // Replace
        let notif2 = MentionNotification::new("msg-2");
        set_mention_notification(&mut msg, &notif2);
        let extracted = extract_mention_notification(&msg).expect("has notif");
        assert_eq!(extracted.message_id, "msg-2");
    }

    #[test]
    fn test_strip_mention_notification() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_mention_notification(&mut msg, &MentionNotification::new("msg-1"));
        strip_mention_notification(&mut msg);
        assert!(!has_mention_notification(&msg));
    }

    #[test]
    fn test_mention_notification_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <mention xmlns='urn:xmpp:mmn:0' id='msg-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_mention_notification());
        assert_eq!(
            msg.mention_notification().map(|n| n.message_id),
            Some("msg-1".to_owned())
        );
    }

    #[test]
    fn test_mention_counter() {
        let mut counter = MentionCounter::new();

        assert_eq!(counter.count("room@muc"), 0);
        assert_eq!(counter.total(), 0);

        counter.increment("room1@muc");
        counter.increment("room1@muc");
        counter.increment("room2@muc");

        assert_eq!(counter.count("room1@muc"), 2);
        assert_eq!(counter.count("room2@muc"), 1);
        assert_eq!(counter.total(), 3);

        let rooms = counter.rooms_with_mentions();
        assert_eq!(rooms.len(), 2);

        counter.clear_room("room1@muc");
        assert_eq!(counter.count("room1@muc"), 0);
        assert_eq!(counter.total(), 1);

        counter.clear_all();
        assert_eq!(counter.total(), 0);
    }

    #[test]
    fn test_notification_builder() {
        let n = MentionNotification::new("id-1")
            .with_by("room@muc/alice")
            .with_room("room@muc");
        assert_eq!(n.message_id, "id-1");
        assert_eq!(n.mentioned_by.as_deref(), Some("room@muc/alice"));
        assert_eq!(n.room_jid.as_deref(), Some("room@muc"));
    }
}
