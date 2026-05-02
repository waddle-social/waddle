//! Waddle MUC thread metadata.
//!
//! This is a private Waddle message payload used by the chat UI while real
//! XEP-0508 support is implemented through PubSub/XEP-0472.
//!
//! ## XML Format
//!
//! Create a forum thread:
//! ```xml
//! <message type='groupchat' to='forum@muc.example.com'>
//!   <body>Welcome to the discussion!</body>
//!   <thread-create xmlns='urn:waddle:forums:0'
//!                  title='Getting Started Guide'/>
//! </message>
//! ```
//!
//! Reply to a thread:
//! ```xml
//! <message type='groupchat' to='forum@muc.example.com'>
//!   <body>Great guide, thanks!</body>
//!   <thread-reply xmlns='urn:waddle:forums:0' thread-id='thread-123'/>
//! </message>
//! ```
//!
//! ## Room Type
//!
//! Forum rooms are MUC rooms with `muc#roomconfig_forum` set to `true`.
//! They support both forum threads and regular chat messages.

use minidom::Element;
use xmpp_parsers::message::Message;

/// Private Waddle namespace for MUC thread metadata.
pub const NS_FORUMS: &str = "urn:waddle:forums:0";

/// Room config field for enabling forum mode.
pub const FIELD_FORUM_MODE: &str = "muc#roomconfig_forum";

/// A thread creation element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadCreate {
    /// The thread title/topic.
    pub title: String,
}

impl ThreadCreate {
    /// Create a new thread creation.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
        }
    }
}

/// A reply to an existing thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadReply {
    /// The ID of the thread being replied to.
    pub thread_id: String,
}

impl ThreadReply {
    /// Create a new thread reply.
    pub fn new(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
        }
    }
}

/// What kind of forum element is in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForumAction {
    /// Creating a new thread with a title.
    CreateThread(ThreadCreate),
    /// Replying to an existing thread.
    Reply(ThreadReply),
}

/// Trait for types that can carry forum elements.
pub trait ForumCarrier {
    /// Extract the forum action from this carrier.
    fn forum_action(&self) -> Option<ForumAction>;

    /// Returns `true` if this creates a new thread.
    fn is_thread_creation(&self) -> bool {
        matches!(self.forum_action(), Some(ForumAction::CreateThread(_)))
    }

    /// Returns `true` if this is a thread reply.
    fn is_thread_reply(&self) -> bool {
        matches!(self.forum_action(), Some(ForumAction::Reply(_)))
    }
}

impl ForumCarrier for Message {
    fn forum_action(&self) -> Option<ForumAction> {
        extract_forum_action(self)
    }
}

/// Summary of a forum thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSummary {
    /// Thread ID (typically the message ID of the opening post).
    pub id: String,
    /// Thread title.
    pub title: String,
    /// Author of the opening post.
    pub author: Option<String>,
    /// Number of replies.
    pub reply_count: u32,
    /// Timestamp of last activity.
    pub last_activity: Option<String>,
}

impl ThreadSummary {
    /// Create a new thread summary.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            author: None,
            reply_count: 0,
            last_activity: None,
        }
    }

    /// Set the author.
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// Set the reply count.
    pub fn with_reply_count(mut self, count: u32) -> Self {
        self.reply_count = count;
        self
    }

    /// Set the last activity timestamp.
    pub fn with_last_activity(mut self, ts: impl Into<String>) -> Self {
        self.last_activity = Some(ts.into());
        self
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a forum element.
pub fn is_forum_element(elem: &Element) -> bool {
    elem.ns() == NS_FORUMS && matches!(elem.name(), "thread-create" | "thread-reply")
}

/// Check if a message contains forum elements.
pub fn has_forum_action(msg: &Message) -> bool {
    msg.payloads.iter().any(is_forum_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract the forum action from a message.
pub fn extract_forum_action(msg: &Message) -> Option<ForumAction> {
    for elem in &msg.payloads {
        if elem.ns() != NS_FORUMS {
            continue;
        }
        match elem.name() {
            "thread-create" => {
                if let Some(title) = elem.attr("title").map(str::trim).filter(|t| !t.is_empty()) {
                    return Some(ForumAction::CreateThread(ThreadCreate::new(title)));
                }
            }
            "thread-reply" => {
                if let Some(thread_id) = elem
                    .attr("thread-id")
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    return Some(ForumAction::Reply(ThreadReply::new(thread_id)));
                }
            }
            _ => {}
        }
    }
    None
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<thread-create/>` element.
pub fn build_thread_create_element(thread: &ThreadCreate) -> Element {
    Element::builder("thread-create", NS_FORUMS)
        .attr("title", thread.title.as_str())
        .build()
}

/// Build a `<thread-reply/>` element.
pub fn build_thread_reply_element(reply: &ThreadReply) -> Element {
    Element::builder("thread-reply", NS_FORUMS)
        .attr("thread-id", reply.thread_id.as_str())
        .build()
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a thread creation to a message.
pub fn set_thread_create(msg: &mut Message, thread: &ThreadCreate) {
    msg.payloads.retain(|e| e.ns() != NS_FORUMS);
    msg.payloads.push(build_thread_create_element(thread));
}

/// Add a thread reply to a message.
pub fn set_thread_reply(msg: &mut Message, reply: &ThreadReply) {
    msg.payloads.retain(|e| e.ns() != NS_FORUMS);
    msg.payloads.push(build_thread_reply_element(reply));
}

/// Remove forum elements from a message.
pub fn strip_forum(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_FORUMS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::MessageType;

    #[test]
    fn test_is_forum_element() {
        let create = Element::builder("thread-create", NS_FORUMS).build();
        assert!(is_forum_element(&create));

        let reply = Element::builder("thread-reply", NS_FORUMS).build();
        assert!(is_forum_element(&reply));

        let wrong = Element::builder("thread-create", "jabber:client").build();
        assert!(!is_forum_element(&wrong));
    }

    #[test]
    fn test_extract_thread_create() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Welcome!</body>\
                    <thread-create xmlns='urn:waddle:forums:0' title='Getting Started'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let action = extract_forum_action(&msg).expect("has action");
        assert!(
            matches!(action, ForumAction::CreateThread(ref tc) if tc.title == "Getting Started")
        );
    }

    #[test]
    fn test_extract_thread_reply() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Great guide!</body>\
                    <thread-reply xmlns='urn:waddle:forums:0' thread-id='thread-42'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let action = extract_forum_action(&msg).expect("has action");
        assert!(matches!(action, ForumAction::Reply(ref tr) if tr.thread_id == "thread-42"));
    }

    #[test]
    fn test_extract_skips_malformed_forum_payloads() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <thread-create xmlns='urn:waddle:forums:0' title=''/>\
                    <thread-reply xmlns='urn:waddle:forums:0' thread-id='   '/>\
                    <thread-reply xmlns='urn:waddle:forums:0' thread-id='thread-42'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let action = extract_forum_action(&msg).expect("has action");
        assert!(matches!(action, ForumAction::Reply(ref tr) if tr.thread_id == "thread-42"));
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_forum_action(&msg).is_none());
    }

    #[test]
    fn test_build_thread_create() {
        let tc = ThreadCreate::new("My Thread");
        let elem = build_thread_create_element(&tc);
        assert_eq!(elem.name(), "thread-create");
        assert_eq!(elem.ns(), NS_FORUMS);
        assert_eq!(elem.attr("title"), Some("My Thread"));
    }

    #[test]
    fn test_build_thread_reply() {
        let tr = ThreadReply::new("thread-99");
        let elem = build_thread_reply_element(&tr);
        assert_eq!(elem.name(), "thread-reply");
        assert_eq!(elem.attr("thread-id"), Some("thread-99"));
    }

    #[test]
    fn test_set_thread_create() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.type_ = MessageType::Groupchat;
        set_thread_create(&mut msg, &ThreadCreate::new("Topic"));

        assert!(has_forum_action(&msg));
        assert!(msg.is_thread_creation());
        assert!(!msg.is_thread_reply());
    }

    #[test]
    fn test_set_thread_reply() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_thread_reply(&mut msg, &ThreadReply::new("t-1"));

        assert!(msg.is_thread_reply());
        assert!(!msg.is_thread_creation());
    }

    #[test]
    fn test_strip_forum() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_thread_create(&mut msg, &ThreadCreate::new("Topic"));
        strip_forum(&mut msg);
        assert!(!has_forum_action(&msg));
    }

    #[test]
    fn test_forum_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <thread-create xmlns='urn:waddle:forums:0' title='Test'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.is_thread_creation());
        match msg.forum_action() {
            Some(ForumAction::CreateThread(tc)) => assert_eq!(tc.title, "Test"),
            other => panic!("Expected CreateThread, got {other:?}"),
        }
    }

    #[test]
    fn test_thread_summary() {
        let ts = ThreadSummary::new("t-1", "Getting Started")
            .with_author("alice")
            .with_reply_count(5)
            .with_last_activity("2024-06-01T12:00:00Z");

        assert_eq!(ts.id, "t-1");
        assert_eq!(ts.title, "Getting Started");
        assert_eq!(ts.author.as_deref(), Some("alice"));
        assert_eq!(ts.reply_count, 5);
        assert_eq!(ts.last_activity.as_deref(), Some("2024-06-01T12:00:00Z"));
    }
}
