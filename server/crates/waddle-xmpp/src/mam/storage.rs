//! MAM storage trait and sqlx-backed implementations.
//!
//! Provides persistent storage for archived messages (XEP-0313).
//! The storage layer supports:
//! - Storing messages with unique archive IDs
//! - Querying with time-based and sender filters
//! - RSM (Result Set Management) pagination

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::{Postgres, QueryBuilder, Row, Sqlite};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument};
use uuid::Uuid;

use super::{ArchivedMessage, MamQuery, MamResult};
use crate::xep::matches_fulltext;

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

impl From<sqlx::Error> for MamStorageError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
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

    /// Replace an archived message with a XEP-0424 / XEP-0425 tombstone in
    /// place. Clears `body`, `stanza_xml`, `thread_id`, `parent_thread_id`,
    /// `reply_to_id`, `reply_to_jid`, and overwrites `rich_payload` with
    /// the typed `ArchivedRichPayload::Tombstone(...)` value, per
    /// XEP-0424 §Tombstones / XEP-0425 §Tombstones: "any related
    /// elements which might leak information about the original message".
    ///
    /// Looks up the row by `archive_id` (the storage primary key). Returns
    /// `Ok(true)` when a row was found and updated, `Ok(false)` when no row
    /// matched, and `Err` on storage failure.
    async fn replace_with_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError>;

    /// Get a single message by its original message/stanza id inside an archive.
    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &str,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get a message by its wire message id inside an archive.
    async fn get_message_by_message_id(
        &self,
        archive_jid: &str,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get a message by server archive id or stanza id, excluding client origin-id.
    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &str,
        stanza_id: &str,
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

#[derive(Clone, Default)]
pub struct InMemoryMamStorage {
    entries: Arc<RwLock<Vec<(String, ArchivedMessage)>>>,
}

impl InMemoryMamStorage {
    pub fn new() -> Self {
        Self::default()
    }

    fn generate_archive_id() -> String {
        Uuid::now_v7().to_string()
    }
}

#[async_trait]
impl MamStorage for InMemoryMamStorage {
    async fn store_message(
        &self,
        archive_jid: &str,
        message: &ArchivedMessage,
    ) -> Result<String, MamStorageError> {
        let archive_id = if message.id.is_empty() {
            Self::generate_archive_id()
        } else {
            message.id.clone()
        };

        let mut stored = message.clone();
        stored.id = archive_id.clone();

        let mut entries = self.entries.write().await;
        entries.push((archive_jid.to_string(), stored));
        Ok(archive_id)
    }

    async fn query_messages(
        &self,
        archive_jid: &str,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        let entries = self.entries.read().await;
        let mut messages: Vec<ArchivedMessage> = entries
            .iter()
            .filter(|(jid, _)| jid == archive_jid)
            .map(|(_, message)| message.clone())
            .collect();

        if let Some(start) = query.start {
            messages.retain(|message| message.timestamp >= start);
        }
        if let Some(end) = query.end {
            messages.retain(|message| message.timestamp <= end);
        }
        if let Some(with) = query.with.as_deref() {
            messages
                .retain(|message| message.from.starts_with(with) || message.to.starts_with(with));
        }
        if let Some(thread_id) = query.thread_id.as_ref() {
            messages.retain(|message| matches_thread_filter(message, thread_id.as_str()));
        }
        if let Some(fulltext) = query.fulltext.as_ref() {
            messages.retain(|message| matches_fulltext(message.body.as_str(), fulltext.as_str()));
        }
        let count = Some(u32::try_from(messages.len()).unwrap_or(u32::MAX));

        if let Some(before_id) = query.before_id.as_deref().filter(|id| !id.is_empty()) {
            messages.retain(|message| message.id.as_str() < before_id);
        }
        if let Some(after_id) = query.after_id.as_deref() {
            messages.retain(|message| message.id.as_str() > after_id);
        }

        if uses_backward_pagination(query) {
            messages.sort_by(|a, b| b.id.cmp(&a.id));
        } else {
            messages.sort_by(|a, b| a.id.cmp(&b.id));
        }

        let actual_limit = query.max.unwrap_or(100).min(500) as usize;
        let mut complete = true;
        if messages.len() > actual_limit {
            messages.truncate(actual_limit);
            complete = false;
        }

        if uses_backward_pagination(query) {
            messages.reverse();
        }

        let first_id = messages.first().map(|message| message.id.clone());
        let last_id = messages.last().map(|message| message.id.clone());
        Ok(MamResult {
            messages,
            complete,
            first_id,
            last_id,
            count,
        })
    }

    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(_, message)| message.id == archive_id)
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &str,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == archive_jid
                    && (message.stanza_id.as_deref() == Some(stanza_id)
                        || message.origin_id.as_deref() == Some(stanza_id))
            })
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &str,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == archive_jid && message.stanza_id.as_deref() == Some(message_id)
            })
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &str,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == archive_jid
                    && (message.id == stanza_id || message.stanza_id.as_deref() == Some(stanza_id))
            })
            .map(|(_, message)| message.clone()))
    }

    async fn count_messages(&self, room_jid: &str) -> Result<u32, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries.iter().filter(|(jid, _)| jid == room_jid).count() as u32)
    }

    async fn delete_before(
        &self,
        room_jid: &str,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError> {
        let mut entries = self.entries.write().await;
        let previous_len = entries.len();
        entries.retain(|(jid, message)| !(jid == room_jid && message.timestamp < before));
        Ok((previous_len - entries.len()) as u64)
    }

    async fn replace_with_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        let mut entries = self.entries.write().await;
        for (_jid, message) in entries.iter_mut() {
            if message.id == archive_id {
                apply_tombstone(message, tombstone);
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn apply_tombstone(
    message: &mut ArchivedMessage,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) {
    use waddle_xmpp_core::mam::{ArchivedRichMessage, ArchivedRichPayload};
    // XEP-0424 §Tombstones / XEP-0425 §Tombstones: replace the original
    // contents — `<body/>` AND any related elements which might leak
    // information about the original message — with a `<retracted/>`
    // tombstone. The XEP-0201 `parent_thread_id` falls under that rule
    // (it identifies the parent thread of the retracted message and so
    // leaks the conversation tree the message participated in), and is
    // scrubbed alongside `thread_id`/`reply_to_*`/`stanza_xml`/`body`.
    message.body.clear();
    message.stanza_xml = None;
    message.thread_id = None;
    message.parent_thread_id = None;
    message.reply_to_id = None;
    message.reply_to_jid = None;
    message.rich = Some(ArchivedRichMessage {
        payload: Some(ArchivedRichPayload::Tombstone(tombstone)),
        reply: None,
        references: Vec::new(),
        mentions: Vec::new(),
    });
}

#[derive(Clone)]
enum MamDatabaseBackend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MamDatabaseDriver {
    Sqlite,
    Postgres,
}

/// sqlx-backed MAM storage implementation.
#[derive(Clone)]
pub struct SqlxMamStorage {
    backend: MamDatabaseBackend,
}

impl SqlxMamStorage {
    pub async fn open(database_url: &str) -> Result<Self, MamStorageError> {
        let driver = infer_driver(database_url)?;
        let backend = match driver {
            MamDatabaseDriver::Sqlite => {
                ensure_sqlite_parent_dir(database_url)?;
                let options = SqliteConnectOptions::from_str(database_url)
                    .map_err(|error| MamStorageError::Database(error.to_string()))?
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .journal_mode(SqliteJournalMode::Wal);
                let max_connections = if is_in_memory_sqlite(database_url) {
                    1
                } else {
                    10
                };
                let pool = SqlitePoolOptions::new()
                    .max_connections(max_connections)
                    .connect_with(options)
                    .await?;
                MamDatabaseBackend::Sqlite(pool)
            }
            MamDatabaseDriver::Postgres => {
                let pool = PgPoolOptions::new()
                    .max_connections(10)
                    .connect(database_url)
                    .await?;
                MamDatabaseBackend::Postgres(pool)
            }
        };

        let storage = Self { backend };
        storage.initialize().await?;
        info!(driver = ?driver, "MAM storage initialized");
        Ok(storage)
    }

    pub async fn open_in_memory() -> Result<Self, MamStorageError> {
        Self::open("sqlite::memory:").await
    }

    async fn initialize(&self) -> Result<(), MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                execute_sqlite_batch(pool, SQLITE_MAM_SCHEMA).await?;
                ensure_sqlite_column(pool, "rich_payload", "TEXT").await?;
                ensure_sqlite_column(pool, "stanza_xml", "TEXT").await?;
                ensure_sqlite_column(pool, "nickname_generation", "INTEGER").await?;
                ensure_sqlite_column(pool, "parent_thread_id", "TEXT").await
            }
            MamDatabaseBackend::Postgres(pool) => {
                execute_postgres_batch(pool, POSTGRES_MAM_SCHEMA).await?;
                sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS rich_payload TEXT")
                    .execute(pool)
                    .await?;
                sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS stanza_xml TEXT")
                    .execute(pool)
                    .await?;
                sqlx::query(
                    "ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS nickname_generation BIGINT",
                )
                .execute(pool)
                .await?;
                sqlx::query(
                    "ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS parent_thread_id TEXT",
                )
                .execute(pool)
                .await?;
                Ok(())
            }
        }
    }

    fn generate_archive_id() -> String {
        Uuid::now_v7().to_string()
    }
}

const SELECT_COLUMNS: &str =
    "id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id";

const SQLITE_MAM_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mam_messages (
    id TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    from_jid TEXT NOT NULL,
    to_jid TEXT NOT NULL,
    body TEXT NOT NULL,
    stanza_id TEXT,
    thread_id TEXT,
    reply_to_id TEXT,
    reply_to_jid TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'chat',
    stanza_xml TEXT,
    rich_payload TEXT,
    nickname_generation INTEGER,
    parent_thread_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_mam_room_timestamp
    ON mam_messages(room_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_sender
    ON mam_messages(room_jid, from_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_id
    ON mam_messages(room_jid, id);
CREATE INDEX IF NOT EXISTS idx_mam_room_thread
    ON mam_messages(room_jid, thread_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_reply_to
    ON mam_messages(room_jid, reply_to_id, timestamp DESC);
"#;

const POSTGRES_MAM_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mam_messages (
    id TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL,
    from_jid TEXT NOT NULL,
    to_jid TEXT NOT NULL,
    body TEXT NOT NULL,
    stanza_id TEXT,
    thread_id TEXT,
    reply_to_id TEXT,
    reply_to_jid TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'chat',
    stanza_xml TEXT,
    rich_payload TEXT,
    nickname_generation BIGINT,
    parent_thread_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_mam_room_timestamp
    ON mam_messages(room_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_sender
    ON mam_messages(room_jid, from_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_id
    ON mam_messages(room_jid, id);
CREATE INDEX IF NOT EXISTS idx_mam_room_thread
    ON mam_messages(room_jid, thread_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_reply_to
    ON mam_messages(room_jid, reply_to_id, timestamp DESC);
"#;

fn infer_driver(database_url: &str) -> Result<MamDatabaseDriver, MamStorageError> {
    let lower = database_url.to_ascii_lowercase();
    if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
        return Ok(MamDatabaseDriver::Postgres);
    }
    if lower.starts_with("sqlite:") {
        return Ok(MamDatabaseDriver::Sqlite);
    }

    Err(MamStorageError::Database(format!(
        "unsupported MAM database URL '{}': expected sqlite: or postgres://",
        database_url
    )))
}

fn ensure_sqlite_parent_dir(database_url: &str) -> Result<(), MamStorageError> {
    let Some(path) = sqlite_database_path(database_url) else {
        return Ok(());
    };

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            MamStorageError::Database(format!("failed to create sqlite parent directory: {error}"))
        })?;
    }

    Ok(())
}

fn is_in_memory_sqlite(database_url: &str) -> bool {
    matches!(
        database_url
            .strip_prefix("sqlite://")
            .or_else(|| database_url.strip_prefix("sqlite:")),
        Some(path) if path.starts_with(":memory:")
    )
}

fn sqlite_database_path(database_url: &str) -> Option<&Path> {
    let path = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))?;
    if path.is_empty() || path.starts_with(":memory:") || path.starts_with("file:") {
        return None;
    }
    Some(Path::new(path))
}

async fn execute_sqlite_batch(pool: &SqlitePool, sql: &str) -> Result<(), MamStorageError> {
    for statement in sql
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(pool).await?;
    }
    Ok(())
}

async fn execute_postgres_batch(pool: &PgPool, sql: &str) -> Result<(), MamStorageError> {
    for statement in sql
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(pool).await?;
    }
    Ok(())
}

async fn ensure_sqlite_column(
    pool: &SqlitePool,
    column: &str,
    column_type: &str,
) -> Result<(), MamStorageError> {
    let columns = sqlx::query("PRAGMA table_info(mam_messages)")
        .fetch_all(pool)
        .await?;
    let exists = columns.iter().any(|row| {
        row.try_get::<String, _>("name")
            .is_ok_and(|name| name == column)
    });
    if !exists {
        let sql = format!("ALTER TABLE mam_messages ADD COLUMN {column} {column_type}");
        sqlx::query(&sql).execute(pool).await?;
    }
    Ok(())
}

fn uses_backward_pagination(query: &MamQuery) -> bool {
    // XEP-0059 §2.5: an empty <before/> element requests the last page of
    // results. We treat any present `before_id` — including `Some("")` — as a
    // backward-pagination signal so the SQL emits ORDER BY id DESC and
    // `finalize_result` reverses back to chronological order. The downstream
    // `id < ?` predicate is still skipped for the empty case, so no rows are
    // filtered out.
    query.before_id.is_some()
}

macro_rules! push_common_mam_filters {
    ($builder:expr, $query:expr, $with_filter:expr) => {{
        if let Some(with) = $with_filter {
            $builder
                .push(" AND (from_jid LIKE ")
                .push_bind(with)
                .push(" OR to_jid LIKE ")
                .push_bind(with)
                .push(")");
        }
        if let Some(thread_id) = $query.thread_id.as_ref() {
            $builder
                .push(" AND (id = ")
                .push_bind(thread_id.as_str())
                .push(" OR stanza_id = ")
                .push_bind(thread_id.as_str())
                .push(" OR thread_id = ")
                .push_bind(thread_id.as_str())
                .push(" OR (thread_id IS NULL AND reply_to_id = ")
                .push_bind(thread_id.as_str())
                .push("))");
        }
        if let Some(fulltext) = $query.fulltext.as_ref() {
            for term in fulltext.as_str().split_whitespace() {
                $builder
                    .push(" AND LOWER(body) LIKE ")
                    .push_bind(format!("%{}%", term.to_lowercase()));
            }
        }
    }};
}

fn push_sqlite_mam_filters<'args>(
    builder: &mut QueryBuilder<'args, Sqlite>,
    archive_jid: &'args str,
    query: &'args MamQuery,
    with_filter: Option<&'args str>,
) {
    builder.push_bind(archive_jid);
    if let Some(start) = query.start {
        builder
            .push(" AND timestamp >= ")
            .push_bind(start.to_rfc3339());
    }
    if let Some(end) = query.end {
        builder
            .push(" AND timestamp <= ")
            .push_bind(end.to_rfc3339());
    }
    push_common_mam_filters!(builder, query, with_filter);
}

fn push_postgres_mam_filters<'args>(
    builder: &mut QueryBuilder<'args, Postgres>,
    archive_jid: &'args str,
    query: &'args MamQuery,
    with_filter: Option<&'args str>,
) {
    builder.push_bind(archive_jid);
    if let Some(start) = query.start {
        builder.push(" AND timestamp >= ").push_bind(start);
    }
    if let Some(end) = query.end {
        builder.push(" AND timestamp <= ").push_bind(end);
    }
    push_common_mam_filters!(builder, query, with_filter);
}

fn finalize_result(
    mut messages: Vec<ArchivedMessage>,
    query: &MamQuery,
    count: Option<u32>,
) -> MamResult {
    let actual_limit = query.max.unwrap_or(100).min(500) as usize;
    let complete = messages.len() <= actual_limit;

    if messages.len() > actual_limit {
        messages.pop();
    }

    if uses_backward_pagination(query) {
        messages.reverse();
    }

    let first_id = messages.first().map(|message| message.id.clone());
    let last_id = messages.last().map(|message| message.id.clone());

    MamResult {
        messages,
        complete,
        first_id,
        last_id,
        count,
    }
}

fn matches_thread_filter(message: &ArchivedMessage, thread_id: &str) -> bool {
    message.id == thread_id
        || message.stanza_id.as_deref() == Some(thread_id)
        || message.thread_id.as_deref() == Some(thread_id)
        || (message.thread_id.is_none() && message.reply_to_id.as_deref() == Some(thread_id))
}

fn decode_sqlite_message_row(row: &SqliteRow) -> Result<ArchivedMessage, MamStorageError> {
    let timestamp = DateTime::parse_from_rfc3339(&row.try_get::<String, _>(2)?)
        .map_err(|error| MamStorageError::Serialization(format!("Invalid timestamp: {error}")))?
        .with_timezone(&Utc);

    let rich_payload: Option<String> = row.try_get(13)?;
    let nickname_generation = decode_nickname_generation(row.try_get::<Option<i64>, _>(14)?)?;
    Ok(ArchivedMessage {
        id: row.try_get(0)?,
        timestamp,
        from: row.try_get(3)?,
        to: row.try_get(4)?,
        body: row.try_get(5)?,
        stanza_id: row.try_get(6)?,
        thread_id: row.try_get(7)?,
        parent_thread_id: row.try_get(15)?,
        reply_to_id: row.try_get(8)?,
        reply_to_jid: row.try_get(9)?,
        origin_id: row.try_get(10)?,
        message_type: row.try_get(11)?,
        stanza_xml: row.try_get(12)?,
        rich: decode_rich_payload(rich_payload.as_deref())?,
        nickname_generation,
    })
}

fn decode_postgres_message_row(row: &PgRow) -> Result<ArchivedMessage, MamStorageError> {
    let rich_payload: Option<String> = row.try_get(13)?;
    let nickname_generation = decode_nickname_generation(row.try_get::<Option<i64>, _>(14)?)?;
    Ok(ArchivedMessage {
        id: row.try_get(0)?,
        timestamp: row.try_get(2)?,
        from: row.try_get(3)?,
        to: row.try_get(4)?,
        body: row.try_get(5)?,
        stanza_id: row.try_get(6)?,
        thread_id: row.try_get(7)?,
        parent_thread_id: row.try_get(15)?,
        reply_to_id: row.try_get(8)?,
        reply_to_jid: row.try_get(9)?,
        origin_id: row.try_get(10)?,
        message_type: row.try_get(11)?,
        stanza_xml: row.try_get(12)?,
        rich: decode_rich_payload(rich_payload.as_deref())?,
        nickname_generation,
    })
}

fn encode_rich_payload(message: &ArchivedMessage) -> Result<Option<String>, MamStorageError> {
    message
        .rich
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| MamStorageError::Serialization(error.to_string()))
}

fn decode_rich_payload(
    value: Option<&str>,
) -> Result<Option<waddle_xmpp_core::mam::ArchivedRichMessage>, MamStorageError> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| MamStorageError::Serialization(error.to_string()))
}

/// Convert the database's signed `nickname_generation` column to the
/// typed `u64`. Negative values would only appear from corruption,
/// manual edits, or a write that bypassed `encode_nickname_generation`
/// — refuse them rather than wrap silently with `as u64`.
fn decode_nickname_generation(value: Option<i64>) -> Result<Option<u64>, MamStorageError> {
    value.map(u64::try_from).transpose().map_err(|error| {
        MamStorageError::Serialization(format!(
            "negative nickname_generation column value rejected: {error}"
        ))
    })
}

/// Convert a typed `u64` generation to the SQL backend's signed `i64`,
/// rejecting values outside `i64` range so the column never stores a
/// negative wrapped value that would later round-trip incorrectly.
fn encode_nickname_generation(value: Option<u64>) -> Result<Option<i64>, MamStorageError> {
    value.map(i64::try_from).transpose().map_err(|error| {
        MamStorageError::Serialization(format!(
            "nickname_generation overflow on store ({error}) — exceeds i64::MAX"
        ))
    })
}

#[async_trait]
impl MamStorage for SqlxMamStorage {
    #[instrument(skip(self, message), fields(archive = %archive_jid))]
    async fn store_message(
        &self,
        archive_jid: &str,
        message: &ArchivedMessage,
    ) -> Result<String, MamStorageError> {
        let archive_id = if message.id.is_empty() {
            Self::generate_archive_id()
        } else {
            message.id.clone()
        };
        let message_type = if message.message_type.is_empty() {
            "chat"
        } else {
            message.message_type.as_str()
        };
        let rich_payload = encode_rich_payload(message)?;
        let nickname_generation = encode_nickname_generation(message.nickname_generation)?;

        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut query = QueryBuilder::<Sqlite>::new(
                    "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) ",
                );
                query.push_values(std::iter::once(()), |mut builder, _| {
                    builder
                        .push_bind(&archive_id)
                        .push_bind(archive_jid)
                        .push_bind(message.timestamp.to_rfc3339())
                        .push_bind(message.from.as_str())
                        .push_bind(message.to.as_str())
                        .push_bind(message.body.as_str())
                        .push_bind(message.stanza_id.as_deref())
                        .push_bind(message.thread_id.as_deref())
                        .push_bind(message.reply_to_id.as_deref())
                        .push_bind(message.reply_to_jid.as_deref())
                        .push_bind(message.origin_id.as_deref())
                        .push_bind(message_type)
                        .push_bind(message.stanza_xml.as_deref())
                        .push_bind(rich_payload.as_deref())
                        .push_bind(nickname_generation)
                        .push_bind(message.parent_thread_id.as_deref());
                });
                query.build().execute(pool).await?;
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut query = QueryBuilder::<Postgres>::new(
                    "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) ",
                );
                query.push_values(std::iter::once(()), |mut builder, _| {
                    builder
                        .push_bind(&archive_id)
                        .push_bind(archive_jid)
                        .push_bind(message.timestamp)
                        .push_bind(message.from.as_str())
                        .push_bind(message.to.as_str())
                        .push_bind(message.body.as_str())
                        .push_bind(message.stanza_id.as_deref())
                        .push_bind(message.thread_id.as_deref())
                        .push_bind(message.reply_to_id.as_deref())
                        .push_bind(message.reply_to_jid.as_deref())
                        .push_bind(message.origin_id.as_deref())
                        .push_bind(message_type)
                        .push_bind(message.stanza_xml.as_deref())
                        .push_bind(rich_payload.as_deref())
                        .push_bind(nickname_generation)
                        .push_bind(message.parent_thread_id.as_deref());
                });
                query.build().execute(pool).await?;
            }
        }

        debug!(archive_id = %archive_id, "Message stored in MAM archive");
        Ok(archive_id)
    }

    #[instrument(skip(self), fields(archive = %archive_jid))]
    async fn query_messages(
        &self,
        archive_jid: &str,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        let limit = i64::from(query.max.unwrap_or(100).min(500)) + 1;
        let with_filter = query.with.as_deref().map(|with| format!("{with}%"));

        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut count_builder = QueryBuilder::<Sqlite>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                push_sqlite_mam_filters(
                    &mut count_builder,
                    archive_jid,
                    query,
                    with_filter.as_deref(),
                );
                let count = count_builder
                    .build_query_scalar::<i64>()
                    .fetch_one(pool)
                    .await?;

                let mut builder = QueryBuilder::<Sqlite>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                push_sqlite_mam_filters(&mut builder, archive_jid, query, with_filter.as_deref());
                if let Some(before_id) = query.before_id.as_deref().filter(|id| !id.is_empty()) {
                    builder.push(" AND id < ").push_bind(before_id);
                }
                if let Some(after_id) = query.after_id.as_deref() {
                    builder.push(" AND id > ").push_bind(after_id);
                }
                builder.push(if uses_backward_pagination(query) {
                    " ORDER BY id DESC"
                } else {
                    " ORDER BY id ASC"
                });
                builder.push(" LIMIT ").push_bind(limit);

                let rows: Vec<SqliteRow> = builder.build().fetch_all(pool).await?;
                let messages = rows
                    .iter()
                    .map(decode_sqlite_message_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(finalize_result(
                    messages,
                    query,
                    Some(u32::try_from(count).unwrap_or(u32::MAX)),
                ))
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut count_builder = QueryBuilder::<Postgres>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                push_postgres_mam_filters(
                    &mut count_builder,
                    archive_jid,
                    query,
                    with_filter.as_deref(),
                );
                let count = count_builder
                    .build_query_scalar::<i64>()
                    .fetch_one(pool)
                    .await?;

                let mut builder = QueryBuilder::<Postgres>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                push_postgres_mam_filters(&mut builder, archive_jid, query, with_filter.as_deref());
                if let Some(before_id) = query.before_id.as_deref().filter(|id| !id.is_empty()) {
                    builder.push(" AND id < ").push_bind(before_id);
                }
                if let Some(after_id) = query.after_id.as_deref() {
                    builder.push(" AND id > ").push_bind(after_id);
                }
                builder.push(if uses_backward_pagination(query) {
                    " ORDER BY id DESC"
                } else {
                    " ORDER BY id ASC"
                });
                builder.push(" LIMIT ").push_bind(limit);

                let rows: Vec<PgRow> = builder.build().fetch_all(pool).await?;
                let messages = rows
                    .iter()
                    .map(decode_postgres_message_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(finalize_result(
                    messages,
                    query,
                    Some(u32::try_from(count).unwrap_or(u32::MAX)),
                ))
            }
        }
    }

    #[instrument(skip(self))]
    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE id = "
                ));
                builder.push_bind(archive_id);
                let row = builder.build().fetch_optional(pool).await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE id = "
                ));
                builder.push_bind(archive_id);
                let row = builder.build().fetch_optional(pool).await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &str,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND (stanza_id = ? OR origin_id = ?) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid)
                .bind(stanza_id)
                .bind(stanza_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND (stanza_id = $2 OR origin_id = $2) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid)
                .bind(stanza_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &str,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND stanza_id = ? ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid)
                .bind(message_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND stanza_id = $2 ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid)
                .bind(message_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &str,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND (id = ? OR stanza_id = ?) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid)
                .bind(stanza_id)
                .bind(stanza_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND (id = $2 OR stanza_id = $2) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid)
                .bind(stanza_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    #[instrument(skip(self))]
    async fn count_messages(&self, room_jid: &str) -> Result<u32, MamStorageError> {
        let count = match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                builder.push_bind(room_jid);
                builder.build_query_scalar::<i64>().fetch_one(pool).await?
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                builder.push_bind(room_jid);
                builder.build_query_scalar::<i64>().fetch_one(pool).await?
            }
        };

        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    #[instrument(skip(self))]
    async fn delete_before(
        &self,
        room_jid: &str,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError> {
        let deleted = match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder =
                    QueryBuilder::<Sqlite>::new("DELETE FROM mam_messages WHERE room_jid = ");
                builder
                    .push_bind(room_jid)
                    .push(" AND timestamp < ")
                    .push_bind(before.to_rfc3339());
                builder.build().execute(pool).await?.rows_affected()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder =
                    QueryBuilder::<Postgres>::new("DELETE FROM mam_messages WHERE room_jid = ");
                builder
                    .push_bind(room_jid)
                    .push(" AND timestamp < ")
                    .push_bind(before);
                builder.build().execute(pool).await?.rows_affected()
            }
        };

        debug!(archive = %room_jid, deleted, "Deleted old messages from MAM archive");
        Ok(deleted)
    }

    #[instrument(skip(self, tombstone))]
    async fn replace_with_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        use waddle_xmpp_core::mam::{ArchivedRichMessage, ArchivedRichPayload};
        let payload = ArchivedRichMessage {
            payload: Some(ArchivedRichPayload::Tombstone(tombstone)),
            reply: None,
            references: Vec::new(),
            mentions: Vec::new(),
        };
        let encoded = serde_json::to_string(&payload)
            .map_err(|error| MamStorageError::Serialization(error.to_string()))?;

        let rows = match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(
                    "UPDATE mam_messages SET body = '', stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
                );
                builder
                    .push_bind(encoded.as_str())
                    .push(" WHERE id = ")
                    .push_bind(archive_id);
                builder.build().execute(pool).await?.rows_affected()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(
                    "UPDATE mam_messages SET body = '', stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
                );
                builder
                    .push_bind(encoded.as_str())
                    .push(" WHERE id = ")
                    .push_bind(archive_id);
                builder.build().execute(pool).await?.rows_affected()
            }
        };

        debug!(
            archive_id = %archive_id,
            rows_affected = rows,
            "Replaced archived message with tombstone"
        );
        Ok(rows > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    async fn create_test_storage() -> SqlxMamStorage {
        SqlxMamStorage::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn test_store_and_retrieve_message() {
        let storage = create_test_storage().await;

        let msg = ArchivedMessage {
            id: String::new(),
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
            parent_thread_id: None,
            reply_to_id: Some("parent-message-1".to_string()),
            reply_to_jid: Some("bob@example.com".to_string()),
            origin_id: Some("origin-abc".to_string()),
            message_type: "groupchat".to_string(),
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='archive-stanza-1'><body>Reply body</body></message>".to_string(),
            ),
            rich: None,
            nickname_generation: None,
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
        assert!(retrieved.stanza_xml.is_some());
    }

    #[tokio::test]
    async fn xep_0201_parent_thread_id_round_trips_through_storage() {
        // Locks the column-level round-trip for the new parent_thread_id
        // column. Replay of `<thread parent>` is covered separately by the
        // mam.rs replay-builder tests in commit 4.
        let storage = create_test_storage().await;
        let msg = ArchivedMessage {
            id: String::new(),
            timestamp: Utc::now(),
            from: "room@conference.example.com/alice".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "Nested-thread reply".to_string(),
            stanza_id: Some("archive-stanza-2".to_string()),
            thread_id: Some("child-thread".to_string()),
            parent_thread_id: Some("root-thread".to_string()),
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: None,
            message_type: "groupchat".to_string(),
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
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

        assert_eq!(retrieved.thread_id.as_deref(), Some("child-thread"));
        assert_eq!(retrieved.parent_thread_id.as_deref(), Some("root-thread"));
    }

    #[tokio::test]
    async fn test_query_with_pagination() {
        let storage = create_test_storage().await;
        let archive = "room@conference.example.com";

        for body in ["one", "two", "three"] {
            let msg = ArchivedMessage {
                id: String::new(),
                timestamp: Utc::now(),
                from: "user@example.com/device".to_string(),
                to: archive.to_string(),
                body: body.to_string(),
                ..Default::default()
            };
            storage.store_message(archive, &msg).await.unwrap();
        }

        let page_one = storage
            .query_messages(
                archive,
                &MamQuery {
                    max: Some(2),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page_one.messages.len(), 2);
        assert!(!page_one.complete);

        let page_two = storage
            .query_messages(
                archive,
                &MamQuery {
                    after_id: page_one.last_id.clone(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page_two.messages.len(), 1);
        assert_eq!(page_two.messages[0].body, "three");
    }

    #[tokio::test]
    async fn test_thread_query_filters_before_pagination_and_count() {
        let storage = create_test_storage().await;
        let archive = "room@conference.example.com";

        for msg in [
            ArchivedMessage {
                id: "a-thread-root".to_string(),
                body: "root".to_string(),
                ..archived_groupchat(archive)
            },
            ArchivedMessage {
                id: "b-thread-reply".to_string(),
                thread_id: Some("a-thread-root".to_string()),
                body: "reply".to_string(),
                ..archived_groupchat(archive)
            },
            ArchivedMessage {
                id: "c-legacy-reply".to_string(),
                reply_to_id: Some("a-thread-root".to_string()),
                body: "legacy".to_string(),
                ..archived_groupchat(archive)
            },
            ArchivedMessage {
                id: "unrelated".to_string(),
                thread_id: Some("other-thread".to_string()),
                body: "unrelated".to_string(),
                ..archived_groupchat(archive)
            },
        ] {
            storage.store_message(archive, &msg).await.unwrap();
        }

        let result = storage
            .query_messages(
                archive,
                &MamQuery {
                    thread_id: waddle_xmpp_core::mam::ThreadId::new("a-thread-root"),
                    max: Some(2),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let ids: Vec<&str> = result
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a-thread-root", "b-thread-reply"]);
        assert_eq!(result.count, Some(3));
        assert!(!result.complete);
    }

    #[tokio::test]
    async fn test_fulltext_query_filters_before_pagination_and_count() {
        let storage = create_test_storage().await;
        let archive = "room@conference.example.com";

        for msg in [
            ArchivedMessage {
                id: "a-alpha".to_string(),
                body: "release notes alpha".to_string(),
                ..archived_groupchat(archive)
            },
            ArchivedMessage {
                id: "b-beta".to_string(),
                body: "release notes beta".to_string(),
                ..archived_groupchat(archive)
            },
            ArchivedMessage {
                id: "c-other".to_string(),
                body: "standup notes".to_string(),
                ..archived_groupchat(archive)
            },
        ] {
            storage.store_message(archive, &msg).await.unwrap();
        }

        let result = storage
            .query_messages(
                archive,
                &MamQuery {
                    fulltext: waddle_xmpp_core::mam::RichText::new("release notes"),
                    max: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let ids: Vec<&str> = result
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a-alpha"]);
        assert_eq!(result.count, Some(2));
        assert!(!result.complete);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_sqlite_file_backing_persists() {
        let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        let path = artifacts.join(format!("mam-{}.db", uuid::Uuid::new_v4()));
        let database_url = format!("sqlite://{}", path.display());
        let archive = "room@conference.example.com";

        {
            let storage = SqlxMamStorage::open(&database_url).await.expect("storage");
            let msg = ArchivedMessage {
                id: String::new(),
                timestamp: Utc::now(),
                from: "user@example.com/device".to_string(),
                to: archive.to_string(),
                body: "persisted".to_string(),
                ..Default::default()
            };
            storage.store_message(archive, &msg).await.expect("store");
        }

        let reopened = SqlxMamStorage::open(&database_url).await.expect("reopen");
        let result = reopened
            .query_messages(archive, &MamQuery::default())
            .await
            .expect("query");
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].body, "persisted");

        for cleanup in [
            path.clone(),
            PathBuf::from(format!("{}-shm", path.display())),
            PathBuf::from(format!("{}-wal", path.display())),
        ] {
            let _ = std::fs::remove_file(cleanup);
        }
    }

    // XEP-0059 §2.5: an empty <before/> element requests the last page of
    // results. Regression test for a bug where `before_id = Some("")` was
    // collapsed to "no pagination" and the query returned the *first* page
    // (oldest N) instead of the last page (newest N).
    #[tokio::test]
    async fn test_empty_before_returns_last_page() {
        let storage = create_test_storage().await;
        let archive = "room@conference.example.com";

        for body in ["one", "two", "three", "four", "five", "six"] {
            let msg = ArchivedMessage {
                id: String::new(),
                timestamp: Utc::now(),
                from: "user@example.com/device".to_string(),
                to: archive.to_string(),
                body: body.to_string(),
                ..Default::default()
            };
            storage.store_message(archive, &msg).await.unwrap();
        }

        let last_page = storage
            .query_messages(
                archive,
                &MamQuery {
                    max: Some(3),
                    before_id: Some(String::new()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let bodies: Vec<&str> = last_page.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(bodies, vec!["four", "five", "six"]);
        assert!(!last_page.complete);
    }

    fn archived_groupchat(archive: &str) -> ArchivedMessage {
        ArchivedMessage {
            timestamp: Utc::now(),
            from: format!("{archive}/alice"),
            to: archive.to_string(),
            message_type: "groupchat".to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn xep_0424_tombstone_scrubs_parent_thread_id() {
        // XEP-0424 §Tombstones: replace `<body/>` and any related
        // elements which might leak information. `parent_thread_id`
        // identifies the parent thread and so must be cleared.
        use waddle_xmpp_core::mam::{ArchivedRichMessage, ArchivedTombstone, RichMessageId};

        let storage = create_test_storage().await;
        let archive_jid = "room@conference.example.com";
        let msg = ArchivedMessage {
            id: String::new(),
            timestamp: Utc::now(),
            from: format!("{archive_jid}/alice"),
            to: archive_jid.to_string(),
            body: "secret thread content".to_string(),
            stanza_id: Some("wire-id-1".to_string()),
            thread_id: Some("child-thread".to_string()),
            parent_thread_id: Some("root-thread".to_string()),
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: None,
            message_type: "groupchat".to_string(),
            stanza_xml: Some(
                "<message xmlns='jabber:client'><body>secret</body><thread parent='root-thread'>child-thread</thread></message>".to_string(),
            ),
            rich: None,
            nickname_generation: None,
        };
        let archive_id = storage.store_message(archive_jid, &msg).await.unwrap();

        let tombstone = ArchivedTombstone {
            retraction_id: Some(RichMessageId::new("retract-1").expect("rich id")),
            stamp: Utc::now(),
            moderation: None,
        };
        let replaced = storage
            .replace_with_tombstone(&archive_id, tombstone)
            .await
            .unwrap();
        assert!(replaced);

        let row = storage
            .get_message(&archive_id)
            .await
            .unwrap()
            .expect("tombstone row");

        assert!(row.body.is_empty(), "body must be cleared");
        assert!(
            row.stanza_xml.is_none(),
            "stanza_xml must be cleared so the original wire form does not leak"
        );
        assert_eq!(
            row.thread_id, None,
            "thread_id is leak-prone (identifies the conversation), must be NULL"
        );
        assert_eq!(
            row.parent_thread_id, None,
            "parent_thread_id is leak-prone (identifies the parent conversation tree), must be NULL"
        );
        assert_eq!(row.reply_to_id, None);
        assert_eq!(row.reply_to_jid, None);

        // The row's rich payload must be the tombstone marker — a
        // `<retracted/>`-only message with no `<thread/>` ever
        // re-emitted on replay.
        let rich = row.rich.expect("tombstone row has rich payload");
        assert!(
            matches!(
                rich,
                ArchivedRichMessage {
                    payload: Some(waddle_xmpp_core::mam::ArchivedRichPayload::Tombstone(_)),
                    ..
                }
            ),
            "tombstone rich payload variant must be `Tombstone`"
        );
    }

    #[tokio::test]
    async fn xep_0425_moderation_tombstone_scrubs_parent_thread_id() {
        // XEP-0425 §Tombstones uses the same scrub rule as XEP-0424;
        // the only difference is the `<moderated/>` annotation in the
        // rich payload. Same leak-prone fields must be cleared.
        use waddle_xmpp_core::mam::{ArchivedModeration, ArchivedTombstone, RichMessageId};

        let storage = create_test_storage().await;
        let archive_jid = "room@conference.example.com";
        let msg = ArchivedMessage {
            id: String::new(),
            timestamp: Utc::now(),
            from: format!("{archive_jid}/alice"),
            to: archive_jid.to_string(),
            body: "moderated content".to_string(),
            stanza_id: Some("wire-id-2".to_string()),
            thread_id: Some("child-thread".to_string()),
            parent_thread_id: Some("root-thread".to_string()),
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: None,
            message_type: "groupchat".to_string(),
            stanza_xml: Some(
                "<message xmlns='jabber:client'><body>x</body><thread parent='root-thread'>child-thread</thread></message>".to_string(),
            ),
            rich: None,
            nickname_generation: None,
        };
        let archive_id = storage.store_message(archive_jid, &msg).await.unwrap();

        let moderator: jid::Jid = "mod@example.com".parse().expect("jid");
        let tombstone = ArchivedTombstone {
            retraction_id: None,
            stamp: Utc::now(),
            moderation: Some(ArchivedModeration {
                target_id: RichMessageId::new("wire-id-2").expect("rich id"),
                moderated_by: moderator,
                stamp: Some(Utc::now()),
                reason: None,
            }),
        };
        storage
            .replace_with_tombstone(&archive_id, tombstone)
            .await
            .unwrap();

        let row = storage
            .get_message(&archive_id)
            .await
            .unwrap()
            .expect("tombstone row");

        assert_eq!(row.thread_id, None);
        assert_eq!(row.parent_thread_id, None);
        assert!(row.body.is_empty());
        assert!(row.stanza_xml.is_none());

        // Verify the tombstone is the moderation variant (covers the
        // XEP-0425 path specifically).
        let rich = row.rich.expect("tombstone row has rich payload");
        match rich.payload {
            Some(waddle_xmpp_core::mam::ArchivedRichPayload::Tombstone(t)) => {
                assert!(
                    t.moderation.is_some(),
                    "moderation tombstone must carry XEP-0425 moderation annotation"
                );
            }
            other => panic!("expected Tombstone, got {other:?}"),
        }
    }
}
