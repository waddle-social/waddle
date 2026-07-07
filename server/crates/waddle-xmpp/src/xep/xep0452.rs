//! XEP-0452: MUC Mention Notifications
//!
//! Provides mention notification elements for MUC rooms. When a user is
//! mentioned in a room, the server can send a notification message to the
//! mentioned user even if they are not actively viewing the room.
//!
//! ## XML Format
//!
//! Notification message sent to a mentioned user:
//! ```xml
//! <message from='room@muc.example.com' to='alice@example.com'>
//!   <mentions xmlns='urn:xmpp:mmn:0'>
//!     <forwarded xmlns='urn:xmpp:forward:0'>
//!       <message xmlns='jabber:client'
//!                type='groupchat'
//!                id='original-msg-id'
//!                from='room@muc.example.com/nick'
//!                to='room@muc.example.com'/>
//!     </forwarded>
//!   </mentions>
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
use xmpp_parsers::message::{Message, MessageType};

/// Namespace for XEP-0452 MUC Mention Notifications.
pub const NS_MENTION_NOTIFICATION: &str = "urn:xmpp:mmn:0";

/// Namespace for XEP-0297 Stanza Forwarding, used by XEP-0452.
pub const NS_FORWARD: &str = "urn:xmpp:forward:0";

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

/// Check if an element is a `<mentions/>` notification element.
pub fn is_mention_notification_element(elem: &Element) -> bool {
    elem.is("mentions", NS_MENTION_NOTIFICATION)
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
    let outer_room = msg.from.as_ref()?.to_bare();
    let forwarded = elem.get_child("forwarded", NS_FORWARD)?;
    let forwarded_message_el = forwarded
        .children()
        .find(|child| child.name() == "message")?;
    let forwarded_message = Message::try_from(forwarded_message_el.clone()).ok()?;
    if forwarded_message.type_ != MessageType::Groupchat {
        return None;
    }

    if forwarded_message
        .from
        .as_ref()
        .is_some_and(|from| from.to_bare() != outer_room)
    {
        return None;
    }
    if forwarded_message
        .to
        .as_ref()
        .is_some_and(|to| to.to_bare() != outer_room)
    {
        return None;
    }

    let message_id = forwarded_message
        .id
        .as_ref()
        .map(|id| id.0.as_str())
        .filter(|id| !id.is_empty())?
        .to_owned();
    let mentioned_by = forwarded_message.from.as_ref().map(ToString::to_string);

    Some(MentionNotification {
        message_id,
        mentioned_by,
        room_jid: Some(outer_room.to_string()),
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<mentions/>` notification from the original MUC message.
///
/// XEP-0452 forwards the whole original groupchat stanza, including payloads
/// added by the room such as stanza IDs and references.
pub fn build_mention_notification_element(original: &Message) -> Element {
    Element::builder("mentions", NS_MENTION_NOTIFICATION)
        .append(
            Element::builder("forwarded", NS_FORWARD)
                .append(Element::from(original.clone()))
                .build(),
        )
        .build()
}

fn room_from_forwarded_message(original: &Message) -> Option<jid::BareJid> {
    original
        .from
        .as_ref()
        .map(|from| from.to_bare())
        .or_else(|| original.to.as_ref().map(|to| to.to_bare()))
}

/// Build a mention notification message.
///
/// Creates a message from the room bare JID to notify a user they were
/// mentioned in the forwarded original MUC message.
pub fn build_mention_notification_message(to: jid::Jid, original: &Message) -> Option<Message> {
    let room = room_from_forwarded_message(original)?;
    let mut msg = Message::new(Some(to));
    msg.type_ = MessageType::Normal;
    msg.from = Some(jid::Jid::from(room));
    msg.payloads
        .push(build_mention_notification_element(original));

    Some(msg)
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a mention notification to a message.
pub fn set_mention_notification(msg: &mut Message, original: &Message) {
    msg.payloads.retain(|e| !is_mention_notification_element(e));
    msg.payloads
        .push(build_mention_notification_element(original));
}

/// Remove mention notification from a message.
pub fn strip_mention_notification(msg: &mut Message) {
    msg.payloads.retain(|e| !is_mention_notification_element(e));
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
        let elem = Element::builder("mentions", NS_MENTION_NOTIFICATION).build();
        assert!(is_mention_notification_element(&elem));

        let wrong = Element::builder("mention", NS_MENTION_NOTIFICATION).build();
        assert!(!is_mention_notification_element(&wrong));
    }

    #[test]
    fn test_extract_mention_notification() {
        let xml = "<message xmlns='jabber:client' from='room@muc.example.com'>\
                    <mentions xmlns='urn:xmpp:mmn:0'>\
                      <forwarded xmlns='urn:xmpp:forward:0'>\
                        <message xmlns='jabber:client' type='groupchat' id='msg-42' from='room@muc.example.com/bob' to='room@muc.example.com'/>\
                      </forwarded>\
                    </mentions>\
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
        let original = Message::try_from(
            "<message xmlns='jabber:client' type='groupchat' id='msg-1' \
                 from='room@muc/nick' to='room@muc'>\
               <body>hello</body>\
             </message>"
                .parse::<Element>()
                .expect("valid xml"),
        )
        .expect("valid message");
        let elem = build_mention_notification_element(&original);

        assert_eq!(elem.name(), "mentions");
        assert_eq!(elem.ns(), NS_MENTION_NOTIFICATION);
        let forwarded = elem.get_child("forwarded", NS_FORWARD).expect("forwarded");
        let message = forwarded
            .children()
            .find(|child| child.name() == "message")
            .expect("forwarded message");
        assert_eq!(message.attr("id"), Some("msg-1"));
        assert_eq!(message.attr("from"), Some("room@muc/nick"));
        assert_eq!(
            message
                .get_child("body", "jabber:client")
                .map(|body| body.text()),
            Some("hello".to_owned())
        );
    }

    #[test]
    fn test_build_notification_message() {
        let to: jid::Jid = "alice@example.com".parse().expect("valid");
        let original = Message::try_from(
            "<message xmlns='jabber:client' type='groupchat' id='msg-1' \
                 from='room@muc.example.com/bob' to='room@muc.example.com'>\
               <body>@alice hey!</body>\
             </message>"
                .parse::<Element>()
                .expect("valid xml"),
        )
        .expect("valid message");

        let msg = build_mention_notification_message(to.clone(), &original)
            .expect("original has room jid");

        assert_eq!(msg.to, Some(to));
        assert_eq!(
            msg.from,
            Some("room@muc.example.com".parse().expect("valid room"))
        );
        assert_eq!(msg.type_, MessageType::Normal);
        assert!(has_mention_notification(&msg));
        assert!(msg.bodies.is_empty());
    }

    #[test]
    fn test_set_mention_notification() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.from = Some("room@muc.example.com".parse().expect("valid room"));
        let original = Message::try_from(
            "<message xmlns='jabber:client' type='groupchat' id='msg-1' \
                 from='room@muc.example.com/bob' to='room@muc.example.com'/>"
                .parse::<Element>()
                .expect("valid xml"),
        )
        .expect("valid message");
        set_mention_notification(&mut msg, &original);
        assert!(has_mention_notification(&msg));

        // Replace
        let replacement = Message::try_from(
            "<message xmlns='jabber:client' type='groupchat' id='msg-2' \
                 from='room@muc.example.com/bob' to='room@muc.example.com'/>"
                .parse::<Element>()
                .expect("valid xml"),
        )
        .expect("valid message");
        set_mention_notification(&mut msg, &replacement);
        let extracted = extract_mention_notification(&msg).expect("has notif");
        assert_eq!(extracted.message_id, "msg-2");
    }

    #[test]
    fn test_strip_mention_notification() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.from = Some("room@muc.example.com".parse().expect("valid room"));
        let original = Message::try_from(
            "<message xmlns='jabber:client' type='groupchat' id='msg-1' \
                 from='room@muc.example.com/bob' to='room@muc.example.com'/>"
                .parse::<Element>()
                .expect("valid xml"),
        )
        .expect("valid message");
        set_mention_notification(&mut msg, &original);
        strip_mention_notification(&mut msg);
        assert!(!has_mention_notification(&msg));
    }

    #[test]
    fn test_mention_notification_carrier_trait() {
        let xml = "<message xmlns='jabber:client' from='room@muc.example.com'>\
                    <mentions xmlns='urn:xmpp:mmn:0'>\
                      <forwarded xmlns='urn:xmpp:forward:0'>\
                        <message xmlns='jabber:client' type='groupchat' id='msg-1' to='room@muc.example.com'/>\
                      </forwarded>\
                    </mentions>\
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
