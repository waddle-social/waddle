//! MAM storage trait and sqlx-backed implementations.
//!
//! Provides persistent storage for archived messages (XEP-0313).
//! The storage layer supports:
//! - Storing messages with unique archive IDs
//! - Querying with time-based and sender filters
//! - RSM (Result Set Management) pagination

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::{BareJid, Jid};
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
use xmpp_parsers::message::{MessageType, Thread};

use super::{ArchivedMessage, MamQuery, MamResult, RichMessageId, RichText};
use waddle_xmpp_core::mam::{ArchiveId, ArchivedRichMessage, ArchivedTombstone};
use waddle_xmpp_core::xep::xep0359::{OriginId, StanzaId};

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
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<ArchiveId, MamStorageError>;

    /// Query messages from the archive.
    ///
    /// The `archive_jid` identifies which archive to query:
    /// - For MUC archives: the room bare JID
    /// - For personal archives: the user's bare JID
    ///
    /// Supports filtering by time range, sender, and RSM pagination.
    async fn query_messages(
        &self,
        archive_jid: &BareJid,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError>;

    /// Get a single message by its archive ID.
    async fn get_message(
        &self,
        archive_id: &ArchiveId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Replace an archived message with a XEP-0424 / XEP-0425 tombstone in
    /// place. Clears `body`, `stanza_xml`, `thread`, and overwrites
    /// `rich_payload` with the typed `ArchivedRichPayload::Tombstone(...)`
    /// value (the canonical reply target also lives in `rich.reply` and
    /// is cleared here).
    ///
    /// Looks up the row by `archive_id` (the storage primary key). Returns
    /// `Ok(true)` when a row was found and updated, `Ok(false)` when no row
    /// matched, and `Err` on storage failure.
    async fn replace_with_tombstone(
        &self,
        archive_id: &ArchiveId,
        tombstone: ArchivedTombstone,
    ) -> Result<bool, MamStorageError>;

    /// Get a single message by its original message/stanza id inside an archive.
    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &RichMessageId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get a message by its wire message id inside an archive.
    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &RichMessageId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get a message by server archive id or stanza id, excluding client origin-id.
    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &RichMessageId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get the total count of messages in an archive (for RSM).
    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError>;

    /// Delete messages older than a given timestamp.
    ///
    /// Used for archive maintenance/cleanup.
    async fn delete_before(
        &self,
        room_jid: &BareJid,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError>;
}

#[derive(Clone, Default)]
pub struct InMemoryMamStorage {
    entries: Arc<RwLock<Vec<(BareJid, ArchivedMessage)>>>,
}

impl InMemoryMamStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MamStorage for InMemoryMamStorage {
    async fn store_message(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<ArchiveId, MamStorageError> {
        let archive_id = message.id.clone();
        let stored = message.clone();
        let mut entries = self.entries.write().await;
        entries.push((archive_jid.clone(), stored));
        Ok(archive_id)
    }

    async fn query_messages(
        &self,
        archive_jid: &BareJid,
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
        if let Some(with) = query.with.as_ref() {
            let with_str = with.to_string();
            messages.retain(|message| {
                message.from.to_string().starts_with(&with_str)
                    || message.to.to_string().starts_with(&with_str)
            });
        }
        if let Some(before_id) = query.before_id.as_ref() {
            messages.retain(|message| message.id.as_str() < before_id.as_str());
        }
        if let Some(after_id) = query.after_id.as_ref() {
            messages.retain(|message| message.id.as_str() > after_id.as_str());
        }

        if uses_backward_pagination(query) {
            messages.sort_by(|a, b| b.id.as_str().cmp(a.id.as_str()));
        } else {
            messages.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
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
            count: None,
        })
    }

    async fn get_message(
        &self,
        archive_id: &ArchiveId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(_, message)| &message.id == archive_id)
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &RichMessageId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == archive_jid
                    && (message.stanza_id.as_ref().map(|sid| sid.id.as_str())
                        == Some(stanza_id.as_str())
                        || message.origin_id.as_ref().map(OriginId::as_str)
                            == Some(stanza_id.as_str()))
            })
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &RichMessageId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == archive_jid
                    && message.stanza_id.as_ref().map(|sid| sid.id.as_str())
                        == Some(message_id.as_str())
            })
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &RichMessageId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == archive_jid
                    && (message.id.as_str() == stanza_id.as_str()
                        || message.stanza_id.as_ref().map(|sid| sid.id.as_str())
                            == Some(stanza_id.as_str()))
            })
            .map(|(_, message)| message.clone()))
    }

    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries.iter().filter(|(jid, _)| jid == room_jid).count() as u32)
    }

    async fn delete_before(
        &self,
        room_jid: &BareJid,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError> {
        let mut entries = self.entries.write().await;
        let previous_len = entries.len();
        entries.retain(|(jid, message)| !(jid == room_jid && message.timestamp < before));
        Ok((previous_len - entries.len()) as u64)
    }

    async fn replace_with_tombstone(
        &self,
        archive_id: &ArchiveId,
        tombstone: ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        let mut entries = self.entries.write().await;
        for (_jid, message) in entries.iter_mut() {
            if &message.id == archive_id {
                apply_tombstone(message, tombstone);
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn apply_tombstone(message: &mut ArchivedMessage, tombstone: ArchivedTombstone) {
    use waddle_xmpp_core::mam::ArchivedRichPayload;
    message.body = None;
    message.stanza_xml = None;
    message.thread = None;
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
                ensure_sqlite_column(pool, "nickname_generation", "INTEGER").await
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
                Ok(())
            }
        }
    }
}

const SELECT_COLUMNS: &str =
    "id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, origin_id, message_type, stanza_xml, rich_payload, nickname_generation";

const SQLITE_MAM_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mam_messages (
    id TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    from_jid TEXT NOT NULL,
    to_jid TEXT NOT NULL,
    body TEXT,
    stanza_id TEXT,
    thread_id TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'chat',
    stanza_xml TEXT,
    rich_payload TEXT,
    nickname_generation INTEGER,
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
"#;

const POSTGRES_MAM_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mam_messages (
    id TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL,
    from_jid TEXT NOT NULL,
    to_jid TEXT NOT NULL,
    body TEXT,
    stanza_id TEXT,
    thread_id TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'chat',
    stanza_xml TEXT,
    rich_payload TEXT,
    nickname_generation BIGINT,
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
    // results, signalled by `last_page = true`. A `<before/>` with a cursor
    // is also backward-pagination. Either signal flips the SQL to
    // ORDER BY id DESC and `finalize_result` reverses back to chronological
    // order. With `last_page` and no cursor, no `id < ?` predicate is
    // emitted, so no rows are filtered out.
    query.before_id.is_some() || query.last_page
}

fn finalize_result(mut messages: Vec<ArchivedMessage>, query: &MamQuery) -> MamResult {
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
        count: None,
    }
}

/// Column indices match `SELECT_COLUMNS`:
/// `id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, origin_id, message_type, stanza_xml, rich_payload, nickname_generation`.
fn decode_sqlite_message_row(row: &SqliteRow) -> Result<ArchivedMessage, MamStorageError> {
    let timestamp = DateTime::parse_from_rfc3339(&row.try_get::<String, _>(2)?)
        .map_err(|error| MamStorageError::Serialization(format!("Invalid timestamp: {error}")))?
        .with_timezone(&Utc);

    let room_jid: String = row.try_get(1)?;
    let stanza_id: Option<String> = row.try_get(6)?;
    let thread_id: Option<String> = row.try_get(7)?;
    let origin_id: Option<String> = row.try_get(8)?;
    let message_type_str: String = row.try_get(9)?;
    let rich_payload: Option<String> = row.try_get(11)?;
    let nickname_generation = decode_nickname_generation(row.try_get::<Option<i64>, _>(12)?)?;

    let archive_owner = decode_archive_jid(&room_jid)?;
    Ok(ArchivedMessage {
        id: decode_archive_id(row.try_get(0)?, "id")?,
        timestamp,
        from: decode_jid(row.try_get(3)?, "from_jid")?,
        to: decode_archive_jid(&row.try_get::<String, _>(4)?)?,
        body: decode_body(row.try_get(5)?),
        stanza_id: decode_stanza_id(stanza_id, &archive_owner),
        thread: thread_id.map(Thread),
        origin_id: origin_id.and_then(OriginId::new),
        message_type: decode_message_type(&message_type_str)?,
        stanza_xml: row.try_get(10)?,
        rich: decode_rich_payload(rich_payload.as_deref())?,
        nickname_generation,
    })
}

fn decode_postgres_message_row(row: &PgRow) -> Result<ArchivedMessage, MamStorageError> {
    let room_jid: String = row.try_get(1)?;
    let stanza_id: Option<String> = row.try_get(6)?;
    let thread_id: Option<String> = row.try_get(7)?;
    let origin_id: Option<String> = row.try_get(8)?;
    let message_type_str: String = row.try_get(9)?;
    let rich_payload: Option<String> = row.try_get(11)?;
    let nickname_generation = decode_nickname_generation(row.try_get::<Option<i64>, _>(12)?)?;

    let archive_owner = decode_archive_jid(&room_jid)?;
    Ok(ArchivedMessage {
        id: decode_archive_id(row.try_get(0)?, "id")?,
        timestamp: row.try_get(2)?,
        from: decode_jid(row.try_get(3)?, "from_jid")?,
        to: decode_archive_jid(&row.try_get::<String, _>(4)?)?,
        body: decode_body(row.try_get(5)?),
        stanza_id: decode_stanza_id(stanza_id, &archive_owner),
        thread: thread_id.map(Thread),
        origin_id: origin_id.and_then(OriginId::new),
        message_type: decode_message_type(&message_type_str)?,
        stanza_xml: row.try_get(10)?,
        rich: decode_rich_payload(rich_payload.as_deref())?,
        nickname_generation,
    })
}

fn decode_archive_id(raw: String, column: &str) -> Result<ArchiveId, MamStorageError> {
    ArchiveId::new(raw).ok_or_else(|| {
        MamStorageError::Serialization(format!(
            "empty `{column}` column value rejected (XEP-0359 §4 requires non-empty)"
        ))
    })
}

fn decode_jid(raw: String, column: &str) -> Result<Jid, MamStorageError> {
    Jid::from_str(&raw).map_err(|error| {
        MamStorageError::Serialization(format!("Invalid `{column}` column JID: {error}"))
    })
}

fn decode_archive_jid(raw: &str) -> Result<BareJid, MamStorageError> {
    BareJid::from_str(raw).map_err(|error| {
        MamStorageError::Serialization(format!("Invalid `room_jid` column JID: {error}"))
    })
}

fn decode_body(raw: Option<String>) -> Option<RichText> {
    raw.and_then(RichText::new)
}

/// Per Q4 of #228 the `<stanza-id by='...'>` is reconstructed from the row's
/// archive owner — the archive IS the assigning entity per XEP-0359 §4.
fn decode_stanza_id(raw: Option<String>, archive_owner: &BareJid) -> Option<StanzaId> {
    raw.filter(|value| !value.is_empty())
        .map(|id| StanzaId::new(id, archive_owner.clone()))
}

fn decode_message_type(raw: &str) -> Result<MessageType, MamStorageError> {
    match raw {
        "chat" => Ok(MessageType::Chat),
        "error" => Ok(MessageType::Error),
        "groupchat" => Ok(MessageType::Groupchat),
        "headline" => Ok(MessageType::Headline),
        "normal" => Ok(MessageType::Normal),
        other => Err(MamStorageError::Serialization(format!(
            "Invalid `message_type` column value: {other}"
        ))),
    }
}

fn encode_message_type(message_type: &MessageType) -> &'static str {
    match message_type {
        MessageType::Chat => "chat",
        MessageType::Error => "error",
        MessageType::Groupchat => "groupchat",
        MessageType::Headline => "headline",
        MessageType::Normal => "normal",
    }
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
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<ArchiveId, MamStorageError> {
        let archive_id = message.id.clone();
        let message_type = encode_message_type(&message.message_type);
        let rich_payload = encode_rich_payload(message)?;
        let nickname_generation = encode_nickname_generation(message.nickname_generation)?;
        let archive_jid_str = archive_jid.to_string();
        let from_jid_str = message.from.to_string();
        let to_jid_str = message.to.to_string();
        let stanza_id_str = message.stanza_id.as_ref().map(|sid| sid.id.clone());
        let thread_id_str = message.thread.as_ref().map(|t| t.0.clone());
        let origin_id_str = message
            .origin_id
            .as_ref()
            .map(|oid| oid.as_str().to_owned());
        let body_str = message.body.as_ref().map(|b| b.as_str().to_owned());

        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut query = QueryBuilder::<Sqlite>::new(
                    "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, origin_id, message_type, stanza_xml, rich_payload, nickname_generation) ",
                );
                query.push_values(std::iter::once(()), |mut builder, _| {
                    builder
                        .push_bind(archive_id.as_str())
                        .push_bind(&archive_jid_str)
                        .push_bind(message.timestamp.to_rfc3339())
                        .push_bind(&from_jid_str)
                        .push_bind(&to_jid_str)
                        .push_bind(body_str.as_deref())
                        .push_bind(stanza_id_str.as_deref())
                        .push_bind(thread_id_str.as_deref())
                        .push_bind(origin_id_str.as_deref())
                        .push_bind(message_type)
                        .push_bind(message.stanza_xml.as_deref())
                        .push_bind(rich_payload.as_deref())
                        .push_bind(nickname_generation);
                });
                query.build().execute(pool).await?;
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut query = QueryBuilder::<Postgres>::new(
                    "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, origin_id, message_type, stanza_xml, rich_payload, nickname_generation) ",
                );
                query.push_values(std::iter::once(()), |mut builder, _| {
                    builder
                        .push_bind(archive_id.as_str())
                        .push_bind(&archive_jid_str)
                        .push_bind(message.timestamp)
                        .push_bind(&from_jid_str)
                        .push_bind(&to_jid_str)
                        .push_bind(body_str.as_deref())
                        .push_bind(stanza_id_str.as_deref())
                        .push_bind(thread_id_str.as_deref())
                        .push_bind(origin_id_str.as_deref())
                        .push_bind(message_type)
                        .push_bind(message.stanza_xml.as_deref())
                        .push_bind(rich_payload.as_deref())
                        .push_bind(nickname_generation);
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
        archive_jid: &BareJid,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        let limit = i64::from(query.max.unwrap_or(100).min(500)) + 1;
        let with_filter = query.with.as_ref().map(|with| format!("{with}%"));
        let archive_jid_str = archive_jid.to_string();

        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                builder.push_bind(&archive_jid_str);
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
                if let Some(with) = with_filter.as_deref() {
                    builder
                        .push(" AND (from_jid LIKE ")
                        .push_bind(with)
                        .push(" OR to_jid LIKE ")
                        .push_bind(with)
                        .push(")");
                }
                if let Some(before_id) = query.before_id.as_ref() {
                    builder.push(" AND id < ").push_bind(before_id.as_str());
                }
                if let Some(after_id) = query.after_id.as_ref() {
                    builder.push(" AND id > ").push_bind(after_id.as_str());
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
                Ok(finalize_result(messages, query))
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                builder.push_bind(&archive_jid_str);
                if let Some(start) = query.start {
                    builder.push(" AND timestamp >= ").push_bind(start);
                }
                if let Some(end) = query.end {
                    builder.push(" AND timestamp <= ").push_bind(end);
                }
                if let Some(with) = with_filter.as_deref() {
                    builder
                        .push(" AND (from_jid LIKE ")
                        .push_bind(with)
                        .push(" OR to_jid LIKE ")
                        .push_bind(with)
                        .push(")");
                }
                if let Some(before_id) = query.before_id.as_ref() {
                    builder.push(" AND id < ").push_bind(before_id.as_str());
                }
                if let Some(after_id) = query.after_id.as_ref() {
                    builder.push(" AND id > ").push_bind(after_id.as_str());
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
                Ok(finalize_result(messages, query))
            }
        }
    }

    #[instrument(skip(self))]
    async fn get_message(
        &self,
        archive_id: &ArchiveId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE id = "
                ));
                builder.push_bind(archive_id.as_str());
                let row = builder.build().fetch_optional(pool).await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE id = "
                ));
                builder.push_bind(archive_id.as_str());
                let row = builder.build().fetch_optional(pool).await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &RichMessageId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND (stanza_id = ? OR origin_id = ?) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(&archive_jid_str)
                .bind(stanza_id.as_str())
                .bind(stanza_id.as_str())
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND (stanza_id = $2 OR origin_id = $2) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(&archive_jid_str)
                .bind(stanza_id.as_str())
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &RichMessageId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND stanza_id = ? ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(&archive_jid_str)
                .bind(message_id.as_str())
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND stanza_id = $2 ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(&archive_jid_str)
                .bind(message_id.as_str())
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &RichMessageId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND (id = ? OR stanza_id = ?) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(&archive_jid_str)
                .bind(stanza_id.as_str())
                .bind(stanza_id.as_str())
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND (id = $2 OR stanza_id = $2) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(&archive_jid_str)
                .bind(stanza_id.as_str())
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    #[instrument(skip(self))]
    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError> {
        let room_jid_str = room_jid.to_string();
        let count = match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                builder.push_bind(&room_jid_str);
                builder.build_query_scalar::<i64>().fetch_one(pool).await?
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                builder.push_bind(&room_jid_str);
                builder.build_query_scalar::<i64>().fetch_one(pool).await?
            }
        };

        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    #[instrument(skip(self))]
    async fn delete_before(
        &self,
        room_jid: &BareJid,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError> {
        let room_jid_str = room_jid.to_string();
        let deleted = match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder =
                    QueryBuilder::<Sqlite>::new("DELETE FROM mam_messages WHERE room_jid = ");
                builder
                    .push_bind(&room_jid_str)
                    .push(" AND timestamp < ")
                    .push_bind(before.to_rfc3339());
                builder.build().execute(pool).await?.rows_affected()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder =
                    QueryBuilder::<Postgres>::new("DELETE FROM mam_messages WHERE room_jid = ");
                builder
                    .push_bind(&room_jid_str)
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
        archive_id: &ArchiveId,
        tombstone: ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        use waddle_xmpp_core::mam::ArchivedRichPayload;
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
                    "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, rich_payload = ",
                );
                builder
                    .push_bind(encoded.as_str())
                    .push(" WHERE id = ")
                    .push_bind(archive_id.as_str());
                builder.build().execute(pool).await?.rows_affected()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(
                    "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, rich_payload = ",
                );
                builder
                    .push_bind(encoded.as_str())
                    .push(" WHERE id = ")
                    .push_bind(archive_id.as_str());
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
    use uuid::Uuid;

    async fn create_test_storage() -> SqlxMamStorage {
        SqlxMamStorage::open_in_memory().await.unwrap()
    }

    fn bare(s: &str) -> BareJid {
        BareJid::from_str(s).expect("valid bare jid")
    }

    fn jid(s: &str) -> Jid {
        Jid::from_str(s).expect("valid jid")
    }

    fn new_archive_id() -> ArchiveId {
        ArchiveId::new(Uuid::now_v7().to_string()).expect("UUID is non-empty")
    }

    fn fixture_message(archive: &BareJid, from: &str, body: &str) -> ArchivedMessage {
        ArchivedMessage {
            id: new_archive_id(),
            timestamp: Utc::now(),
            from: jid(from),
            to: archive.clone(),
            body: RichText::new(body),
            stanza_id: None,
            thread: None,
            origin_id: None,
            message_type: MessageType::Groupchat,
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        }
    }

    #[tokio::test]
    async fn test_store_and_retrieve_message() {
        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");

        let mut msg = fixture_message(&archive, "user@example.com/nick", "Hello, world!");
        msg.stanza_id = Some(StanzaId::new("abc123", archive.clone()));

        let archive_id = storage.store_message(&archive, &msg).await.unwrap();
        assert!(!archive_id.as_str().is_empty());

        let retrieved = storage
            .get_message(&archive_id)
            .await
            .unwrap()
            .expect("archived");
        assert_eq!(retrieved.id, archive_id);
        assert_eq!(
            retrieved.body.as_ref().map(RichText::as_str),
            Some("Hello, world!")
        );
        assert_eq!(
            retrieved.stanza_id.as_ref().map(|sid| sid.id.as_str()),
            Some("abc123")
        );
    }

    #[tokio::test]
    async fn test_store_and_retrieve_reply_thread_metadata() {
        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");

        let msg = ArchivedMessage {
            id: new_archive_id(),
            timestamp: Utc::now(),
            from: jid("room@conference.example.com/alice"),
            to: archive.clone(),
            body: RichText::new("Reply body"),
            stanza_id: Some(StanzaId::new("archive-stanza-1", archive.clone())),
            thread: Some(Thread("thread-root-1".to_owned())),
            origin_id: OriginId::new("origin-abc"),
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='archive-stanza-1'><body>Reply body</body></message>".to_string(),
            ),
            rich: None,
            nickname_generation: None,
        };

        let archive_id = storage.store_message(&archive, &msg).await.unwrap();

        let retrieved = storage
            .get_message(&archive_id)
            .await
            .unwrap()
            .expect("archived message");

        assert_eq!(
            retrieved.thread.as_ref().map(|t| t.0.as_str()),
            Some("thread-root-1")
        );
        assert_eq!(
            retrieved.origin_id.as_ref().map(OriginId::as_str),
            Some("origin-abc")
        );
        assert_eq!(retrieved.message_type, MessageType::Groupchat);
        assert!(retrieved.stanza_xml.is_some());
    }

    #[tokio::test]
    async fn test_query_with_pagination() {
        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");

        for body in ["one", "two", "three"] {
            let msg = fixture_message(&archive, "user@example.com/device", body);
            storage.store_message(&archive, &msg).await.unwrap();
        }

        let page_one = storage
            .query_messages(
                &archive,
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
                &archive,
                &MamQuery {
                    after_id: page_one.last_id.clone(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page_two.messages.len(), 1);
        assert_eq!(
            page_two.messages[0].body.as_ref().map(RichText::as_str),
            Some("three")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_sqlite_file_backing_persists() {
        let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        let path = artifacts.join(format!("mam-{}.db", uuid::Uuid::new_v4()));
        let database_url = format!("sqlite://{}", path.display());
        let archive = bare("room@conference.example.com");

        {
            let storage = SqlxMamStorage::open(&database_url).await.expect("storage");
            let msg = fixture_message(&archive, "user@example.com/device", "persisted");
            storage.store_message(&archive, &msg).await.expect("store");
        }

        let reopened = SqlxMamStorage::open(&database_url).await.expect("reopen");
        let result = reopened
            .query_messages(&archive, &MamQuery::default())
            .await
            .expect("query");
        assert_eq!(result.messages.len(), 1);
        assert_eq!(
            result.messages[0].body.as_ref().map(RichText::as_str),
            Some("persisted")
        );

        for cleanup in [
            path.clone(),
            PathBuf::from(format!("{}-shm", path.display())),
            PathBuf::from(format!("{}-wal", path.display())),
        ] {
            let _ = std::fs::remove_file(cleanup);
        }
    }

    // XEP-0059 §2.5: an empty <before/> element requests the last page of
    // results. Regression test for the case where `last_page=true` (the typed
    // equivalent of pre-typing `before_id = Some("")`) must signal backward
    // pagination so the query returns the newest N rows, not the oldest N.
    #[tokio::test]
    async fn test_empty_before_returns_last_page() {
        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");

        for body in ["one", "two", "three", "four", "five", "six"] {
            let msg = fixture_message(&archive, "user@example.com/device", body);
            storage.store_message(&archive, &msg).await.unwrap();
        }

        let last_page = storage
            .query_messages(
                &archive,
                &MamQuery {
                    max: Some(3),
                    last_page: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let bodies: Vec<&str> = last_page
            .messages
            .iter()
            .filter_map(|m| m.body.as_ref().map(RichText::as_str))
            .collect();
        assert_eq!(bodies, vec!["four", "five", "six"]);
        assert!(!last_page.complete);
    }
}
