//! Stanza types — byte-compatible with waddle's `mam_messages` row shape.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An archived XMPP message, matching waddle's `ArchivedMessage` struct.
///
/// Keep field names and optionality aligned with
/// `waddle/server/crates/waddle-xmpp/src/mam/mod.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedMessage {
    /// UUID v7 (time-sortable) as a string.
    pub id: String,
    /// Room/archive JID this message belongs to.
    pub room_jid: String,
    pub timestamp: DateTime<Utc>,
    /// Sender full JID (with resource / nick).
    pub from: String,
    /// Recipient JID (room JID for MUC).
    pub to: String,
    /// `<body/>` is optional per RFC 6121 §5.2.2 — `None` when absent.
    pub body: Option<String>,
    /// RFC 6121 `<message id='...'>` attribute (wire stanza identifier).
    pub message_id: Option<String>,
    /// RFC 6121 thread identifier.
    pub thread_id: Option<String>,
    /// XEP-0359 origin id.
    pub origin_id: Option<String>,
    /// "chat" | "groupchat" | ...
    pub message_type: String,
    /// Full normalized stanza XML for faithful MAM replay.
    pub stanza_xml: Option<String>,
}

impl ArchivedMessage {
    /// Build a minimal chat message with a fresh UUID v7 id.
    pub fn new_chat(room_jid: &str, from: &str, to: &str, body: &str) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            room_jid: room_jid.to_string(),
            timestamp: Utc::now(),
            from: from.to_string(),
            to: to.to_string(),
            body: Some(body.to_string()),
            message_id: None,
            thread_id: None,
            origin_id: None,
            message_type: MessageType::Chat.as_str().to_string(),
            stanza_xml: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MessageType {
    Chat,
    Groupchat,
}

impl MessageType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Groupchat => "groupchat",
        }
    }
}

/// A MAM (XEP-0313) query against a single archive.
///
/// This is a narrowed subset of waddle's MAM query — enough to exercise the
/// three canonical index patterns (room+time, room+sender+time, room+id).
#[derive(Debug, Clone, Default)]
pub struct MamQuery {
    pub room_jid: String,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub from_jid: Option<String>,
    /// RSM before_id: return rows whose id < this id, newest first.
    pub before_id: Option<String>,
    /// RSM after_id: return rows whose id > this id.
    pub after_id: Option<String>,
    /// Max rows (RSM caps at 500 in waddle).
    pub limit: u32,
}

impl MamQuery {
    pub fn new(room_jid: impl Into<String>) -> Self {
        Self {
            room_jid: room_jid.into(),
            limit: 100,
            ..Default::default()
        }
    }
}
