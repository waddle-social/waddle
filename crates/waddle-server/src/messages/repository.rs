//! Message repository for Waddle Server
//!
//! Provides CRUD operations for messages stored in per-Waddle databases.

// Allow dead_code for this module - these types are defined for future use
// but not yet integrated into the routes/handlers
#![allow(dead_code)]

use super::types::{Message, MessageCreate, MessageFlags, MessageUpdate};
use super::MessageError;
use crate::db::Database;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tracing::{debug, instrument};
use uuid::Uuid;

/// Repository for message CRUD operations
#[allow(dead_code)]
pub struct MessageRepository {
    db: Arc<Database>,
}

impl MessageRepository {
    /// Create a new message repository
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Create a new message
    #[instrument(skip(self, create))]
    pub async fn create(&self, create: MessageCreate) -> Result<Message, MessageError> {
        // Validate content
        create.validate()?;

        // Generate UUID v7 for time-sortable IDs
        let id = Uuid::now_v7().to_string();
        let created_at = Utc::now();

        let content = create.content.clone();
        let flags_bits = create.flags.bits() as i64;
        let expires_at_str = create.expires_at.map(|dt| dt.to_rfc3339());
        let created_at_str = created_at.to_rfc3339();

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(
                r#"
                INSERT INTO messages (
                    id, channel_id, author_did, content, reply_to_id, thread_id,
                    flags, edited_at, created_at, expires_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?)
                "#,
                libsql::params![
                    id.clone(),
                    create.channel_id.clone(),
                    create.author_did.clone(),
                    content.clone(),
                    create.reply_to_id.clone(),
                    create.thread_id.clone(),
                    flags_bits,
                    created_at_str.clone(),
                    expires_at_str.clone()
                ],
            )
            .await
            .map_err(|e| MessageError::DatabaseError(format!("Failed to insert message: {}", e)))?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                MessageError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;
            conn.execute(
                r#"
                INSERT INTO messages (
                    id, channel_id, author_did, content, reply_to_id, thread_id,
                    flags, edited_at, created_at, expires_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?)
                "#,
                libsql::params![
                    id.clone(),
                    create.channel_id.clone(),
                    create.author_did.clone(),
                    content.clone(),
                    create.reply_to_id.clone(),
                    create.thread_id.clone(),
                    flags_bits,
                    created_at_str,
                    expires_at_str
                ],
            )
            .await
            .map_err(|e| MessageError::DatabaseError(format!("Failed to insert message: {}", e)))?;
        }

        debug!("Created message: {}", id);

        Ok(Message {
            id,
            channel_id: create.channel_id,
            author_did: create.author_did,
            content: Some(content),
            reply_to_id: create.reply_to_id,
            thread_id: create.thread_id,
            flags: create.flags,
            edited_at: None,
            created_at,
            expires_at: create.expires_at,
        })
    }

    /// Get a message by ID
    #[instrument(skip(self))]
    pub async fn get_by_id(&self, id: &str) -> Result<Option<Message>, MessageError> {
        let query = r#"
            SELECT id, channel_id, author_did, content, reply_to_id, thread_id,
                   flags, edited_at, created_at, expires_at
            FROM messages
            WHERE id = ?
        "#;

        let row = if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            let mut rows = conn
                .query(query, libsql::params![id])
                .await
                .map_err(|e| MessageError::DatabaseError(format!("Failed to query message: {}", e)))?;

            rows.next().await.map_err(|e| {
                MessageError::DatabaseError(format!("Failed to read message row: {}", e))
            })?
        } else {
            let conn = self.db.connect().map_err(|e| {
                MessageError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;
            let mut rows = conn
                .query(query, libsql::params![id])
                .await
                .map_err(|e| MessageError::DatabaseError(format!("Failed to query message: {}", e)))?;

            rows.next().await.map_err(|e| {
                MessageError::DatabaseError(format!("Failed to read message row: {}", e))
            })?
        };

        match row {
            Some(row) => {
                let message = self.row_to_message(&row)?;
                Ok(Some(message))
            }
            None => Ok(None),
        }
    }

    /// Get messages by channel with pagination
    ///
    /// Returns messages in reverse chronological order (newest first).
    /// Uses cursor-based pagination for efficient retrieval.
    ///
    /// # Arguments
    ///
    /// * `channel_id` - The channel to get messages from
    /// * `limit` - Maximum number of messages to return
    /// * `before_cursor` - Optional cursor (message ID) to paginate before
    ///
    /// # Returns
    ///
    /// A tuple of (messages, next_cursor) where next_cursor is Some if there are more messages
    #[instrument(skip(self))]
    pub async fn get_by_channel(
        &self,
        channel_id: &str,
        limit: usize,
        before_cursor: Option<&str>,
    ) -> Result<(Vec<Message>, Option<String>), MessageError> {
        let limit_plus_one = (limit + 1) as i64;

        let (query, params): (&str, Vec<libsql::Value>) = match before_cursor {
            Some(cursor) => {
                // Get the created_at of the cursor message for proper pagination
                let cursor_query = "SELECT created_at FROM messages WHERE id = ?";
                let cursor_created_at = if let Some(persistent) = self.db.persistent_connection() {
                    let conn = persistent.lock().await;
                    let mut rows = conn
                        .query(cursor_query, libsql::params![cursor])
                        .await
                        .map_err(|e| {
                            MessageError::DatabaseError(format!("Failed to query cursor: {}", e))
                        })?;

                    match rows.next().await.map_err(|e| {
                        MessageError::DatabaseError(format!("Failed to read cursor row: {}", e))
                    })? {
                        Some(row) => {
                            let created_at: String = row.get(0).map_err(|e| {
                                MessageError::DatabaseError(format!(
                                    "Failed to get cursor created_at: {}",
                                    e
                                ))
                            })?;
                            created_at
                        }
                        None => {
                            return Err(MessageError::InvalidId(format!(
                                "Cursor message not found: {}",
                                cursor
                            )))
                        }
                    }
                } else {
                    let conn = self.db.connect().map_err(|e| {
                        MessageError::DatabaseError(format!("Failed to connect to database: {}", e))
                    })?;
                    let mut rows = conn
                        .query(cursor_query, libsql::params![cursor])
                        .await
                        .map_err(|e| {
                            MessageError::DatabaseError(format!("Failed to query cursor: {}", e))
                        })?;

                    match rows.next().await.map_err(|e| {
                        MessageError::DatabaseError(format!("Failed to read cursor row: {}", e))
                    })? {
                        Some(row) => {
                            let created_at: String = row.get(0).map_err(|e| {
                                MessageError::DatabaseError(format!(
                                    "Failed to get cursor created_at: {}",
                                    e
                                ))
                            })?;
                            created_at
                        }
                        None => {
                            return Err(MessageError::InvalidId(format!(
                                "Cursor message not found: {}",
                                cursor
                            )))
                        }
                    }
                };

                (
                    r#"
                    SELECT id, channel_id, author_did, content, reply_to_id, thread_id,
                           flags, edited_at, created_at, expires_at
                    FROM messages
                    WHERE channel_id = ? AND created_at < ?
                    ORDER BY created_at DESC
                    LIMIT ?
                    "#,
                    vec![
                        channel_id.into(),
                        cursor_created_at.into(),
                        limit_plus_one.into(),
                    ],
                )
            }
            None => (
                r#"
                SELECT id, channel_id, author_did, content, reply_to_id, thread_id,
                       flags, edited_at, created_at, expires_at
                FROM messages
                WHERE channel_id = ?
                ORDER BY created_at DESC
                LIMIT ?
                "#,
                vec![channel_id.into(), limit_plus_one.into()],
            ),
        };

        let mut messages = Vec::new();

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            let mut rows = conn.query(query, params).await.map_err(|e| {
                MessageError::DatabaseError(format!("Failed to query messages: {}", e))
            })?;

            while let Some(row) = rows.next().await.map_err(|e| {
                MessageError::DatabaseError(format!("Failed to read message row: {}", e))
            })? {
                messages.push(self.row_to_message(&row)?);
            }
        } else {
            let conn = self.db.connect().map_err(|e| {
                MessageError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;
            let mut rows = conn.query(query, params).await.map_err(|e| {
                MessageError::DatabaseError(format!("Failed to query messages: {}", e))
            })?;

            while let Some(row) = rows.next().await.map_err(|e| {
                MessageError::DatabaseError(format!("Failed to read message row: {}", e))
            })? {
                messages.push(self.row_to_message(&row)?);
            }
        }

        // Check if there are more messages
        let has_more = messages.len() > limit;
        if has_more {
            messages.pop(); // Remove the extra message we fetched
        }

        // Get the cursor for the next page
        let next_cursor = if has_more {
            messages.last().map(|m| m.id.clone())
        } else {
            None
        };

        Ok((messages, next_cursor))
    }

    /// Update a message
    #[instrument(skip(self, update))]
    pub async fn update(&self, id: &str, update: MessageUpdate) -> Result<Message, MessageError> {
        // Validate update
        update.validate()?;

        // First, fetch the existing message
        let existing = self
            .get_by_id(id)
            .await?
            .ok_or_else(|| MessageError::NotFound(id.to_string()))?;

        // Build update query dynamically
        let mut set_clauses = Vec::new();
        let mut params: Vec<libsql::Value> = Vec::new();

        if let Some(ref content) = update.content {
            set_clauses.push("content = ?");
            params.push(content.clone().into());
            // Also update edited_at
            set_clauses.push("edited_at = ?");
            params.push(Utc::now().to_rfc3339().into());
        }

        if let Some(flags) = update.flags {
            set_clauses.push("flags = ?");
            params.push((flags.bits() as i64).into());
        }

        if set_clauses.is_empty() {
            // Nothing to update
            return Ok(existing);
        }

        // Add the WHERE clause parameter
        params.push(id.into());

        let query = format!(
            "UPDATE messages SET {} WHERE id = ?",
            set_clauses.join(", ")
        );

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(&query, params)
                .await
                .map_err(|e| MessageError::DatabaseError(format!("Failed to update message: {}", e)))?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                MessageError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;
            conn.execute(&query, params)
                .await
                .map_err(|e| MessageError::DatabaseError(format!("Failed to update message: {}", e)))?;
        }

        debug!("Updated message: {}", id);

        // Fetch and return the updated message
        self.get_by_id(id)
            .await?
            .ok_or_else(|| MessageError::NotFound(id.to_string()))
    }

    /// Delete a message
    #[instrument(skip(self))]
    pub async fn delete(&self, id: &str) -> Result<(), MessageError> {
        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            let rows_affected = conn
                .execute("DELETE FROM messages WHERE id = ?", libsql::params![id])
                .await
                .map_err(|e| MessageError::DatabaseError(format!("Failed to delete message: {}", e)))?;

            if rows_affected == 0 {
                return Err(MessageError::NotFound(id.to_string()));
            }
        } else {
            let conn = self.db.connect().map_err(|e| {
                MessageError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;
            let rows_affected = conn
                .execute("DELETE FROM messages WHERE id = ?", libsql::params![id])
                .await
                .map_err(|e| MessageError::DatabaseError(format!("Failed to delete message: {}", e)))?;

            if rows_affected == 0 {
                return Err(MessageError::NotFound(id.to_string()));
            }
        }

        debug!("Deleted message: {}", id);
        Ok(())
    }

    /// Convert a database row to a Message
    fn row_to_message(&self, row: &libsql::Row) -> Result<Message, MessageError> {
        let id: String = row.get(0).map_err(|e| {
            MessageError::DatabaseError(format!("Failed to get message id: {}", e))
        })?;

        let channel_id: String = row.get(1).map_err(|e| {
            MessageError::DatabaseError(format!("Failed to get channel_id: {}", e))
        })?;

        let author_did: String = row.get(2).map_err(|e| {
            MessageError::DatabaseError(format!("Failed to get author_did: {}", e))
        })?;

        let content: Option<String> = row.get(3).ok();

        let reply_to_id: Option<String> = row.get(4).ok();

        let thread_id: Option<String> = row.get(5).ok();

        let flags_bits: i64 = row.get(6).unwrap_or(0);
        let flags = MessageFlags::from(flags_bits);

        let edited_at_str: Option<String> = row.get(7).ok();
        let edited_at = edited_at_str
            .map(|s| DateTime::parse_from_rfc3339(&s).map(|dt| dt.with_timezone(&Utc)))
            .transpose()
            .map_err(|e| MessageError::DatabaseError(format!("Failed to parse edited_at: {}", e)))?;

        let created_at_str: String = row.get(8).map_err(|e| {
            MessageError::DatabaseError(format!("Failed to get created_at: {}", e))
        })?;
        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| MessageError::DatabaseError(format!("Failed to parse created_at: {}", e)))?;

        let expires_at_str: Option<String> = row.get(9).ok();
        let expires_at = expires_at_str
            .map(|s| DateTime::parse_from_rfc3339(&s).map(|dt| dt.with_timezone(&Utc)))
            .transpose()
            .map_err(|e| MessageError::DatabaseError(format!("Failed to parse expires_at: {}", e)))?;

        Ok(Message {
            id,
            channel_id,
            author_did,
            content,
            reply_to_id,
            thread_id,
            flags,
            edited_at,
            created_at,
            expires_at,
        })
    }
}
