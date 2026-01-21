//! Messages module for Waddle Server
//!
//! This module provides:
//! - Message domain types (Message, MessageFlags, etc.)
//! - Message repository for CRUD operations
//! - Support for replies, threads, and message flags
//!
//! # Architecture
//!
//! Messages are stored in per-Waddle databases alongside channels. The message
//! repository provides pagination and cursor-based navigation for efficient
//! retrieval of message history.
//!
//! # Example
//!
//! ```ignore
//! use waddle_server::messages::{MessageRepository, MessageCreate};
//!
//! let repo = MessageRepository::new(db);
//! let msg = MessageCreate::new(
//!     "channel-123".to_string(),
//!     "did:plc:alice".to_string(),
//!     "Hello, world!".to_string(),
//! );
//! let message = repo.create(msg).await?;
//! ```

mod repository;
mod types;

pub use repository::MessageRepository;
pub use types::{Message, MessageCreate, MessageFlags, MessageUpdate};

use thiserror::Error;

/// Message-specific errors
#[derive(Error, Debug)]
pub enum MessageError {
    #[error("Message not found: {0}")]
    NotFound(String),

    #[error("Invalid message content: {0}")]
    InvalidContent(String),

    #[error("Channel not found: {0}")]
    ChannelNotFound(String),

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Invalid message ID: {0}")]
    InvalidId(String),

    #[error("Content too long: max {max} characters, got {actual}")]
    ContentTooLong { max: usize, actual: usize },
}

impl From<crate::db::DatabaseError> for MessageError {
    fn from(err: crate::db::DatabaseError) -> Self {
        MessageError::DatabaseError(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, MigrationRunner};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_message_create_and_get() {
        let db = Database::in_memory("test-messages").await.unwrap();
        let db = Arc::new(db);

        // Run per-waddle migrations
        let runner = MigrationRunner::waddle();
        runner.run(&db).await.unwrap();

        let repo = MessageRepository::new(Arc::clone(&db));

        // Create a message
        let create = MessageCreate::new(
            "channel-123".to_string(),
            "did:plc:alice".to_string(),
            "Hello, world!".to_string(),
        );

        let message = repo.create(create).await.unwrap();
        assert_eq!(message.content, Some("Hello, world!".to_string()));
        assert_eq!(message.author_did, "did:plc:alice");
        assert_eq!(message.channel_id, "channel-123");

        // Get the message by ID
        let retrieved = repo.get_by_id(&message.id).await.unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, message.id);
        assert_eq!(retrieved.content, message.content);
    }

    #[tokio::test]
    async fn test_message_pagination() {
        let db = Database::in_memory("test-messages-pagination").await.unwrap();
        let db = Arc::new(db);

        // Run per-waddle migrations
        let runner = MigrationRunner::waddle();
        runner.run(&db).await.unwrap();

        let repo = MessageRepository::new(Arc::clone(&db));

        // Create multiple messages
        for i in 0..10 {
            let create = MessageCreate::new(
                "channel-test".to_string(),
                "did:plc:alice".to_string(),
                format!("Message {}", i),
            );
            repo.create(create).await.unwrap();
        }

        // Get first page
        let (messages, cursor) = repo
            .get_by_channel("channel-test", 5, None)
            .await
            .unwrap();
        assert_eq!(messages.len(), 5);
        assert!(cursor.is_some());

        // Get second page
        let (messages2, cursor2) = repo
            .get_by_channel("channel-test", 5, cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(messages2.len(), 5);
        assert!(cursor2.is_none()); // No more messages
    }

    #[tokio::test]
    async fn test_message_update() {
        let db = Database::in_memory("test-messages-update").await.unwrap();
        let db = Arc::new(db);

        // Run per-waddle migrations
        let runner = MigrationRunner::waddle();
        runner.run(&db).await.unwrap();

        let repo = MessageRepository::new(Arc::clone(&db));

        // Create a message
        let create = MessageCreate::new(
            "channel-123".to_string(),
            "did:plc:alice".to_string(),
            "Original content".to_string(),
        );
        let message = repo.create(create).await.unwrap();

        // Update the message
        let update = MessageUpdate {
            content: Some("Updated content".to_string()),
            flags: None,
        };
        let updated = repo.update(&message.id, update).await.unwrap();
        assert_eq!(updated.content, Some("Updated content".to_string()));
        assert!(updated.edited_at.is_some());
    }

    #[tokio::test]
    async fn test_message_delete() {
        let db = Database::in_memory("test-messages-delete").await.unwrap();
        let db = Arc::new(db);

        // Run per-waddle migrations
        let runner = MigrationRunner::waddle();
        runner.run(&db).await.unwrap();

        let repo = MessageRepository::new(Arc::clone(&db));

        // Create a message
        let create = MessageCreate::new(
            "channel-123".to_string(),
            "did:plc:alice".to_string(),
            "To be deleted".to_string(),
        );
        let message = repo.create(create).await.unwrap();

        // Delete the message
        repo.delete(&message.id).await.unwrap();

        // Verify it's gone
        let retrieved = repo.get_by_id(&message.id).await.unwrap();
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn test_message_with_reply() {
        let db = Database::in_memory("test-messages-reply").await.unwrap();
        let db = Arc::new(db);

        // Run per-waddle migrations
        let runner = MigrationRunner::waddle();
        runner.run(&db).await.unwrap();

        let repo = MessageRepository::new(Arc::clone(&db));

        // Create a parent message
        let parent_create = MessageCreate::new(
            "channel-123".to_string(),
            "did:plc:alice".to_string(),
            "Parent message".to_string(),
        );
        let parent = repo.create(parent_create).await.unwrap();

        // Create a reply
        let reply_create = MessageCreate::new(
            "channel-123".to_string(),
            "did:plc:bob".to_string(),
            "Reply to parent".to_string(),
        )
        .with_reply_to(parent.id.clone());

        let reply = repo.create(reply_create).await.unwrap();
        assert_eq!(reply.reply_to_id, Some(parent.id));
    }

    #[tokio::test]
    async fn test_message_flags() {
        assert_eq!(MessageFlags::PINNED.bits(), 1);
        assert_eq!(MessageFlags::SUPPRESS_EMBEDS.bits(), 2);
        assert_eq!(MessageFlags::EPHEMERAL.bits(), 4);

        let flags = MessageFlags::PINNED | MessageFlags::SILENT;
        assert!(flags.contains(MessageFlags::PINNED));
        assert!(flags.contains(MessageFlags::SILENT));
        assert!(!flags.contains(MessageFlags::URGENT));
    }
}
