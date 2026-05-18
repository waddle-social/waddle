//! Message repository for Waddle Server
//!
//! Provides CRUD operations for messages stored in per-Waddle databases.

// Allow dead_code for this module - these types are defined for future use
// but not yet integrated into the routes/handlers
#![allow(dead_code)]

use super::types::{Message, MessageCreate, MessageFlags, MessageUpdate};
use super::MessageError;
use crate::db::actor::{DbActor, DbExecute, DbQuery, DbQueryOne};
use crate::db::{row_value, ValueExt};
use chrono::{DateTime, Utc};
use kameo::actor::ActorRef;
use tracing::{debug, instrument};
use uuid::Uuid;

/// Repository for message CRUD operations
#[allow(dead_code)]
pub struct MessageRepository {
    actor: ActorRef<DbActor>,
}

impl MessageRepository {
    pub fn new(actor: ActorRef<DbActor>) -> Self {
        Self { actor }
    }

    #[instrument(skip(self, create))]
    pub async fn create(&self, create: MessageCreate) -> Result<Message, MessageError> {
        create.validate()?;

        let id = Uuid::now_v7().to_string();
        let created_at = Utc::now();

        let content = create.content.clone();
        let flags_bits = create.flags.bits() as i64;
        let expires_at_str = create.expires_at.map(|dt| dt.to_rfc3339());
        let created_at_str = created_at.to_rfc3339();

        self.actor
            .ask(DbExecute {
                sql: r#"
                    INSERT INTO messages (
                        id, channel_id, author_user_id, content, reply_to_id, thread_id,
                        flags, edited_at, created_at, expires_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?)
                "#
                .to_string(),
                params: vec![
                    id.clone().into(),
                    create.channel_id.clone().into(),
                    create.author_user_id.clone().into(),
                    content.clone().into(),
                    crate::db::Value::from(create.reply_to_id.clone()),
                    crate::db::Value::from(create.thread_id.clone()),
                    flags_bits.into(),
                    created_at_str.into(),
                    crate::db::Value::from(expires_at_str),
                ],
            })
            .await
            .map_err(|e| MessageError::DatabaseError(format!("Failed to insert message: {}", e)))?;

        debug!("Created message: {}", id);

        Ok(Message {
            id,
            channel_id: create.channel_id,
            author_user_id: create.author_user_id,
            content: Some(content),
            reply_to_id: create.reply_to_id,
            thread_id: create.thread_id,
            flags: create.flags,
            edited_at: None,
            created_at,
            expires_at: create.expires_at,
        })
    }

    #[instrument(skip(self))]
    pub async fn get_by_id(&self, id: &str) -> Result<Option<Message>, MessageError> {
        let query = r#"
            SELECT id, channel_id, author_user_id, content, reply_to_id, thread_id,
                   flags, edited_at, created_at, expires_at
            FROM messages
            WHERE id = ?
        "#;

        let row = self
            .actor
            .ask(DbQueryOne {
                sql: query.to_string(),
                params: vec![id.into()],
            })
            .await
            .map_err(|e| MessageError::DatabaseError(format!("Failed to query message: {}", e)))?;

        match row {
            Some(row) => Ok(Some(self.values_to_message(&row)?)),
            None => Ok(None),
        }
    }

    #[instrument(skip(self))]
    pub async fn get_by_channel(
        &self,
        channel_id: &str,
        limit: usize,
        before_cursor: Option<&str>,
    ) -> Result<(Vec<Message>, Option<String>), MessageError> {
        let limit_plus_one = (limit + 1) as i64;

        let (query, params): (&str, Vec<crate::db::Value>) = match before_cursor {
            Some(cursor) => {
                let cursor_query = "SELECT created_at FROM messages WHERE id = ?";
                let cursor_created_at = match self
                    .actor
                    .ask(DbQueryOne {
                        sql: cursor_query.to_string(),
                        params: vec![cursor.into()],
                    })
                    .await
                    .map_err(|e| {
                        MessageError::DatabaseError(format!("Failed to query cursor: {}", e))
                    })? {
                    Some(row) => row_value(&row, 0)
                        .and_then(ValueExt::as_string)
                        .map_err(|e| {
                            MessageError::DatabaseError(format!(
                                "Failed to get cursor created_at: {}",
                                e
                            ))
                        })?,
                    None => {
                        return Err(MessageError::InvalidId(format!(
                            "Cursor message not found: {}",
                            cursor
                        )))
                    }
                };

                (
                    r#"
                    SELECT id, channel_id, author_user_id, content, reply_to_id, thread_id,
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
                SELECT id, channel_id, author_user_id, content, reply_to_id, thread_id,
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

        let rows = self
            .actor
            .ask(DbQuery {
                sql: query.to_string(),
                params,
            })
            .await
            .map_err(|e| MessageError::DatabaseError(format!("Failed to query messages: {}", e)))?;

        for row in rows {
            messages.push(self.values_to_message(&row)?);
        }

        let has_more = messages.len() > limit;
        if has_more {
            messages.pop();
        }

        let next_cursor = if has_more {
            messages.last().map(|m| m.id.clone())
        } else {
            None
        };

        Ok((messages, next_cursor))
    }

    #[instrument(skip(self, update))]
    pub async fn update(&self, id: &str, update: MessageUpdate) -> Result<Message, MessageError> {
        update.validate()?;

        let existing = self
            .get_by_id(id)
            .await?
            .ok_or_else(|| MessageError::NotFound(id.to_string()))?;

        let mut set_clauses = Vec::new();
        let mut params: Vec<crate::db::Value> = Vec::new();

        if let Some(ref content) = update.content {
            set_clauses.push("content = ?");
            params.push(content.clone().into());
            set_clauses.push("edited_at = ?");
            params.push(Utc::now().to_rfc3339().into());
        }

        if let Some(flags) = update.flags {
            set_clauses.push("flags = ?");
            params.push((flags.bits() as i64).into());
        }

        if set_clauses.is_empty() {
            return Ok(existing);
        }

        params.push(id.into());

        let query = format!(
            "UPDATE messages SET {} WHERE id = ?",
            set_clauses.join(", ")
        );

        self.actor
            .ask(DbExecute { sql: query, params })
            .await
            .map_err(|e| MessageError::DatabaseError(format!("Failed to update message: {}", e)))?;

        debug!("Updated message: {}", id);

        self.get_by_id(id)
            .await?
            .ok_or_else(|| MessageError::NotFound(id.to_string()))
    }

    #[instrument(skip(self))]
    pub async fn delete(&self, id: &str) -> Result<(), MessageError> {
        let rows_affected = self
            .actor
            .ask(DbExecute {
                sql: "DELETE FROM messages WHERE id = ?".to_string(),
                params: vec![id.into()],
            })
            .await
            .map_err(|e| MessageError::DatabaseError(format!("Failed to delete message: {}", e)))?;

        if rows_affected == 0 {
            return Err(MessageError::NotFound(id.to_string()));
        }

        debug!("Deleted message: {}", id);
        Ok(())
    }

    fn values_to_message(&self, row: &[crate::db::Value]) -> Result<Message, MessageError> {
        let id = row_value(row, 0)
            .and_then(ValueExt::as_string)
            .map_err(|e| MessageError::DatabaseError(format!("Failed to get message id: {}", e)))?;

        let channel_id = row_value(row, 1)
            .and_then(ValueExt::as_string)
            .map_err(|e| MessageError::DatabaseError(format!("Failed to get channel_id: {}", e)))?;

        let author_user_id = row_value(row, 2)
            .and_then(ValueExt::as_string)
            .map_err(|e| {
                MessageError::DatabaseError(format!("Failed to get author_user_id: {}", e))
            })?;

        let content = row_value(row, 3)
            .and_then(ValueExt::as_optional_string)
            .ok()
            .flatten();

        let reply_to_id = row_value(row, 4)
            .and_then(ValueExt::as_optional_string)
            .ok()
            .flatten();

        let thread_id = row_value(row, 5)
            .and_then(ValueExt::as_optional_string)
            .ok()
            .flatten();

        let flags_bits = match row_value(row, 6) {
            Ok(crate::db::Value::Integer(v)) => *v,
            _ => 0,
        };
        let flags = MessageFlags::from(flags_bits);

        let edited_at_str = row_value(row, 7)
            .and_then(ValueExt::as_optional_string)
            .ok()
            .flatten();
        let edited_at = edited_at_str
            .map(|s| DateTime::parse_from_rfc3339(&s).map(|dt| dt.with_timezone(&Utc)))
            .transpose()
            .map_err(|e| {
                MessageError::DatabaseError(format!("Failed to parse edited_at: {}", e))
            })?;

        let created_at_str = row_value(row, 8)
            .and_then(ValueExt::as_string)
            .map_err(|e| MessageError::DatabaseError(format!("Failed to get created_at: {}", e)))?;
        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| {
                MessageError::DatabaseError(format!("Failed to parse created_at: {}", e))
            })?;

        let expires_at_str = row_value(row, 9)
            .and_then(ValueExt::as_optional_string)
            .ok()
            .flatten();
        let expires_at = expires_at_str
            .map(|s| DateTime::parse_from_rfc3339(&s).map(|dt| dt.with_timezone(&Utc)))
            .transpose()
            .map_err(|e| {
                MessageError::DatabaseError(format!("Failed to parse expires_at: {}", e))
            })?;

        Ok(Message {
            id,
            channel_id,
            author_user_id,
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
