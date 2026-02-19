//! MAM storage trait and libSQL implementation.
//!
//! Provides persistent storage for archived messages (XEP-0313).
//! The storage layer supports:
//! - Storing messages with unique archive IDs
//! - Querying with time-based and sender filters
//! - RSM (Result Set Management) pagination

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use libsql::Connection;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, instrument};
use uuid::Uuid;

use super::{ArchivedMessage, MamQuery, MamResult};

/// Errors that can occur during MAM storage operations.
#[derive(Error, Debug)]
pub enum MamStorageError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Message not found: {0}")]
    NotFound(String),

    #[error("Invalid query parameter: {0}")]
    InvalidQuery(String),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<libsql::Error> for MamStorageError {
    fn from(e: libsql::Error) -> Self {
        MamStorageError::Database(e.to_string())
    }
}

/// Trait for MAM message storage backends.
#[async_trait]
pub trait MamStorage: Send + Sync {
    /// Store a message in the archive.
    ///
    /// The `archive_jid` identifies which archive to store in:
    /// - For MUC messages: the room bare JID
    /// - For 1:1 messages: the user's bare JID (personal archive)
    ///
    /// Returns the unique archive ID assigned to the message.
    async fn store_message(
        &self,
        archive_jid: &str,
        message: &ArchivedMessage,
    ) -> Result<String, MamStorageError>;

    /// Query messages from the archive.
    ///
    /// The `archive_jid` identifies which archive to query:
    /// - For MUC archives: the room bare JID
    /// - For personal archives: the user's bare JID
    ///
    /// Supports filtering by time range, sender, and RSM pagination.
    async fn query_messages(
        &self,
        archive_jid: &str,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError>;

    /// Get a single message by its archive ID.
    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get the total count of messages in an archive (for RSM).
    async fn count_messages(&self, room_jid: &str) -> Result<u32, MamStorageError>;

    /// Delete messages older than a given timestamp.
    ///
    /// Used for archive maintenance/cleanup.
    async fn delete_before(
        &self,
        room_jid: &str,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError>;
}

/// libSQL-based MAM storage implementation.
///
/// Uses an in-memory or file-based libSQL database for message archival.
/// Designed to work with the existing Waddle database infrastructure.
#[derive(Clone)]
pub struct LibSqlMamStorage {
    /// Database connection.
    /// For in-memory databases, this must be a persistent connection.
    conn: Arc<Mutex<Connection>>,
    /// Whether the schema has been initialized.
    initialized: Arc<std::sync::atomic::AtomicBool>,
}

impl LibSqlMamStorage {
    /// Create a new libSQL MAM storage with the given connection.
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Create from an Arc<Mutex<Connection>> (for sharing with other components).
    pub fn from_shared(conn: Arc<Mutex<Connection>>) -> Self {
        Self {
            conn,
            initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Initialize the database schema if not already done.
    #[instrument(skip(self))]
    pub async fn initialize(&self) -> Result<(), MamStorageError> {
        if self.initialized.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }

        let conn = self.conn.lock().await;

        // Create the message archive table
        conn.execute_batch(MAM_SCHEMA).await?;
        Self::migrate_schema(&conn).await?;

        self.initialized
            .store(true, std::sync::atomic::Ordering::Release);
        debug!("MAM storage schema initialized");

        Ok(())
    }

    /// Generate a time-sortable archive ID using UUID v7.
    fn generate_archive_id() -> String {
        Uuid::now_v7().to_string()
    }

    async fn migrate_schema(conn: &Connection) -> Result<(), MamStorageError> {
        Self::ensure_column(
            conn,
            "thread_id",
            "ALTER TABLE mam_messages ADD COLUMN thread_id TEXT",
        )
        .await?;
        Self::ensure_column(
            conn,
            "reply_to_id",
            "ALTER TABLE mam_messages ADD COLUMN reply_to_id TEXT",
        )
        .await?;
        Self::ensure_column(
            conn,
            "reply_to_jid",
            "ALTER TABLE mam_messages ADD COLUMN reply_to_jid TEXT",
        )
        .await?;
        Self::ensure_column(
            conn,
            "origin_id",
            "ALTER TABLE mam_messages ADD COLUMN origin_id TEXT",
        )
        .await?;
        Self::ensure_column(
            conn,
            "message_type",
            "ALTER TABLE mam_messages ADD COLUMN message_type TEXT",
        )
        .await?;

        conn.execute_batch(
            r#"
            CREATE INDEX IF NOT EXISTS idx_mam_room_thread
                ON mam_messages(room_jid, thread_id, timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_mam_room_reply_to
                ON mam_messages(room_jid, reply_to_id, timestamp DESC);
            "#,
        )
        .await?;

        // Backfill nullable message_type for rows created before this column existed.
        conn.execute(
            "UPDATE mam_messages SET message_type = 'chat' WHERE message_type IS NULL",
            (),
        )
        .await?;

        Ok(())
    }

    async fn ensure_column(
        conn: &Connection,
        column_name: &str,
        alter_statement: &str,
    ) -> Result<(), MamStorageError> {
        let mut rows = conn.query("PRAGMA table_info(mam_messages)", ()).await?;
        let mut exists = false;

        while let Some(row) = rows.next().await? {
            let name: String = row.get(1)?;
            if name == column_name {
                exists = true;
                break;
            }
        }

        if !exists {
            conn.execute(alter_statement, ()).await?;
        }

        Ok(())
    }
}

/// SQL schema for MAM message storage.
pub const MAM_SCHEMA: &str = r#"
-- Message Archive Management (MAM) table for XEP-0313
CREATE TABLE IF NOT EXISTS mam_messages (
    -- Primary key: UUID v7 (time-sortable)
    id TEXT PRIMARY KEY,
    -- Room/archive JID this message belongs to
    room_jid TEXT NOT NULL,
    -- Timestamp when the message was archived
    timestamp TEXT NOT NULL,
    -- Sender JID (full JID with resource/nick)
    from_jid TEXT NOT NULL,
    -- Recipient JID (room JID for MUC)
    to_jid TEXT NOT NULL,
    -- Message body content
    body TEXT NOT NULL,
    -- Original stanza-id from the client (if any)
    stanza_id TEXT,
    -- RFC 6121 thread identifier
    thread_id TEXT,
    -- XEP-0461 reply target message ID
    reply_to_id TEXT,
    -- XEP-0461 optional original sender JID
    reply_to_jid TEXT,
    -- XEP-0359 origin-id from client
    origin_id TEXT,
    -- Message type ("chat", "groupchat", ...)
    message_type TEXT NOT NULL DEFAULT 'chat',
    -- Created timestamp for internal tracking
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Index for querying messages by room with timestamp ordering (most common query)
CREATE INDEX IF NOT EXISTS idx_mam_room_timestamp
    ON mam_messages(room_jid, timestamp DESC);

-- Index for querying by sender within a room
CREATE INDEX IF NOT EXISTS idx_mam_room_sender
    ON mam_messages(room_jid, from_jid, timestamp DESC);

-- Index for pagination by ID (for before_id/after_id queries)
CREATE INDEX IF NOT EXISTS idx_mam_room_id
    ON mam_messages(room_jid, id);

-- Index for thread message retrieval
CREATE INDEX IF NOT EXISTS idx_mam_room_thread
    ON mam_messages(room_jid, thread_id, timestamp DESC);

-- Index for reply message lookup
CREATE INDEX IF NOT EXISTS idx_mam_room_reply_to
    ON mam_messages(room_jid, reply_to_id, timestamp DESC);
"#;

#[async_trait]
impl MamStorage for LibSqlMamStorage {
    #[instrument(skip(self, message), fields(archive = %archive_jid))]
    async fn store_message(
        &self,
        archive_jid: &str,
        message: &ArchivedMessage,
    ) -> Result<String, MamStorageError> {
        self.initialize().await?;

        // Use provided ID or generate a new one
        let archive_id = if message.id.is_empty() {
            Self::generate_archive_id()
        } else {
            message.id.clone()
        };

        let conn = self.conn.lock().await;

        let timestamp = message.timestamp.to_rfc3339();
        let stanza_id = message.stanza_id.as_deref();
        let thread_id = message.thread_id.as_deref();
        let reply_to_id = message.reply_to_id.as_deref();
        let reply_to_jid = message.reply_to_jid.as_deref();
        let origin_id = message.origin_id.as_deref();
        let message_type = if message.message_type.is_empty() {
            "chat"
        } else {
            message.message_type.as_str()
        };

        conn.execute(
            r#"
            INSERT INTO mam_messages (
                id, room_jid, timestamp, from_jid, to_jid, body, stanza_id,
                thread_id, reply_to_id, reply_to_jid, origin_id, message_type
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            (
                archive_id.as_str(),
                archive_jid,
                timestamp.as_str(),
                message.from.as_str(),
                message.to.as_str(),
                message.body.as_str(),
                stanza_id,
                thread_id,
                reply_to_id,
                reply_to_jid,
                origin_id,
                message_type,
            ),
        )
        .await?;

        debug!(archive_id = %archive_id, "Message stored in MAM archive");

        Ok(archive_id)
    }

    #[instrument(skip(self), fields(room = %room_jid))]
    async fn query_messages(
        &self,
        room_jid: &str,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        self.initialize().await?;

        let conn = self.conn.lock().await;

        // Build the query dynamically based on filters
        let mut sql = String::from(
            r#"
            SELECT
                id, room_jid, timestamp, from_jid, to_jid, body, stanza_id,
                thread_id, reply_to_id, reply_to_jid, origin_id, message_type
            FROM mam_messages
            WHERE room_jid = ?1
            "#,
        );

        let mut param_index = 2;
        let mut conditions = Vec::new();
        let mut params: Vec<String> = vec![room_jid.to_string()];

        // Time range filters
        if let Some(ref start) = query.start {
            conditions.push(format!("timestamp >= ?{}", param_index));
            params.push(start.to_rfc3339());
            param_index += 1;
        }

        if let Some(ref end) = query.end {
            conditions.push(format!("timestamp <= ?{}", param_index));
            params.push(end.to_rfc3339());
            param_index += 1;
        }

        // "with" filter: matches either sender or recipient (for personal archives,
        // this filters by conversation partner; for MUC archives, by sender).
        if let Some(ref with) = query.with {
            conditions.push(format!(
                "(from_jid LIKE ?{idx} OR to_jid LIKE ?{idx})",
                idx = param_index
            ));
            params.push(format!("{}%", with));
            param_index += 1;
        }

        // Pagination filters (before_id/after_id)
        if let Some(ref before_id) = query.before_id {
            conditions.push(format!("id < ?{}", param_index));
            params.push(before_id.clone());
            param_index += 1;
        }

        if let Some(ref after_id) = query.after_id {
            conditions.push(format!("id > ?{}", param_index));
            params.push(after_id.clone());
            // param_index += 1; // Not needed after last use
        }

        // Add conditions to SQL
        for condition in conditions {
            sql.push_str(" AND ");
            sql.push_str(&condition);
        }

        // Order by timestamp (ascending for forward pagination, descending for backward)
        if query.before_id.is_some() {
            sql.push_str(" ORDER BY id DESC");
        } else {
            sql.push_str(" ORDER BY id ASC");
        }

        // Limit results (add 1 to check if there are more)
        let limit = query.max.unwrap_or(100).min(500) + 1;
        sql.push_str(&format!(" LIMIT {}", limit));

        debug!(sql = %sql, params = ?params, "Executing MAM query");

        // Execute the query with dynamic parameters
        // Convert Vec<String> to Vec<&str> for libsql
        let params_refs: Vec<&str> = params.iter().map(|s| s.as_str()).collect();
        let mut rows = match params_refs.len() {
            1 => conn.query(&sql, [params_refs[0]]).await?,
            2 => conn.query(&sql, [params_refs[0], params_refs[1]]).await?,
            3 => {
                conn.query(&sql, [params_refs[0], params_refs[1], params_refs[2]])
                    .await?
            }
            4 => {
                conn.query(
                    &sql,
                    [
                        params_refs[0],
                        params_refs[1],
                        params_refs[2],
                        params_refs[3],
                    ],
                )
                .await?
            }
            5 => {
                conn.query(
                    &sql,
                    [
                        params_refs[0],
                        params_refs[1],
                        params_refs[2],
                        params_refs[3],
                        params_refs[4],
                    ],
                )
                .await?
            }
            6 => {
                conn.query(
                    &sql,
                    [
                        params_refs[0],
                        params_refs[1],
                        params_refs[2],
                        params_refs[3],
                        params_refs[4],
                        params_refs[5],
                    ],
                )
                .await?
            }
            _ => {
                return Err(MamStorageError::InvalidQuery(
                    "Too many query parameters".to_string(),
                ))
            }
        };

        let mut messages = Vec::new();

        while let Some(row) = rows.next().await? {
            let id: String = row.get(0)?;
            let timestamp_str: String = row.get(2)?;
            let from: String = row.get(3)?;
            let to: String = row.get(4)?;
            let body: String = row.get(5)?;
            let stanza_id: Option<String> = row.get(6).ok();
            let thread_id: Option<String> = row.get(7).ok();
            let reply_to_id: Option<String> = row.get(8).ok();
            let reply_to_jid: Option<String> = row.get(9).ok();
            let origin_id: Option<String> = row.get(10).ok();
            let message_type: String = row.get(11).unwrap_or_else(|_| "chat".to_string());

            let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
                .map_err(|e| MamStorageError::Serialization(format!("Invalid timestamp: {}", e)))?
                .with_timezone(&Utc);

            messages.push(ArchivedMessage {
                id,
                timestamp,
                from,
                to,
                body,
                stanza_id,
                thread_id,
                reply_to_id,
                reply_to_jid,
                origin_id,
                message_type,
            });
        }

        // Check if there are more results
        let actual_limit = query.max.unwrap_or(100).min(500) as usize;
        let complete = messages.len() <= actual_limit;

        // Remove the extra message if we fetched one more than requested
        if messages.len() > actual_limit {
            messages.pop();
        }

        // Reverse if we were paginating backwards
        if query.before_id.is_some() {
            messages.reverse();
        }

        let first_id = messages.first().map(|m| m.id.clone());
        let last_id = messages.last().map(|m| m.id.clone());

        debug!(
            message_count = messages.len(),
            complete = complete,
            "MAM query completed"
        );

        Ok(MamResult {
            messages,
            complete,
            first_id,
            last_id,
            count: None, // Count is optional and expensive
        })
    }

    #[instrument(skip(self))]
    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.initialize().await?;

        let conn = self.conn.lock().await;

        let mut rows = conn
            .query(
                r#"
                SELECT
                    id, room_jid, timestamp, from_jid, to_jid, body, stanza_id,
                    thread_id, reply_to_id, reply_to_jid, origin_id, message_type
                FROM mam_messages
                WHERE id = ?1
                "#,
                [archive_id],
            )
            .await?;

        if let Some(row) = rows.next().await? {
            let id: String = row.get(0)?;
            let timestamp_str: String = row.get(2)?;
            let from: String = row.get(3)?;
            let to: String = row.get(4)?;
            let body: String = row.get(5)?;
            let stanza_id: Option<String> = row.get(6).ok();
            let thread_id: Option<String> = row.get(7).ok();
            let reply_to_id: Option<String> = row.get(8).ok();
            let reply_to_jid: Option<String> = row.get(9).ok();
            let origin_id: Option<String> = row.get(10).ok();
            let message_type: String = row.get(11).unwrap_or_else(|_| "chat".to_string());

            let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
                .map_err(|e| MamStorageError::Serialization(format!("Invalid timestamp: {}", e)))?
                .with_timezone(&Utc);

            Ok(Some(ArchivedMessage {
                id,
                timestamp,
                from,
                to,
                body,
                stanza_id,
                thread_id,
                reply_to_id,
                reply_to_jid,
                origin_id,
                message_type,
            }))
        } else {
            Ok(None)
        }
    }

    #[instrument(skip(self))]
    async fn count_messages(&self, room_jid: &str) -> Result<u32, MamStorageError> {
        self.initialize().await?;

        let conn = self.conn.lock().await;

        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ?1",
                [room_jid],
            )
            .await?;

        if let Some(row) = rows.next().await? {
            let count: i64 = row.get(0)?;
            Ok(count as u32)
        } else {
            Ok(0)
        }
    }

    #[instrument(skip(self))]
    async fn delete_before(
        &self,
        room_jid: &str,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError> {
        self.initialize().await?;

        let conn = self.conn.lock().await;

        let before_str = before.to_rfc3339();

        let deleted = conn
            .execute(
                "DELETE FROM mam_messages WHERE room_jid = ?1 AND timestamp < ?2",
                [room_jid, &before_str],
            )
            .await?;

        debug!(room = %room_jid, deleted = deleted, "Deleted old messages from MAM archive");

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_test_storage() -> LibSqlMamStorage {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        LibSqlMamStorage::new(conn)
    }

    #[tokio::test]
    async fn test_store_and_retrieve_message() {
        let storage = create_test_storage().await;

        let msg = ArchivedMessage {
            id: String::new(), // Let storage generate ID
            timestamp: Utc::now(),
            from: "user@example.com/nick".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "Hello, world!".to_string(),
            stanza_id: Some("abc123".to_string()),
            ..Default::default()
        };

        let archive_id = storage
            .store_message("room@conference.example.com", &msg)
            .await
            .unwrap();
        assert!(!archive_id.is_empty());

        let retrieved = storage.get_message(&archive_id).await.unwrap();
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, archive_id);
        assert_eq!(retrieved.body, "Hello, world!");
        assert_eq!(retrieved.stanza_id, Some("abc123".to_string()));
    }

    #[tokio::test]
    async fn test_store_and_retrieve_reply_thread_metadata() {
        let storage = create_test_storage().await;

        let msg = ArchivedMessage {
            id: String::new(),
            timestamp: Utc::now(),
            from: "room@conference.example.com/alice".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "Reply body".to_string(),
            stanza_id: Some("archive-stanza-1".to_string()),
            thread_id: Some("thread-root-1".to_string()),
            reply_to_id: Some("parent-message-1".to_string()),
            reply_to_jid: Some("bob@example.com".to_string()),
            origin_id: Some("origin-abc".to_string()),
            message_type: "groupchat".to_string(),
        };

        let archive_id = storage
            .store_message("room@conference.example.com", &msg)
            .await
            .unwrap();

        let retrieved = storage
            .get_message(&archive_id)
            .await
            .unwrap()
            .expect("archived message");

        assert_eq!(retrieved.thread_id.as_deref(), Some("thread-root-1"));
        assert_eq!(retrieved.reply_to_id.as_deref(), Some("parent-message-1"));
        assert_eq!(retrieved.reply_to_jid.as_deref(), Some("bob@example.com"));
        assert_eq!(retrieved.origin_id.as_deref(), Some("origin-abc"));
        assert_eq!(retrieved.message_type, "groupchat");
    }

    #[tokio::test]
    async fn test_query_messages_by_room() {
        let storage = create_test_storage().await;

        let room = "room@conference.example.com";

        // Store multiple messages
        for i in 0..5 {
            let msg = ArchivedMessage {
                id: String::new(),
                timestamp: Utc::now(),
                from: format!("user{}@example.com/nick", i),
                to: room.to_string(),
                body: format!("Message {}", i),
                stanza_id: None,
                ..Default::default()
            };
            storage.store_message(room, &msg).await.unwrap();
        }

        // Query all messages
        let result = storage
            .query_messages(room, &MamQuery::default())
            .await
            .unwrap();
        assert_eq!(result.messages.len(), 5);
        assert!(result.complete);
    }

    #[tokio::test]
    async fn test_query_with_limit() {
        let storage = create_test_storage().await;

        let room = "room@conference.example.com";

        // Store 10 messages
        for i in 0..10 {
            let msg = ArchivedMessage {
                id: String::new(),
                timestamp: Utc::now(),
                from: "user@example.com/nick".to_string(),
                to: room.to_string(),
                body: format!("Message {}", i),
                stanza_id: None,
                ..Default::default()
            };
            storage.store_message(room, &msg).await.unwrap();
        }

        // Query with limit
        let query = MamQuery {
            max: Some(5),
            ..Default::default()
        };
        let result = storage.query_messages(room, &query).await.unwrap();
        assert_eq!(result.messages.len(), 5);
        assert!(!result.complete); // There are more messages
    }

    #[tokio::test]
    async fn test_count_messages() {
        let storage = create_test_storage().await;

        let room = "room@conference.example.com";

        // Store messages
        for i in 0..7 {
            let msg = ArchivedMessage {
                id: String::new(),
                timestamp: Utc::now(),
                from: "user@example.com/nick".to_string(),
                to: room.to_string(),
                body: format!("Message {}", i),
                stanza_id: None,
                ..Default::default()
            };
            storage.store_message(room, &msg).await.unwrap();
        }

        let count = storage.count_messages(room).await.unwrap();
        assert_eq!(count, 7);
    }

    #[tokio::test]
    async fn test_delete_before() {
        let storage = create_test_storage().await;

        let room = "room@conference.example.com";
        let old_time = Utc::now() - chrono::Duration::hours(2);
        let new_time = Utc::now();

        // Store old messages
        for i in 0..3 {
            let msg = ArchivedMessage {
                id: String::new(),
                timestamp: old_time,
                from: "user@example.com/nick".to_string(),
                to: room.to_string(),
                body: format!("Old message {}", i),
                stanza_id: None,
                ..Default::default()
            };
            storage.store_message(room, &msg).await.unwrap();
        }

        // Store new messages
        for i in 0..2 {
            let msg = ArchivedMessage {
                id: String::new(),
                timestamp: new_time,
                from: "user@example.com/nick".to_string(),
                to: room.to_string(),
                body: format!("New message {}", i),
                stanza_id: None,
                ..Default::default()
            };
            storage.store_message(room, &msg).await.unwrap();
        }

        // Delete old messages
        let cutoff = Utc::now() - chrono::Duration::hours(1);
        let deleted = storage.delete_before(room, cutoff).await.unwrap();
        assert_eq!(deleted, 3);

        // Verify only new messages remain
        let count = storage.count_messages(room).await.unwrap();
        assert_eq!(count, 2);
    }
}
