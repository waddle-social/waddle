//! Message domain types for Waddle Server
//!
//! This module defines the core message types used throughout the messaging system:
//! - `Message`: The main message entity
//! - `MessageFlags`: Bitfield for message properties
//! - `MessageCreate`: DTO for creating new messages
//! - `MessageUpdate`: DTO for updating existing messages

// Allow dead_code for this module - these types are defined for future use
// but not yet integrated into the routes/handlers
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_CONTENT_LENGTH: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MessageFlags(u32);

impl MessageFlags {
    pub const PINNED: MessageFlags = MessageFlags(1 << 0);
    pub const SUPPRESS_EMBEDS: MessageFlags = MessageFlags(1 << 1);
    pub const EPHEMERAL: MessageFlags = MessageFlags(1 << 2);
    pub const URGENT: MessageFlags = MessageFlags(1 << 3);
    pub const SILENT: MessageFlags = MessageFlags(1 << 4);
    pub const SYSTEM: MessageFlags = MessageFlags(1 << 5);
    pub const CROSSPOST: MessageFlags = MessageFlags(1 << 6);

    pub const fn empty() -> Self {
        MessageFlags(0)
    }

    pub const fn from_bits(bits: u32) -> Self {
        MessageFlags(bits)
    }

    pub const fn bits(&self) -> u32 {
        self.0
    }

    pub const fn contains(&self, other: MessageFlags) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn insert(&mut self, other: MessageFlags) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: MessageFlags) {
        self.0 &= !other.0;
    }

    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for MessageFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        MessageFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for MessageFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        MessageFlags(self.0 & rhs.0)
    }
}

impl std::ops::BitOrAssign for MessageFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::Not for MessageFlags {
    type Output = Self;

    fn not(self) -> Self::Output {
        MessageFlags(!self.0)
    }
}

impl From<u32> for MessageFlags {
    fn from(bits: u32) -> Self {
        MessageFlags(bits)
    }
}

impl From<i64> for MessageFlags {
    fn from(bits: i64) -> Self {
        MessageFlags(bits as u32)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub channel_id: String,
    pub author_jid: String,
    pub content: Option<String>,
    pub reply_to_id: Option<String>,
    pub thread_id: Option<String>,
    pub flags: MessageFlags,
    pub edited_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl Message {
    pub fn is_pinned(&self) -> bool {
        self.flags.contains(MessageFlags::PINNED)
    }

    pub fn is_system(&self) -> bool {
        self.flags.contains(MessageFlags::SYSTEM)
    }

    pub fn is_ephemeral(&self) -> bool {
        self.flags.contains(MessageFlags::EPHEMERAL)
    }

    pub fn is_silent(&self) -> bool {
        self.flags.contains(MessageFlags::SILENT)
    }

    pub fn is_edited(&self) -> bool {
        self.edited_at.is_some()
    }

    pub fn is_reply(&self) -> bool {
        self.reply_to_id.is_some()
    }

    pub fn is_in_thread(&self) -> bool {
        self.thread_id.is_some()
    }

    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(expires) => Utc::now() >= expires,
            None => false,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MessageCreate {
    pub channel_id: String,
    pub author_jid: String,
    pub content: String,
    pub reply_to_id: Option<String>,
    pub thread_id: Option<String>,
    pub flags: MessageFlags,
    pub expires_at: Option<DateTime<Utc>>,
}

impl MessageCreate {
    pub fn new(channel_id: String, author_jid: String, content: String) -> Self {
        Self {
            channel_id,
            author_jid,
            content,
            reply_to_id: None,
            thread_id: None,
            flags: MessageFlags::empty(),
            expires_at: None,
        }
    }

    pub fn with_reply_to(mut self, reply_to_id: String) -> Self {
        self.reply_to_id = Some(reply_to_id);
        self
    }

    pub fn with_thread_id(mut self, thread_id: String) -> Self {
        self.thread_id = Some(thread_id);
        self
    }

    pub fn with_flags(mut self, flags: MessageFlags) -> Self {
        self.flags = flags;
        self
    }

    pub fn with_expiration(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn validate(&self) -> Result<(), super::MessageError> {
        if self.content.len() > MAX_CONTENT_LENGTH {
            return Err(super::MessageError::ContentTooLong {
                max: MAX_CONTENT_LENGTH,
                actual: self.content.len(),
            });
        }
        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct MessageUpdate {
    pub content: Option<String>,
    pub flags: Option<MessageFlags>,
}

impl MessageUpdate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_content(mut self, content: String) -> Self {
        self.content = Some(content);
        self
    }

    pub fn with_flags(mut self, flags: MessageFlags) -> Self {
        self.flags = Some(flags);
        self
    }

    pub fn validate(&self) -> Result<(), super::MessageError> {
        if let Some(ref content) = self.content {
            if content.len() > MAX_CONTENT_LENGTH {
                return Err(super::MessageError::ContentTooLong {
                    max: MAX_CONTENT_LENGTH,
                    actual: content.len(),
                });
            }
        }
        Ok(())
    }

    pub fn has_changes(&self) -> bool {
        self.content.is_some() || self.flags.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_flags_basic() {
        let flags = MessageFlags::empty();
        assert!(flags.is_empty());
        assert_eq!(flags.bits(), 0);

        let pinned = MessageFlags::PINNED;
        assert!(!pinned.is_empty());
        assert!(pinned.contains(MessageFlags::PINNED));
        assert!(!pinned.contains(MessageFlags::SILENT));
    }

    #[test]
    fn test_message_flags_combine() {
        let flags = MessageFlags::PINNED | MessageFlags::URGENT | MessageFlags::SILENT;
        assert!(flags.contains(MessageFlags::PINNED));
        assert!(flags.contains(MessageFlags::URGENT));
        assert!(flags.contains(MessageFlags::SILENT));
        assert!(!flags.contains(MessageFlags::EPHEMERAL));
        assert!(!flags.contains(MessageFlags::SYSTEM));
    }

    #[test]
    fn test_message_flags_insert_remove() {
        let mut flags = MessageFlags::PINNED;
        flags.insert(MessageFlags::URGENT);
        assert!(flags.contains(MessageFlags::PINNED));
        assert!(flags.contains(MessageFlags::URGENT));

        flags.remove(MessageFlags::PINNED);
        assert!(!flags.contains(MessageFlags::PINNED));
        assert!(flags.contains(MessageFlags::URGENT));
    }

    #[test]
    fn test_message_flags_bits() {
        assert_eq!(MessageFlags::PINNED.bits(), 1);
        assert_eq!(MessageFlags::SUPPRESS_EMBEDS.bits(), 2);
        assert_eq!(MessageFlags::EPHEMERAL.bits(), 4);
        assert_eq!(MessageFlags::URGENT.bits(), 8);
        assert_eq!(MessageFlags::SILENT.bits(), 16);
        assert_eq!(MessageFlags::SYSTEM.bits(), 32);
        assert_eq!(MessageFlags::CROSSPOST.bits(), 64);
    }

    #[test]
    fn test_message_create_validation() {
        let create = MessageCreate::new(
            "channel-123".to_string(),
            "user-alice".to_string(),
            "Hello".to_string(),
        );
        assert!(create.validate().is_ok());

        let long_content = "x".repeat(MAX_CONTENT_LENGTH + 1);
        let create_long = MessageCreate::new(
            "channel-123".to_string(),
            "user-alice".to_string(),
            long_content,
        );
        assert!(matches!(
            create_long.validate(),
            Err(super::super::MessageError::ContentTooLong { .. })
        ));
    }

    #[test]
    fn test_message_create_builder() {
        let create = MessageCreate::new(
            "channel-123".to_string(),
            "user-alice".to_string(),
            "Hello".to_string(),
        )
        .with_reply_to("msg-456".to_string())
        .with_thread_id("thread-789".to_string())
        .with_flags(MessageFlags::URGENT);

        assert_eq!(create.reply_to_id, Some("msg-456".to_string()));
        assert_eq!(create.thread_id, Some("thread-789".to_string()));
        assert!(create.flags.contains(MessageFlags::URGENT));
    }

    #[test]
    fn test_message_update_validation() {
        let update = MessageUpdate::new().with_content("Updated content".to_string());
        assert!(update.validate().is_ok());
        assert!(update.has_changes());

        let empty_update = MessageUpdate::new();
        assert!(!empty_update.has_changes());

        let long_content = "x".repeat(MAX_CONTENT_LENGTH + 1);
        let update_long = MessageUpdate::new().with_content(long_content);
        assert!(matches!(
            update_long.validate(),
            Err(super::super::MessageError::ContentTooLong { .. })
        ));
    }

    #[test]
    fn test_message_helper_methods() {
        let message = Message {
            id: "msg-123".to_string(),
            channel_id: "channel-456".to_string(),
            author_jid: "alice@example.com".to_string(),
            content: Some("Hello".to_string()),
            reply_to_id: Some("msg-parent".to_string()),
            thread_id: Some("thread-root".to_string()),
            flags: MessageFlags::PINNED | MessageFlags::SYSTEM,
            edited_at: Some(Utc::now()),
            created_at: Utc::now(),
            expires_at: None,
        };

        assert!(message.is_pinned());
        assert!(message.is_system());
        assert!(!message.is_ephemeral());
        assert!(!message.is_silent());
        assert!(message.is_edited());
        assert!(message.is_reply());
        assert!(message.is_in_thread());
        assert!(!message.is_expired());
    }
}
