//! MAM storage trait and sqlx-backed implementations.
//!
//! Provides persistent storage for archived messages (XEP-0313).
//! The storage layer supports:
//! - Storing messages with unique archive IDs
//! - Querying with time-based and sender filters
//! - RSM (Result Set Management) pagination

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::BareJid;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::{Postgres, QueryBuilder, Row, Sqlite};
use std::collections::HashSet;
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
///
/// Per XEP-0313 §4.1, archive addressing is normatively a **bare JID**
/// — the user's bare JID for personal archives, the room's bare JID
/// for MUC archives. Typing `archive_jid: &BareJid` (not `&str`) makes
/// that invariant load-bearing in the type system: a caller cannot
/// accidentally pass a full JID with a resource part and silently
/// land in the wrong archive bucket. Internal SQL bindings serialize
/// to `String` once at the bind site (the SQL boundary is the only
/// place untyped textual representation is allowed).
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
        archive_jid: &BareJid,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError>;

    /// Get a single message by its archive ID.
    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Replace an archived message with a XEP-0424 / XEP-0425 tombstone in
    /// place. Clears `body`, `stanza_xml`, `thread` (id and optional
    /// parent), `reply` (id and optional sender JID), and overwrites
    /// `rich_payload` with the typed `ArchivedRichPayload::Tombstone(...)`
    /// value, per XEP-0424 §Tombstones / XEP-0425 §Tombstones: "any
    /// related elements which might leak information about the original
    /// message".
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
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get a message by its wire message id inside an archive.
    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get a message by server archive id or stanza id, excluding client origin-id.
    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
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
        archive_jid: &BareJid,
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
        archive_jid: &BareJid,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        let entries = self.entries.read().await;
        let mut messages: Vec<ArchivedMessage> = entries
            .iter()
            .filter(|(jid, _)| jid == &archive_jid_str)
            .map(|(_, message)| message.clone())
            .collect();
        let filter_before_cursor = match query.filter_before_id.as_deref() {
            Some(before_id) => Some(
                messages
                    .iter()
                    .find(|message| message.id == before_id)
                    .cloned()
                    .ok_or_else(|| MamStorageError::NotFound(before_id.to_string()))?,
            ),
            None => None,
        };
        let filter_after_cursor = match query.filter_after_id.as_deref() {
            Some(after_id) => Some(
                messages
                    .iter()
                    .find(|message| message.id == after_id)
                    .cloned()
                    .ok_or_else(|| MamStorageError::NotFound(after_id.to_string()))?,
            ),
            None => None,
        };
        if let Some(missing_id) = missing_requested_id(&messages, &query.ids) {
            return Err(MamStorageError::NotFound(missing_id));
        }
        let before_cursor = match query.before_id.as_deref().filter(|id| !id.is_empty()) {
            Some(before_id) => Some(
                messages
                    .iter()
                    .find(|message| message.id == before_id)
                    .cloned()
                    .ok_or_else(|| MamStorageError::NotFound(before_id.to_string()))?,
            ),
            None => None,
        };
        let after_cursor = match query.after_id.as_deref() {
            Some(after_id) => Some(
                messages
                    .iter()
                    .find(|message| message.id == after_id)
                    .cloned()
                    .ok_or_else(|| MamStorageError::NotFound(after_id.to_string()))?,
            ),
            None => None,
        };

        if let Some(start) = query.start {
            messages.retain(|message| message.timestamp >= start);
        }
        if let Some(end) = query.end {
            messages.retain(|message| message.timestamp <= end);
        }
        if let Some(with) = query.with.as_ref() {
            // XEP-0313 §4.3.1: `with` matches sender or recipient.
            //  - Bare `with` matches archived JIDs whose **bare form**
            //    equals `with`, regardless of resource (so the row may
            //    be `alice@example.com` or `alice@example.com/web`).
            //  - Full `with` matches only the exact full JID.
            //
            // Earlier this was a textual `starts_with` prefix match
            // which is incorrect: `alice@example.com` would match
            // `alice@example.com.evil/whatever`, leaking unrelated
            // archive rows across XMPP domains. Compare via parsed
            // [`jid::Jid`] values so the matching respects JID
            // structure, not byte-prefix overlap.
            let with_resource = with.resource().is_some();
            let with_bare = with.to_bare();
            messages.retain(|message| {
                jid_matches_with_filter(&message.from, &with_bare, with_resource, with)
                    || jid_matches_with_filter(&message.to, &with_bare, with_resource, with)
            });
        }
        if !query.ids.is_empty() {
            let requested_ids = query.ids.iter().map(String::as_str).collect::<HashSet<_>>();
            messages.retain(|message| requested_ids.contains(message.id.as_str()));
        }
        if let Some(thread_id) = query.thread_id.as_ref() {
            messages.retain(|message| matches_thread_filter(message, thread_id.as_str()));
        }
        if let Some(fulltext) = query.fulltext.as_ref() {
            // None body matches no fulltext query — there's no text to
            // search. Treat absent body as empty for the matcher's
            // purposes; the matcher's existing semantics for "" are
            // unchanged.
            messages.retain(|message| {
                matches_fulltext(message.body.as_deref().unwrap_or(""), fulltext.as_str())
            });
        }
        if let Some(cursor) = filter_before_cursor.as_ref() {
            messages.retain(|message| archive_order_before(message, cursor));
        }
        if let Some(cursor) = filter_after_cursor.as_ref() {
            messages.retain(|message| archive_order_after(message, cursor));
        }
        let count = Some(u32::try_from(messages.len()).unwrap_or(u32::MAX));

        if let Some(cursor) = before_cursor {
            messages.retain(|message| archive_order_before(message, &cursor));
        }
        if let Some(cursor) = after_cursor {
            messages.retain(|message| archive_order_after(message, &cursor));
        }

        // XEP-0313 §archive_order: results MUST be in chronological (received)
        // order. Order by timestamp first; archive id is the tiebreak for
        // messages that share a timestamp.
        if uses_backward_pagination(query) {
            messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| b.id.cmp(&a.id)));
        } else {
            messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));
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
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == &archive_jid_str
                    && (message.stanza_id.as_ref().map(|s| s.id.as_str()) == Some(stanza_id)
                        || message.origin_id.as_ref().map(|o| o.id.as_str()) == Some(stanza_id))
            })
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == &archive_jid_str
                    && message.stanza_id.as_ref().map(|s| s.id.as_str()) == Some(message_id)
            })
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == &archive_jid_str
                    && (message.id == stanza_id
                        || message.stanza_id.as_ref().map(|s| s.id.as_str()) == Some(stanza_id))
            })
            .map(|(_, message)| message.clone()))
    }

    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError> {
        let room_jid_str = room_jid.to_string();
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .filter(|(jid, _)| jid == &room_jid_str)
            .count() as u32)
    }

    async fn delete_before(
        &self,
        room_jid: &BareJid,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError> {
        let room_jid_str = room_jid.to_string();
        let mut entries = self.entries.write().await;
        let previous_len = entries.len();
        entries.retain(|(jid, message)| !(jid == &room_jid_str && message.timestamp < before));
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
    // tombstone. The XEP-0201 thread reference (id + optional parent)
    // and the XEP-0461 reply reference (id + optional sender JID) both
    // fall under that rule — they identify the conversation tree and
    // the message being replied to, leaking the same metadata — and
    // are scrubbed via `message.thread = None` and `message.reply =
    // None` alongside `stanza_xml`/`body`.
    // Tombstones drop the body entirely (XEP-0424 §Tombstones) — None
    // is the correct "no body element" wire form for the replayed
    // tombstone stanza.
    message.body = None;
    message.stanza_xml = None;
    message.thread = None;
    message.reply = None;
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

    /// Test-only escape hatch that inserts a row directly via raw SQL,
    /// bypassing the typed encode path. Used to construct deliberately
    /// malformed rows (e.g. orphan `parent_thread_id` with NULL
    /// `thread_id`) so the decode-side hard-error contract can be
    /// tested. Gated behind `cfg(test)` for in-crate tests and the
    /// `test-utils` Cargo feature for cross-crate integration tests;
    /// sqlite-only.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub async fn insert_raw_thread_columns_for_test(
        &self,
        archive_jid: &BareJid,
        archive_id: &str,
        thread_id: Option<&str>,
        parent_thread_id: Option<&str>,
    ) -> Result<(), MamStorageError> {
        let MamDatabaseBackend::Sqlite(pool) = &self.backend else {
            return Err(MamStorageError::Database(
                "insert_raw_thread_columns_for_test is sqlite-only".to_string(),
            ));
        };
        let archive_jid_str = archive_jid.to_string();
        sqlx::query(
            "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) VALUES (?, ?, ?, ?, ?, NULL, NULL, ?, NULL, NULL, NULL, ?, NULL, NULL, NULL, ?)",
        )
        .bind(archive_id)
        .bind(archive_jid_str.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(archive_jid_str.as_str())
        .bind(archive_jid_str.as_str())
        .bind(thread_id)
        .bind("chat")
        .bind(parent_thread_id)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Test-only escape hatch that inserts a row with a deliberately
    /// malformed `from_jid` text column, bypassing the typed encode
    /// path. Used to construct rows that exercise the decode-side
    /// hard-error contract for `parse_archived_addressing` (a parse
    /// failure surfaces as `MamStorageError::Serialization` rather
    /// than collapsing to a sentinel JID, the data-loss bug
    /// `parse_message_jid` papered over). Gated behind `cfg(test)` for
    /// in-crate tests and the `test-utils` Cargo feature for
    /// cross-crate integration tests; sqlite-only.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub async fn insert_raw_from_jid_for_test(
        &self,
        archive_jid: &BareJid,
        archive_id: &str,
        raw_from: &str,
    ) -> Result<(), MamStorageError> {
        let MamDatabaseBackend::Sqlite(pool) = &self.backend else {
            return Err(MamStorageError::Database(
                "insert_raw_from_jid_for_test is sqlite-only".to_string(),
            ));
        };
        let archive_jid_str = archive_jid.to_string();
        sqlx::query(
            "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind(archive_id)
        .bind(archive_jid_str.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(raw_from)
        .bind(archive_jid_str.as_str())
        .bind("chat")
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Test-only escape hatch mirroring
    /// [`Self::insert_raw_thread_columns_for_test`] for the XEP-0461
    /// reply columns. Used to construct deliberately malformed rows
    /// (e.g. orphan `reply_to_jid` with NULL `reply_to_id`) so the
    /// decode-side hard-error contract for the collapsed
    /// `Option<ArchivedReply>` field can be tested. Gated behind
    /// `cfg(test)` for in-crate tests and the `test-utils` Cargo
    /// feature for cross-crate integration tests; sqlite-only.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub async fn insert_raw_reply_columns_for_test(
        &self,
        archive_jid: &BareJid,
        archive_id: &str,
        reply_to_id: Option<&str>,
        reply_to_jid: Option<&str>,
    ) -> Result<(), MamStorageError> {
        let MamDatabaseBackend::Sqlite(pool) = &self.backend else {
            return Err(MamStorageError::Database(
                "insert_raw_reply_columns_for_test is sqlite-only".to_string(),
            ));
        };
        let archive_jid_str = archive_jid.to_string();
        sqlx::query(
            "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind(archive_id)
        .bind(archive_jid_str.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(archive_jid_str.as_str())
        .bind(archive_jid_str.as_str())
        .bind(reply_to_id)
        .bind(reply_to_jid)
        .bind("chat")
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Test-only escape hatch mirroring
    /// [`Self::insert_raw_thread_columns_for_test`] for the
    /// `message_type` column. Used to construct rows whose
    /// `message_type` SQL value is outside the closed RFC 6121
    /// §5.2.2 set (`chat`, `error`, `groupchat`, `headline`,
    /// `normal`) so the decode-side hard-error contract for the
    /// typed [`xmpp_parsers::message::MessageType`] field can be
    /// tested. Gated behind `cfg(test)` for in-crate tests and the
    /// `test-utils` Cargo feature for cross-crate integration tests;
    /// sqlite-only.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub async fn insert_raw_message_type_for_test(
        &self,
        archive_jid: &BareJid,
        archive_id: &str,
        raw_message_type: &str,
    ) -> Result<(), MamStorageError> {
        let MamDatabaseBackend::Sqlite(pool) = &self.backend else {
            return Err(MamStorageError::Database(
                "insert_raw_message_type_for_test is sqlite-only".to_string(),
            ));
        };
        let archive_jid_str = archive_jid.to_string();
        sqlx::query(
            "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind(archive_id)
        .bind(archive_jid_str.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(archive_jid_str.as_str())
        .bind(archive_jid_str.as_str())
        .bind(raw_message_type)
        .execute(pool)
        .await?;
        Ok(())
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

// RFC 6121 §5.2.3 / XEP-0313 §3: `body` is nullable here. NULL means
// no `<body>` element on the archived stanza; the empty string means
// an empty `<body></body>` element. Earlier schemas had `body TEXT NOT
// NULL` and collapsed both via `.unwrap_or_default()` in the
// projection, losing the distinction.
//
// RFC 6121 §5.2.2 ("Type Attribute"): "If absent, the message is
// implicitly of type `normal`." The column DEFAULT is `'normal'` to
// match. Pre-#228 commit 8 the default was `'chat'`, mirroring the
// removed `default_message_type() = "chat"` helper — a latent
// conformance bug. Production rows always bind an explicit value
// (the typed `MessageType` field on `ArchivedMessage`); the column
// DEFAULT only fires for direct INSERTs that omit the column, but
// fixing it removes the schema-level mismatch.
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
    reply_to_id TEXT,
    reply_to_jid TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'normal',
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

// See `SQLITE_MAM_SCHEMA` for the body-nullability rationale.
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
    reply_to_id TEXT,
    reply_to_jid TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'normal',
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

fn missing_requested_id_in_set(
    available_ids: &HashSet<&str>,
    requested_ids: &[String],
) -> Option<String> {
    requested_ids
        .iter()
        .find(|id| !available_ids.contains(id.as_str()))
        .cloned()
}

fn missing_requested_id(messages: &[ArchivedMessage], requested_ids: &[String]) -> Option<String> {
    if requested_ids.is_empty() {
        return None;
    }

    let available_ids = messages
        .iter()
        .map(|message| message.id.as_str())
        .collect::<HashSet<_>>();
    missing_requested_id_in_set(&available_ids, requested_ids)
}

/// Pre-computed bind values for the SQL `with` filter.
///
/// XEP-0313 §4.3.1 distinguishes bare and full `with` semantics:
/// bare matches the archived JID's bare form (resource may be any /
/// absent); full matches only exact equality. Pre-computing both
/// values keeps the bind site lifetime stable for sqlx and makes the
/// matching shape readable at the SQL emit site.
struct WithFilter {
    /// The bare form of the query `with`. Used for both equality
    /// (against an archived bare JID) and as the prefix in
    /// `LIKE 'bare/%'` (against an archived full JID).
    bare: String,
    /// `Some(full_form)` when the query `with` carries a resource;
    /// in that case the SQL emits an exact-equality match against the
    /// full form. `None` when the query `with` is bare.
    full: Option<String>,
}

impl WithFilter {
    fn from_with(with: &jid::Jid) -> Self {
        Self {
            bare: with.to_bare().to_string(),
            full: with.resource().is_some().then(|| with.to_string()),
        }
    }

    /// SQL `LIKE` pattern matching `<bare>/<any-resource>`. The bare
    /// portion is escaped so any `%`, `_`, or `\` characters in the
    /// localpart (legal per RFC 7622) are treated literally instead of
    /// as LIKE wildcards. Pair the produced pattern with `ESCAPE '\\'`
    /// at the SQL site (see [`LIKE_ESCAPE`]).
    fn bare_resource_prefix(&self) -> String {
        format!("{}/%", escape_like_pattern(&self.bare))
    }
}

/// Escape character used with the SQL `LIKE ... ESCAPE '\'` clause. A
/// single backslash, doubled to escape the Rust string literal.
const LIKE_ESCAPE: &str = "\\";

/// Escape SQL `LIKE` pattern metacharacters in `value` so they match
/// literally. Mirrors the `ESCAPE '\\'` clause emitted alongside the
/// resulting bind. Order matters: the escape character itself must be
/// doubled first so the subsequent `%`/`_` substitutions don't double-
/// escape.
fn escape_like_pattern(value: &str) -> String {
    value
        .replace(LIKE_ESCAPE, "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

macro_rules! push_common_mam_filters {
    ($builder:expr, $query:expr, $with_filter:expr) => {{
        if let Some(with) = $with_filter {
            // Bare `with`: archived JID may be bare (`= bare`) or full
            // with any resource (`LIKE 'bare/%'`). The resource prefix
            // MUST include the `/` separator so domain prefix
            // collisions (`example.com` vs `example.com.evil`) cannot
            // match.
            //
            // Full `with`: archived JID must equal exactly.
            if let Some(full) = with.full.as_deref() {
                $builder
                    .push(" AND (from_jid = ")
                    .push_bind(full)
                    .push(" OR to_jid = ")
                    .push_bind(full)
                    .push(")");
            } else {
                let bare = with.bare.as_str();
                let resource_prefix = with.bare_resource_prefix();
                $builder
                    .push(" AND (from_jid = ")
                    .push_bind(bare)
                    .push(" OR from_jid LIKE ")
                    .push_bind(resource_prefix.clone())
                    .push(" ESCAPE '\\' OR to_jid = ")
                    .push_bind(bare)
                    .push(" OR to_jid LIKE ")
                    .push_bind(resource_prefix)
                    .push(" ESCAPE '\\')");
            }
        }
        if !$query.ids.is_empty() {
            $builder.push(" AND id IN (");
            let mut ids = $builder.separated(", ");
            for id in &$query.ids {
                ids.push_bind(id.as_str());
            }
            ids.push_unseparated(")");
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
    with_filter: Option<&'args WithFilter>,
    filter_before: Option<&'args ArchivedMessage>,
    filter_after: Option<&'args ArchivedMessage>,
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
    if let Some(cursor) = filter_before {
        builder
            .push(" AND (timestamp < ")
            .push_bind(cursor.timestamp.to_rfc3339())
            .push(" OR (timestamp = ")
            .push_bind(cursor.timestamp.to_rfc3339())
            .push(" AND id < ")
            .push_bind(cursor.id.as_str())
            .push("))");
    }
    if let Some(cursor) = filter_after {
        builder
            .push(" AND (timestamp > ")
            .push_bind(cursor.timestamp.to_rfc3339())
            .push(" OR (timestamp = ")
            .push_bind(cursor.timestamp.to_rfc3339())
            .push(" AND id > ")
            .push_bind(cursor.id.as_str())
            .push("))");
    }
    push_common_mam_filters!(builder, query, with_filter);
}

fn push_postgres_mam_filters<'args>(
    builder: &mut QueryBuilder<'args, Postgres>,
    archive_jid: &'args str,
    query: &'args MamQuery,
    with_filter: Option<&'args WithFilter>,
    filter_before: Option<&'args ArchivedMessage>,
    filter_after: Option<&'args ArchivedMessage>,
) {
    builder.push_bind(archive_jid);
    if let Some(start) = query.start {
        builder.push(" AND timestamp >= ").push_bind(start);
    }
    if let Some(end) = query.end {
        builder.push(" AND timestamp <= ").push_bind(end);
    }
    if let Some(cursor) = filter_before {
        builder
            .push(" AND (timestamp < ")
            .push_bind(cursor.timestamp)
            .push(" OR (timestamp = ")
            .push_bind(cursor.timestamp)
            .push(" AND id < ")
            .push_bind(cursor.id.as_str())
            .push("))");
    }
    if let Some(cursor) = filter_after {
        builder
            .push(" AND (timestamp > ")
            .push_bind(cursor.timestamp)
            .push(" OR (timestamp = ")
            .push_bind(cursor.timestamp)
            .push(" AND id > ")
            .push_bind(cursor.id.as_str())
            .push("))");
    }
    push_common_mam_filters!(builder, query, with_filter);
}

async fn fetch_sqlite_cursor(
    pool: &SqlitePool,
    archive_jid: &BareJid,
    cursor_id: &str,
) -> Result<ArchivedMessage, MamStorageError> {
    let mut builder = QueryBuilder::<Sqlite>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
    ));
    builder.push_bind(archive_jid.to_string());
    builder.push(" AND id = ").push_bind(cursor_id);

    let row = builder.build().fetch_optional(pool).await?;
    row.as_ref()
        .map(decode_sqlite_message_row)
        .transpose()?
        .ok_or_else(|| MamStorageError::NotFound(cursor_id.to_string()))
}

async fn fetch_postgres_cursor(
    pool: &PgPool,
    archive_jid: &BareJid,
    cursor_id: &str,
) -> Result<ArchivedMessage, MamStorageError> {
    let mut builder = QueryBuilder::<Postgres>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
    ));
    builder.push_bind(archive_jid.to_string());
    builder.push(" AND id = ").push_bind(cursor_id);

    let row = builder.build().fetch_optional(pool).await?;
    row.as_ref()
        .map(decode_postgres_message_row)
        .transpose()?
        .ok_or_else(|| MamStorageError::NotFound(cursor_id.to_string()))
}

async fn ensure_sqlite_requested_ids_exist(
    pool: &SqlitePool,
    archive_jid: &BareJid,
    requested_ids: &[String],
) -> Result<(), MamStorageError> {
    if requested_ids.is_empty() {
        return Ok(());
    }

    let mut builder = QueryBuilder::<Sqlite>::new("SELECT id FROM mam_messages WHERE room_jid = ");
    builder.push_bind(archive_jid.to_string());
    builder.push(" AND id IN (");
    let mut ids = builder.separated(", ");
    for id in requested_ids {
        ids.push_bind(id.as_str());
    }
    ids.push_unseparated(")");

    let available_ids = builder
        .build_query_scalar::<String>()
        .fetch_all(pool)
        .await?;
    let available_ids = available_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if let Some(missing_id) = missing_requested_id_in_set(&available_ids, requested_ids) {
        return Err(MamStorageError::NotFound(missing_id));
    }

    Ok(())
}

async fn ensure_postgres_requested_ids_exist(
    pool: &PgPool,
    archive_jid: &BareJid,
    requested_ids: &[String],
) -> Result<(), MamStorageError> {
    if requested_ids.is_empty() {
        return Ok(());
    }

    let mut builder =
        QueryBuilder::<Postgres>::new("SELECT id FROM mam_messages WHERE room_jid = ");
    builder.push_bind(archive_jid.to_string());
    builder.push(" AND id IN (");
    let mut ids = builder.separated(", ");
    for id in requested_ids {
        ids.push_bind(id.as_str());
    }
    ids.push_unseparated(")");

    let available_ids = builder
        .build_query_scalar::<String>()
        .fetch_all(pool)
        .await?;
    let available_ids = available_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if let Some(missing_id) = missing_requested_id_in_set(&available_ids, requested_ids) {
        return Err(MamStorageError::NotFound(missing_id));
    }

    Ok(())
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

fn archive_order_before(message: &ArchivedMessage, cursor: &ArchivedMessage) -> bool {
    message.timestamp < cursor.timestamp
        || (message.timestamp == cursor.timestamp && message.id < cursor.id)
}

fn archive_order_after(message: &ArchivedMessage, cursor: &ArchivedMessage) -> bool {
    message.timestamp > cursor.timestamp
        || (message.timestamp == cursor.timestamp && message.id > cursor.id)
}

/// XEP-0313 §4.3.1 `with` predicate, evaluated against a single
/// archived JID (either the row's `from` or `to`).
///
/// - `with_bare`: pre-computed bare form of the query's `with`.
/// - `with_has_resource`: whether the query's `with` carries a
///   resource part (i.e. it is a full JID).
/// - `with_full`: the original query JID, used for exact full-JID
///   equality when the query specified a resource.
///
/// A bare `with` matches any archived JID whose bare form equals
/// `with_bare`, regardless of resource. A full `with` matches only
/// when the archived JID equals it exactly. Comparing structurally
/// via parsed JIDs (rather than `starts_with`) prevents the prefix-
/// collision class of bug where `alice@example.com` would otherwise
/// match `alice@example.com.evil/whatever`.
fn jid_matches_with_filter(
    archived: &jid::Jid,
    with_bare: &BareJid,
    with_has_resource: bool,
    with_full: &jid::Jid,
) -> bool {
    if with_has_resource {
        archived == with_full
    } else {
        &archived.to_bare() == with_bare
    }
}

fn matches_thread_filter(message: &ArchivedMessage, thread_id: &str) -> bool {
    let archived_thread_id = message.thread.as_ref().map(|t| t.id.as_str());
    let archived_reply_id = message.reply.as_ref().map(|r| r.id.as_str());
    let archived_stanza_id = message.stanza_id.as_ref().map(|s| s.id.as_str());
    message.id == thread_id
        || archived_stanza_id == Some(thread_id)
        || archived_thread_id == Some(thread_id)
        || (archived_thread_id.is_none() && archived_reply_id == Some(thread_id))
}

fn decode_sqlite_message_row(row: &SqliteRow) -> Result<ArchivedMessage, MamStorageError> {
    let timestamp = DateTime::parse_from_rfc3339(&row.try_get::<String, _>(2)?)
        .map_err(|error| MamStorageError::Serialization(format!("Invalid timestamp: {error}")))?
        .with_timezone(&Utc);

    let rich_payload: Option<String> = row.try_get(13)?;
    let nickname_generation = decode_nickname_generation(row.try_get::<Option<i64>, _>(14)?)?;
    let thread_id_raw: Option<String> = row.try_get(7)?;
    let parent_thread_id_raw: Option<String> = row.try_get(15)?;
    let thread = decode_thread_columns(thread_id_raw, parent_thread_id_raw)?;
    let reply_to_id_raw: Option<String> = row.try_get(8)?;
    let reply_to_jid_raw: Option<String> = row.try_get(9)?;
    let reply = decode_reply_columns(reply_to_id_raw, reply_to_jid_raw)?;
    let from_raw: String = row.try_get(3)?;
    let to_raw: String = row.try_get(4)?;
    let archive_jid_raw: String = row.try_get(1)?;
    let archive_jid_for_decode = parse_archived_addressing("room_jid", &archive_jid_raw)?;
    let stanza_id_raw: Option<String> = row.try_get(6)?;
    let origin_id_raw: Option<String> = row.try_get(10)?;
    let message_type_raw: String = row.try_get(11)?;
    Ok(ArchivedMessage {
        id: row.try_get(0)?,
        timestamp,
        from: parse_archived_addressing("from_jid", &from_raw)?,
        to: parse_archived_addressing("to_jid", &to_raw)?,
        // Nullable TEXT — preserves the wire-fidelity distinction
        // between `NULL` (no `<body>` element) and `''` (empty
        // `<body></body>`). Explicit type to avoid ambiguity with
        // sqlx's inference.
        body: row.try_get::<Option<String>, _>(5)?,
        stanza_id: stanza_id_raw
            .map(|id| waddle_xmpp_core::xep0359::StanzaId::new(id, archive_jid_for_decode.clone())),
        thread,
        reply,
        origin_id: origin_id_raw.map(waddle_xmpp_core::xep0359::OriginId::new),
        message_type: parse_archived_message_type(&message_type_raw)?,
        stanza_xml: row.try_get(12)?,
        rich: decode_rich_payload(rich_payload.as_deref())?,
        nickname_generation,
    })
}

fn decode_postgres_message_row(row: &PgRow) -> Result<ArchivedMessage, MamStorageError> {
    let rich_payload: Option<String> = row.try_get(13)?;
    let nickname_generation = decode_nickname_generation(row.try_get::<Option<i64>, _>(14)?)?;
    let thread_id_raw: Option<String> = row.try_get(7)?;
    let parent_thread_id_raw: Option<String> = row.try_get(15)?;
    let thread = decode_thread_columns(thread_id_raw, parent_thread_id_raw)?;
    let reply_to_id_raw: Option<String> = row.try_get(8)?;
    let reply_to_jid_raw: Option<String> = row.try_get(9)?;
    let reply = decode_reply_columns(reply_to_id_raw, reply_to_jid_raw)?;
    let from_raw: String = row.try_get(3)?;
    let to_raw: String = row.try_get(4)?;
    let archive_jid_raw: String = row.try_get(1)?;
    let archive_jid_for_decode = parse_archived_addressing("room_jid", &archive_jid_raw)?;
    let stanza_id_raw: Option<String> = row.try_get(6)?;
    let origin_id_raw: Option<String> = row.try_get(10)?;
    let message_type_raw: String = row.try_get(11)?;
    Ok(ArchivedMessage {
        id: row.try_get(0)?,
        timestamp: row.try_get(2)?,
        from: parse_archived_addressing("from_jid", &from_raw)?,
        to: parse_archived_addressing("to_jid", &to_raw)?,
        // See `decode_sqlite_message_row` — nullable, explicit type
        // for the wire-fidelity NULL/'' distinction.
        body: row.try_get::<Option<String>, _>(5)?,
        stanza_id: stanza_id_raw
            .map(|id| waddle_xmpp_core::xep0359::StanzaId::new(id, archive_jid_for_decode.clone())),
        thread,
        reply,
        origin_id: origin_id_raw.map(waddle_xmpp_core::xep0359::OriginId::new),
        message_type: parse_archived_message_type(&message_type_raw)?,
        stanza_xml: row.try_get(12)?,
        rich: decode_rich_payload(rich_payload.as_deref())?,
        nickname_generation,
    })
}

/// Combine the raw `thread_id` / `parent_thread_id` columns into a
/// typed [`waddle_xmpp_core::xep0201::ThreadInfo`].
///
/// SQL schema preserves the two columns; the in-memory representation
/// is collapsed (#228 commit 4). A row with `parent_thread_id` set
/// but `thread_id` NULL is malformed (RFC 6121 §5.2.5: parent is
/// meaningful only as a back-reference from a thread that has its own
/// id) and the typed shape would otherwise paper over the corruption,
/// so we hard-reject it as a serialization error rather than silently
/// dropping the parent.
fn decode_thread_columns(
    thread_id: Option<String>,
    parent_thread_id: Option<String>,
) -> Result<Option<waddle_xmpp_core::xep0201::ThreadInfo>, MamStorageError> {
    use waddle_xmpp_core::mam::ThreadId;
    use waddle_xmpp_core::xep0201::ThreadInfo;

    match (thread_id, parent_thread_id) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(MamStorageError::Serialization(
            "orphan parent_thread_id without thread_id".to_string(),
        )),
        (Some(raw_id), parent_raw) => {
            let id = ThreadId::new(raw_id).ok_or_else(|| {
                MamStorageError::Serialization("invalid (empty) thread_id".to_string())
            })?;
            let parent = match parent_raw {
                None => None,
                Some(raw_parent) => Some(ThreadId::new(raw_parent).ok_or_else(|| {
                    MamStorageError::Serialization("invalid (empty) parent_thread_id".to_string())
                })?),
            };
            Ok(Some(ThreadInfo { id, parent }))
        }
    }
}

/// Combine the raw `reply_to_id` / `reply_to_jid` columns into a typed
/// [`waddle_xmpp_core::mam::ArchivedReply`].
///
/// SQL schema preserves the two columns plus the
/// `idx_mam_room_reply_to` index; the in-memory representation is
/// collapsed (#228 commit 5). A row with `reply_to_jid` set but
/// `reply_to_id` NULL is malformed (XEP-0461 §3 makes `id` MUST and
/// `to` SHOULD — a `to` without an `id` cannot identify which message
/// is being replied to) and the typed shape would otherwise paper
/// over the corruption, so we hard-reject it as a serialization error
/// rather than silently dropping the orphan sender JID.
fn decode_reply_columns(
    reply_to_id: Option<String>,
    reply_to_jid: Option<String>,
) -> Result<Option<waddle_xmpp_core::mam::ArchivedReply>, MamStorageError> {
    use waddle_xmpp_core::mam::{ArchivedReply, RichMessageId};

    match (reply_to_id, reply_to_jid) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(MamStorageError::Serialization(
            "orphan reply_to_jid without reply_to_id".to_string(),
        )),
        (Some(id_raw), to_raw) => {
            let id = RichMessageId::new(id_raw)
                .ok_or_else(|| MamStorageError::Serialization("empty reply_to_id".to_string()))?;
            let to = to_raw
                .map(|raw| raw.parse::<jid::Jid>())
                .transpose()
                .map_err(|error| {
                    MamStorageError::Serialization(format!("invalid reply_to_jid: {error}"))
                })?;
            Ok(Some(ArchivedReply { id, to }))
        }
    }
}

/// Parse a `from_jid` / `to_jid` SQL column value into a typed
/// [`jid::Jid`]. Per the typed-decode hard-error policy, an
/// unparseable value is surfaced as `MamStorageError::Serialization`
/// — never silently substituted with a sentinel JID. This closes the
/// `parse_message_jid` "unknown@invalid" data-loss bug at the
/// storage decode boundary as well.
fn parse_archived_addressing(
    column: &'static str,
    value: &str,
) -> Result<jid::Jid, MamStorageError> {
    value.parse::<jid::Jid>().map_err(|error| {
        MamStorageError::Serialization(format!("Invalid {column} value '{value}': {error}"))
    })
}

/// Decode a stored `message_type` column value into the typed
/// [`xmpp_parsers::message::MessageType`] enum.
///
/// `xmpp-parsers` generates `FromStr` for `MessageType` via the
/// `generate_attribute!` macro (variants: `chat`, `error`,
/// `groupchat`, `headline`, `normal`). Any value outside that closed
/// set is database corruption — a write site bypassed the typed
/// encoder (`message_type_wire_str`) or the column was edited
/// manually. Per the typed-decode hard-error policy (#228 Q7) we
/// surface these as `MamStorageError::Serialization` rather than
/// papering over with a sentinel default. The error message echoes
/// the bad value and the column name so DB-corruption signatures are
/// visible at the boundary, mirroring `parse_archived_addressing`'s
/// pattern for `from_jid` / `to_jid`.
fn parse_archived_message_type(
    value: &str,
) -> Result<xmpp_parsers::message::MessageType, MamStorageError> {
    xmpp_parsers::message::MessageType::from_str(value).map_err(|error| {
        MamStorageError::Serialization(format!("Invalid message_type value '{value}': {error}"))
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
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<String, MamStorageError> {
        let archive_id = if message.id.is_empty() {
            Self::generate_archive_id()
        } else {
            message.id.clone()
        };
        // Typed-payloads boundary: convert the closed `MessageType`
        // enum to its canonical wire literal exactly once, here at
        // the SQL bind site. `message_type_wire_str` is a total
        // mapping over the five RFC 6121 §5.2.2 variants, so the
        // bind value is always one of `chat`/`error`/`groupchat`/
        // `headline`/`normal` — never an empty string and never an
        // unknown value, which is what makes the decode-side hard
        // error meaningful.
        let message_type = waddle_xmpp_core::mam::message_type_wire_str(&message.message_type);
        let rich_payload = encode_rich_payload(message)?;
        let nickname_generation = encode_nickname_generation(message.nickname_generation)?;

        // `Jid::to_string` produces an owned String; build the
        // optional binding once outside the QueryBuilder closure so we
        // can hand sqlx an `Option<&str>` via `.as_deref()`. The
        // archive addressing JIDs are also serialized once here so the
        // bind site sees `&str` and not a fresh allocation per closure
        // capture.
        let archive_jid_str = archive_jid.to_string();
        let from_jid_str = message.from.to_string();
        let to_jid_str = message.to.to_string();
        let reply_to_id_bind = message.reply.as_ref().map(|r| r.id.as_str());
        let reply_to_jid_owned: Option<String> = message
            .reply
            .as_ref()
            .and_then(|r| r.to.as_ref())
            .map(|jid| jid.to_string());
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut query = QueryBuilder::<Sqlite>::new(
                    "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) ",
                );
                query.push_values(std::iter::once(()), |mut builder, _| {
                    builder
                        .push_bind(&archive_id)
                        .push_bind(archive_jid_str.as_str())
                        .push_bind(message.timestamp.to_rfc3339())
                        .push_bind(from_jid_str.as_str())
                        .push_bind(to_jid_str.as_str())
                        .push_bind(message.body.as_deref())
                        .push_bind(message.stanza_id.as_ref().map(|s| s.id.as_str()))
                        .push_bind(message.thread.as_ref().map(|t| t.id.as_str()))
                        .push_bind(reply_to_id_bind)
                        .push_bind(reply_to_jid_owned.as_deref())
                        .push_bind(message.origin_id.as_ref().map(|o| o.id.as_str()))
                        .push_bind(message_type)
                        .push_bind(message.stanza_xml.as_deref())
                        .push_bind(rich_payload.as_deref())
                        .push_bind(nickname_generation)
                        .push_bind(
                            message
                                .thread
                                .as_ref()
                                .and_then(|t| t.parent.as_ref())
                                .map(|p| p.as_str()),
                        );
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
                        .push_bind(archive_jid_str.as_str())
                        .push_bind(message.timestamp)
                        .push_bind(from_jid_str.as_str())
                        .push_bind(to_jid_str.as_str())
                        .push_bind(message.body.as_deref())
                        .push_bind(message.stanza_id.as_ref().map(|s| s.id.as_str()))
                        .push_bind(message.thread.as_ref().map(|t| t.id.as_str()))
                        .push_bind(reply_to_id_bind)
                        .push_bind(reply_to_jid_owned.as_deref())
                        .push_bind(message.origin_id.as_ref().map(|o| o.id.as_str()))
                        .push_bind(message_type)
                        .push_bind(message.stanza_xml.as_deref())
                        .push_bind(rich_payload.as_deref())
                        .push_bind(nickname_generation)
                        .push_bind(
                            message
                                .thread
                                .as_ref()
                                .and_then(|t| t.parent.as_ref())
                                .map(|p| p.as_str()),
                        );
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
        // XEP-0313 §4.3.1 `with` filter: a bare `with` matches the
        // bare form of the archived sender/recipient (so the row's
        // resource can be anything, or absent); a full `with` matches
        // only the exact full JID. The strings are stable for sqlx's
        // bind lifetime requirements.
        //
        // NOTE: the previous shape was `LIKE '{with}%'` (a single
        // textual prefix bind). That collided across JID structure —
        // `alice@example.com` would match `alice@example.com.evil/...`
        // because the prefix overlaps. The corrected shape is exact
        // equality plus a `LIKE 'bare/%'` for the resource form.
        let with_filter = query.with.as_ref().map(WithFilter::from_with);
        let archive_jid_str = archive_jid.to_string();

        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let filter_before_cursor = match query.filter_before_id.as_deref() {
                    Some(before_id) => {
                        Some(fetch_sqlite_cursor(pool, archive_jid, before_id).await?)
                    }
                    None => None,
                };
                let filter_after_cursor = match query.filter_after_id.as_deref() {
                    Some(after_id) => Some(fetch_sqlite_cursor(pool, archive_jid, after_id).await?),
                    None => None,
                };
                ensure_sqlite_requested_ids_exist(pool, archive_jid, &query.ids).await?;

                let mut count_builder = QueryBuilder::<Sqlite>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                push_sqlite_mam_filters(
                    &mut count_builder,
                    archive_jid_str.as_str(),
                    query,
                    with_filter.as_ref(),
                    filter_before_cursor.as_ref(),
                    filter_after_cursor.as_ref(),
                );
                let count = count_builder
                    .build_query_scalar::<i64>()
                    .fetch_one(pool)
                    .await?;

                let mut builder = QueryBuilder::<Sqlite>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                push_sqlite_mam_filters(
                    &mut builder,
                    archive_jid_str.as_str(),
                    query,
                    with_filter.as_ref(),
                    filter_before_cursor.as_ref(),
                    filter_after_cursor.as_ref(),
                );
                if let Some(before_id) = query.before_id.as_deref().filter(|id| !id.is_empty()) {
                    let cursor = fetch_sqlite_cursor(pool, archive_jid, before_id).await?;
                    builder
                        .push(" AND (timestamp < ")
                        .push_bind(cursor.timestamp.to_rfc3339())
                        .push(" OR (timestamp = ")
                        .push_bind(cursor.timestamp.to_rfc3339())
                        .push(" AND id < ")
                        .push_bind(cursor.id)
                        .push("))");
                }
                if let Some(after_id) = query.after_id.as_deref() {
                    let cursor = fetch_sqlite_cursor(pool, archive_jid, after_id).await?;
                    builder
                        .push(" AND (timestamp > ")
                        .push_bind(cursor.timestamp.to_rfc3339())
                        .push(" OR (timestamp = ")
                        .push_bind(cursor.timestamp.to_rfc3339())
                        .push(" AND id > ")
                        .push_bind(cursor.id)
                        .push("))");
                }
                // XEP-0313 §archive_order: chronological order primary, archive
                // id as deterministic tiebreak for tied timestamps.
                builder.push(if uses_backward_pagination(query) {
                    " ORDER BY timestamp DESC, id DESC"
                } else {
                    " ORDER BY timestamp ASC, id ASC"
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
                let filter_before_cursor = match query.filter_before_id.as_deref() {
                    Some(before_id) => {
                        Some(fetch_postgres_cursor(pool, archive_jid, before_id).await?)
                    }
                    None => None,
                };
                let filter_after_cursor = match query.filter_after_id.as_deref() {
                    Some(after_id) => {
                        Some(fetch_postgres_cursor(pool, archive_jid, after_id).await?)
                    }
                    None => None,
                };
                ensure_postgres_requested_ids_exist(pool, archive_jid, &query.ids).await?;

                let mut count_builder = QueryBuilder::<Postgres>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                push_postgres_mam_filters(
                    &mut count_builder,
                    archive_jid_str.as_str(),
                    query,
                    with_filter.as_ref(),
                    filter_before_cursor.as_ref(),
                    filter_after_cursor.as_ref(),
                );
                let count = count_builder
                    .build_query_scalar::<i64>()
                    .fetch_one(pool)
                    .await?;

                let mut builder = QueryBuilder::<Postgres>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                push_postgres_mam_filters(
                    &mut builder,
                    archive_jid_str.as_str(),
                    query,
                    with_filter.as_ref(),
                    filter_before_cursor.as_ref(),
                    filter_after_cursor.as_ref(),
                );
                if let Some(before_id) = query.before_id.as_deref().filter(|id| !id.is_empty()) {
                    let cursor = fetch_postgres_cursor(pool, archive_jid, before_id).await?;
                    builder
                        .push(" AND (timestamp < ")
                        .push_bind(cursor.timestamp)
                        .push(" OR (timestamp = ")
                        .push_bind(cursor.timestamp)
                        .push(" AND id < ")
                        .push_bind(cursor.id)
                        .push("))");
                }
                if let Some(after_id) = query.after_id.as_deref() {
                    let cursor = fetch_postgres_cursor(pool, archive_jid, after_id).await?;
                    builder
                        .push(" AND (timestamp > ")
                        .push_bind(cursor.timestamp)
                        .push(" OR (timestamp = ")
                        .push_bind(cursor.timestamp)
                        .push(" AND id > ")
                        .push_bind(cursor.id)
                        .push("))");
                }
                // XEP-0313 §archive_order: chronological order primary, archive
                // id as deterministic tiebreak for tied timestamps.
                builder.push(if uses_backward_pagination(query) {
                    " ORDER BY timestamp DESC, id DESC"
                } else {
                    " ORDER BY timestamp ASC, id ASC"
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
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND (stanza_id = ? OR origin_id = ?) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
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
                .bind(archive_jid_str.as_str())
                .bind(stanza_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND stanza_id = ? ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
                .bind(message_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND stanza_id = $2 ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
                .bind(message_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND (id = ? OR stanza_id = ?) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
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
                .bind(archive_jid_str.as_str())
                .bind(stanza_id)
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
                builder.push_bind(room_jid_str.as_str());
                builder.build_query_scalar::<i64>().fetch_one(pool).await?
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                builder.push_bind(room_jid_str.as_str());
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
                    .push_bind(room_jid_str.as_str())
                    .push(" AND timestamp < ")
                    .push_bind(before.to_rfc3339());
                builder.build().execute(pool).await?.rows_affected()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder =
                    QueryBuilder::<Postgres>::new("DELETE FROM mam_messages WHERE room_jid = ");
                builder
                    .push_bind(room_jid_str.as_str())
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

        // XEP-0424 §Tombstones / XEP-0425 §Tombstones: drop the body
        // entirely on tombstone. With the new wire-fidelity body
        // semantics (`Option<String>`), the correct "no body element
        // on the wire" form is SQL NULL (`None`), not `''` (which is
        // now the distinct "empty `<body></body>`" form).
        let rows = match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(
                    "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
                );
                builder
                    .push_bind(encoded.as_str())
                    .push(" WHERE id = ")
                    .push_bind(archive_id);
                builder.build().execute(pool).await?.rows_affected()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(
                    "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
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
    use chrono::Duration as ChronoDuration;
    use jid::Jid;
    use std::path::PathBuf;

    async fn create_test_storage() -> SqlxMamStorage {
        SqlxMamStorage::open_in_memory().await.unwrap()
    }

    fn jid(value: &str) -> Jid {
        value.parse::<Jid>().expect("valid jid literal")
    }

    fn bare(value: &str) -> BareJid {
        value.parse::<BareJid>().expect("valid bare jid literal")
    }

    fn user_device() -> Jid {
        jid("user@example.com/device")
    }

    fn archive_alice(archive: &BareJid) -> Jid {
        format!("{archive}/alice")
            .parse::<Jid>()
            .expect("valid jid")
    }

    #[tokio::test]
    async fn test_store_and_retrieve_message() {
        let storage = create_test_storage().await;

        let archive = bare("room@conference.example.com");
        let msg = ArchivedMessage {
            body: Some("Hello, world!".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "abc123",
                jid(&archive.to_string()),
            )),
            ..ArchivedMessage::for_test(
                jid("user@example.com/nick"),
                jid("room@conference.example.com"),
            )
        };

        let archive_id = storage.store_message(&archive, &msg).await.unwrap();
        assert!(!archive_id.is_empty());

        let retrieved = storage.get_message(&archive_id).await.unwrap();
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, archive_id);
        assert_eq!(retrieved.body.as_deref(), Some("Hello, world!"));
        let sid = retrieved.stanza_id.expect("stanza_id round-trips");
        assert_eq!(sid.id, "abc123");
        assert_eq!(sid.by, jid(&archive.to_string()));
    }

    #[tokio::test]
    async fn test_store_and_retrieve_reply_thread_metadata() {
        let storage = create_test_storage().await;

        let msg = ArchivedMessage {
            body: Some("Reply body".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "archive-stanza-1",
                jid("room@conference.example.com"),
            )),
            thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::root(
                waddle_xmpp_core::mam::ThreadId::new("thread-root-1").expect("thread id"),
            )),
            reply: Some(waddle_xmpp_core::mam::ArchivedReply {
                id: waddle_xmpp_core::mam::RichMessageId::new("parent-message-1")
                    .expect("non-empty reply id"),
                to: Some(jid("bob@example.com")),
            }),
            origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("origin-abc")),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='archive-stanza-1'><body>Reply body</body></message>".to_string(),
            ),
            ..ArchivedMessage::for_test(
                jid("room@conference.example.com/alice"),
                jid("room@conference.example.com"),
            )
        };

        let archive_id = storage
            .store_message(&bare("room@conference.example.com"), &msg)
            .await
            .unwrap();

        let retrieved = storage
            .get_message(&archive_id)
            .await
            .unwrap()
            .expect("archived message");

        assert_eq!(
            retrieved.thread.as_ref().map(|t| t.id.as_str()),
            Some("thread-root-1")
        );
        let reply = retrieved.reply.as_ref().expect("reply present");
        assert_eq!(reply.id.as_str(), "parent-message-1");
        assert_eq!(
            reply.to.as_ref().map(|jid| jid.to_string()),
            Some("bob@example.com".to_string())
        );
        assert_eq!(
            retrieved.origin_id.as_ref().map(|o| o.id.as_str()),
            Some("origin-abc")
        );
        assert_eq!(
            retrieved.message_type,
            xmpp_parsers::message::MessageType::Groupchat
        );
        assert!(retrieved.stanza_xml.is_some());
    }

    #[tokio::test]
    async fn xep_0201_parent_thread_id_round_trips_through_storage() {
        // Locks the column-level round-trip for the new parent_thread_id
        // column. Replay of `<thread parent>` is covered separately by the
        // mam.rs replay-builder tests in commit 4.
        let storage = create_test_storage().await;
        let msg = ArchivedMessage {
            body: Some("Nested-thread reply".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "archive-stanza-2",
                jid("room@conference.example.com"),
            )),
            thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::child(
                waddle_xmpp_core::mam::ThreadId::new("child-thread").expect("thread id"),
                waddle_xmpp_core::mam::ThreadId::new("root-thread").expect("parent id"),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                jid("room@conference.example.com/alice"),
                jid("room@conference.example.com"),
            )
        };

        let archive_id = storage
            .store_message(&bare("room@conference.example.com"), &msg)
            .await
            .unwrap();

        let retrieved = storage
            .get_message(&archive_id)
            .await
            .unwrap()
            .expect("archived message");

        let thread = retrieved.thread.as_ref().expect("thread present");
        assert_eq!(thread.id.as_str(), "child-thread");
        assert_eq!(
            thread.parent.as_ref().map(|t| t.as_str()),
            Some("root-thread")
        );
    }

    #[tokio::test]
    async fn test_query_with_pagination() {
        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");
        let archive_jid = jid("room@conference.example.com");

        for body in ["one", "two", "three"] {
            let msg = ArchivedMessage {
                body: Some(body.to_string()),
                ..ArchivedMessage::for_test(user_device(), archive_jid.clone())
            };
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
        assert_eq!(page_two.messages[0].body.as_deref(), Some("three"));
    }

    #[tokio::test]
    async fn test_sqlite_rsm_after_uses_archive_order_not_lexical_id_order() {
        let storage = create_test_storage().await;
        assert_rsm_after_uses_archive_order_not_lexical_id_order(&storage).await;
    }

    #[tokio::test]
    async fn test_inmemory_rsm_after_uses_archive_order_not_lexical_id_order() {
        let storage = InMemoryMamStorage::new();
        assert_rsm_after_uses_archive_order_not_lexical_id_order(&storage).await;
    }

    async fn assert_rsm_after_uses_archive_order_not_lexical_id_order(storage: &dyn MamStorage) {
        let archive = bare("room@conference.example.com");
        let base = Utc::now();
        store_nonlexical_archive_order_messages(storage, &archive, base).await;

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
        assert_eq!(bodies(&page_one), vec!["one", "two"]);
        assert_eq!(page_one.last_id.as_deref(), Some("a-second"));

        let page_two = storage
            .query_messages(
                &archive,
                &MamQuery {
                    max: Some(2),
                    after_id: page_one.last_id.clone(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(bodies(&page_two), vec!["three", "four"]);
        assert_eq!(page_two.last_id.as_deref(), Some("b-fourth"));

        let page_three = storage
            .query_messages(
                &archive,
                &MamQuery {
                    max: Some(2),
                    after_id: page_two.last_id.clone(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(bodies(&page_three), vec!["five"]);
        assert!(page_three.complete);
    }

    #[tokio::test]
    async fn test_sqlite_rsm_before_uses_archive_order_not_lexical_id_order() {
        let storage = create_test_storage().await;
        assert_rsm_before_uses_archive_order_not_lexical_id_order(&storage).await;
    }

    #[tokio::test]
    async fn test_inmemory_rsm_before_uses_archive_order_not_lexical_id_order() {
        let storage = InMemoryMamStorage::new();
        assert_rsm_before_uses_archive_order_not_lexical_id_order(&storage).await;
    }

    async fn assert_rsm_before_uses_archive_order_not_lexical_id_order(storage: &dyn MamStorage) {
        let archive = bare("room@conference.example.com");
        let base = Utc::now();
        store_nonlexical_archive_order_messages(storage, &archive, base).await;

        let page = storage
            .query_messages(
                &archive,
                &MamQuery {
                    max: Some(2),
                    before_id: Some("x-fifth".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(bodies(&page), vec!["three", "four"]);
        assert_eq!(page.first_id.as_deref(), Some("y-third"));
        assert_eq!(page.last_id.as_deref(), Some("b-fourth"));
    }

    #[tokio::test]
    async fn test_sqlite_extended_before_id_filters_without_flipping_order() {
        let storage = create_test_storage().await;
        assert_extended_before_id_filters_without_flipping_order(&storage).await;
    }

    #[tokio::test]
    async fn test_inmemory_extended_before_id_filters_without_flipping_order() {
        let storage = InMemoryMamStorage::new();
        assert_extended_before_id_filters_without_flipping_order(&storage).await;
    }

    async fn assert_extended_before_id_filters_without_flipping_order(storage: &dyn MamStorage) {
        let archive = bare("room@conference.example.com");
        let base = Utc::now();
        store_nonlexical_archive_order_messages(storage, &archive, base).await;

        let result = storage
            .query_messages(
                &archive,
                &MamQuery {
                    filter_before_id: Some("x-fifth".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(bodies(&result), vec!["one", "two", "three", "four"]);
        assert_eq!(result.first_id.as_deref(), Some("z-first"));
        assert_eq!(result.last_id.as_deref(), Some("b-fourth"));
        assert_eq!(result.count, Some(4));
    }

    #[tokio::test]
    async fn test_sqlite_extended_ids_query_returns_specific_messages() {
        let storage = create_test_storage().await;
        assert_extended_ids_query_returns_specific_messages(&storage).await;
    }

    #[tokio::test]
    async fn test_inmemory_extended_ids_query_returns_specific_messages() {
        let storage = InMemoryMamStorage::new();
        assert_extended_ids_query_returns_specific_messages(&storage).await;
    }

    async fn assert_extended_ids_query_returns_specific_messages(storage: &dyn MamStorage) {
        let archive = bare("room@conference.example.com");
        let base = Utc::now();
        store_nonlexical_archive_order_messages(storage, &archive, base).await;

        let result = storage
            .query_messages(
                &archive,
                &MamQuery {
                    ids: vec!["x-fifth".to_string(), "a-second".to_string()],
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
        assert_eq!(ids, vec!["a-second", "x-fifth"]);
        assert_eq!(bodies(&result), vec!["two", "five"]);
        assert_eq!(result.count, Some(2));
    }

    async fn store_nonlexical_archive_order_messages(
        storage: &dyn MamStorage,
        archive: &BareJid,
        base: DateTime<Utc>,
    ) {
        let archive_jid = jid(&archive.to_string());
        for (offset, id, body) in [
            (0, "z-first", "one"),
            (1, "a-second", "two"),
            (2, "y-third", "three"),
            (3, "b-fourth", "four"),
            (4, "x-fifth", "five"),
        ] {
            storage
                .store_message(
                    archive,
                    &ArchivedMessage {
                        id: id.to_string(),
                        timestamp: base + ChronoDuration::seconds(offset),
                        body: Some(body.to_string()),
                        ..ArchivedMessage::for_test(user_device(), archive_jid.clone())
                    },
                )
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn test_sqlite_missing_rsm_cursor_returns_not_found() {
        let storage = create_test_storage().await;
        assert_missing_rsm_cursor_returns_not_found(&storage).await;
    }

    #[tokio::test]
    async fn test_inmemory_missing_rsm_cursor_returns_not_found() {
        let storage = InMemoryMamStorage::new();
        assert_missing_rsm_cursor_returns_not_found(&storage).await;
    }

    async fn assert_missing_rsm_cursor_returns_not_found(storage: &dyn MamStorage) {
        let archive = bare("room@conference.example.com");
        let archive_jid = jid("room@conference.example.com");
        storage
            .store_message(
                &archive,
                &ArchivedMessage {
                    id: "known-id".to_string(),
                    body: Some("known".to_string()),
                    ..ArchivedMessage::for_test(user_device(), archive_jid)
                },
            )
            .await
            .unwrap();

        let error = storage
            .query_messages(
                &archive,
                &MamQuery {
                    after_id: Some("missing-id".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("missing cursor must be an error");

        assert!(matches!(error, MamStorageError::NotFound(ref id) if id == "missing-id"));

        let error = storage
            .query_messages(
                &archive,
                &MamQuery {
                    before_id: Some("missing-before-id".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("missing before cursor must be an error");

        assert!(matches!(error, MamStorageError::NotFound(ref id) if id == "missing-before-id"));

        let error = storage
            .query_messages(
                &archive,
                &MamQuery {
                    filter_before_id: Some("missing-filter-before-id".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("missing extended before-id must be an error");

        assert!(
            matches!(error, MamStorageError::NotFound(ref id) if id == "missing-filter-before-id")
        );

        let error = storage
            .query_messages(
                &archive,
                &MamQuery {
                    filter_after_id: Some("missing-filter-after-id".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("missing extended after-id must be an error");

        assert!(
            matches!(error, MamStorageError::NotFound(ref id) if id == "missing-filter-after-id")
        );

        let error = storage
            .query_messages(
                &archive,
                &MamQuery {
                    ids: vec!["known-id".to_string(), "missing-query-id".to_string()],
                    ..Default::default()
                },
            )
            .await
            .expect_err("missing ids entry must be an error");

        assert!(matches!(error, MamStorageError::NotFound(ref id) if id == "missing-query-id"));
    }

    #[tokio::test]
    async fn test_sqlite_rsm_cursor_outside_query_filters_still_pages() {
        let storage = create_test_storage().await;
        assert_rsm_cursor_outside_query_filters_still_pages(&storage).await;
    }

    #[tokio::test]
    async fn test_inmemory_rsm_cursor_outside_query_filters_still_pages() {
        let storage = InMemoryMamStorage::new();
        assert_rsm_cursor_outside_query_filters_still_pages(&storage).await;
    }

    async fn assert_rsm_cursor_outside_query_filters_still_pages(storage: &dyn MamStorage) {
        let archive = bare("room@conference.example.com");
        let base = Utc::now();
        store_nonlexical_archive_order_messages(storage, &archive, base).await;

        let result = storage
            .query_messages(
                &archive,
                &MamQuery {
                    start: Some(base + ChronoDuration::seconds(3)),
                    after_id: Some("a-second".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(bodies(&result), vec!["four", "five"]);
    }

    fn bodies(result: &MamResult) -> Vec<&str> {
        result
            .messages
            .iter()
            .map(|message| message.body.as_deref().unwrap_or(""))
            .collect()
    }

    #[tokio::test]
    async fn test_thread_query_filters_before_pagination_and_count() {
        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");

        for msg in [
            ArchivedMessage {
                id: "a-thread-root".to_string(),
                body: Some("root".to_string()),
                ..archived_groupchat(&archive)
            },
            ArchivedMessage {
                id: "b-thread-reply".to_string(),
                thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::root(
                    waddle_xmpp_core::mam::ThreadId::new("a-thread-root").expect("thread id"),
                )),
                body: Some("reply".to_string()),
                ..archived_groupchat(&archive)
            },
            ArchivedMessage {
                id: "c-legacy-reply".to_string(),
                reply: Some(waddle_xmpp_core::mam::ArchivedReply {
                    id: waddle_xmpp_core::mam::RichMessageId::new("a-thread-root")
                        .expect("non-empty reply id"),
                    to: None,
                }),
                body: Some("legacy".to_string()),
                ..archived_groupchat(&archive)
            },
            ArchivedMessage {
                id: "unrelated".to_string(),
                thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::root(
                    waddle_xmpp_core::mam::ThreadId::new("other-thread").expect("thread id"),
                )),
                body: Some("unrelated".to_string()),
                ..archived_groupchat(&archive)
            },
        ] {
            storage.store_message(&archive, &msg).await.unwrap();
        }

        let result = storage
            .query_messages(
                &archive,
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
        let archive = bare("room@conference.example.com");

        for msg in [
            ArchivedMessage {
                id: "a-alpha".to_string(),
                body: Some("release notes alpha".to_string()),
                ..archived_groupchat(&archive)
            },
            ArchivedMessage {
                id: "b-beta".to_string(),
                body: Some("release notes beta".to_string()),
                ..archived_groupchat(&archive)
            },
            ArchivedMessage {
                id: "c-other".to_string(),
                body: Some("standup notes".to_string()),
                ..archived_groupchat(&archive)
            },
        ] {
            storage.store_message(&archive, &msg).await.unwrap();
        }

        let result = storage
            .query_messages(
                &archive,
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
        let archive = bare("room@conference.example.com");
        let archive_jid = jid("room@conference.example.com");

        {
            let storage = SqlxMamStorage::open(&database_url).await.expect("storage");
            let msg = ArchivedMessage {
                body: Some("persisted".to_string()),
                ..ArchivedMessage::for_test(user_device(), archive_jid.clone())
            };
            storage.store_message(&archive, &msg).await.expect("store");
        }

        let reopened = SqlxMamStorage::open(&database_url).await.expect("reopen");
        let result = reopened
            .query_messages(&archive, &MamQuery::default())
            .await
            .expect("query");
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].body.as_deref(), Some("persisted"));

        for cleanup in [
            path.clone(),
            PathBuf::from(format!("{}-shm", path.display())),
            PathBuf::from(format!("{}-wal", path.display())),
        ] {
            let _ = std::fs::remove_file(cleanup);
        }
    }

    /// Reproducer for "reactions don't survive a page reload".
    ///
    /// A body-less reaction stanza must round-trip through `MamStorage`
    /// with its `<reactions/>` payload intact: the rich-payload column
    /// is the only place the target id and emoji set live for a
    /// reaction-only row, so dropping it on read or write would
    /// invisibly delete every reaction from the archive.
    ///
    /// Two separate cases on purpose: in-memory SQLite (what the
    /// existing test suite uses) and a persistent SQLite file (closer
    /// to production). If the persistent variant fails while the
    /// in-memory one passes, the bug is backend-specific.
    #[tokio::test]
    async fn reaction_round_trips_through_in_memory_storage() {
        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");
        let archive_jid = jid("room@conference.example.com");

        let target_id = waddle_xmpp_core::mam::RichMessageId::new("room-stanza-original")
            .expect("non-empty target id");
        let thumbs_up =
            waddle_xmpp_core::mam::RichText::new("👍").expect("non-empty emoji literal");
        let reactions = waddle_xmpp_core::mam::ArchivedReactionSet {
            target_id: target_id.clone(),
            emojis: vec![thumbs_up.clone()],
        };
        let msg = ArchivedMessage {
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "room-stanza-reaction",
                archive_jid.clone(),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            rich: Some(waddle_xmpp_core::mam::ArchivedRichMessage {
                payload: Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(
                    reactions.clone(),
                )),
                ..Default::default()
            }),
            ..ArchivedMessage::for_test(archive_alice(&archive), archive_jid.clone())
        };

        storage.store_message(&archive, &msg).await.expect("store");

        let result = storage
            .query_messages(&archive, &MamQuery::default())
            .await
            .expect("query");

        assert_eq!(
            result.messages.len(),
            1,
            "reaction-only row must survive MAM query: {:?}",
            result
        );
        let archived = &result.messages[0];
        assert!(
            archived.body.is_none(),
            "reaction-only row must have no body: body={:?}",
            archived.body
        );
        let rich = archived.rich.as_ref().expect("rich payload survives");
        match rich.payload.as_ref() {
            Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(set)) => {
                assert_eq!(set.target_id.as_str(), "room-stanza-original");
                assert_eq!(
                    set.emojis.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
                    vec!["👍"]
                );
            }
            other => panic!("expected Reactions rich payload, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reaction_round_trips_through_persistent_sqlite() {
        let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        let path = artifacts.join(format!("mam-reaction-{}.db", uuid::Uuid::new_v4()));
        let database_url = format!("sqlite://{}", path.display());
        let archive = bare("room@conference.example.com");
        let archive_jid = jid("room@conference.example.com");

        let target_id = waddle_xmpp_core::mam::RichMessageId::new("room-stanza-original")
            .expect("non-empty target id");
        let thumbs_up =
            waddle_xmpp_core::mam::RichText::new("👍").expect("non-empty emoji literal");

        {
            let storage = SqlxMamStorage::open(&database_url).await.expect("storage");
            let msg = ArchivedMessage {
                stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                    "room-stanza-reaction",
                    archive_jid.clone(),
                )),
                message_type: xmpp_parsers::message::MessageType::Groupchat,
                rich: Some(waddle_xmpp_core::mam::ArchivedRichMessage {
                    payload: Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(
                        waddle_xmpp_core::mam::ArchivedReactionSet {
                            target_id: target_id.clone(),
                            emojis: vec![thumbs_up.clone()],
                        },
                    )),
                    ..Default::default()
                }),
                ..ArchivedMessage::for_test(archive_alice(&archive), archive_jid.clone())
            };
            storage.store_message(&archive, &msg).await.expect("store");
        }

        let reopened = SqlxMamStorage::open(&database_url).await.expect("reopen");
        let result = reopened
            .query_messages(&archive, &MamQuery::default())
            .await
            .expect("query");

        assert_eq!(
            result.messages.len(),
            1,
            "reaction-only row must survive MAM round-trip on persistent SQLite: {:?}",
            result
        );
        let archived = &result.messages[0];
        let rich = archived
            .rich
            .as_ref()
            .expect("rich payload survives a reopen");
        match rich.payload.as_ref() {
            Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(set)) => {
                assert_eq!(set.target_id.as_str(), "room-stanza-original");
                assert_eq!(
                    set.emojis.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
                    vec!["👍"]
                );
            }
            other => panic!("expected Reactions rich payload, got {other:?}"),
        }

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
        let archive = bare("room@conference.example.com");
        let archive_jid = jid("room@conference.example.com");
        let base = Utc::now();

        for (offset, body) in ["one", "two", "three", "four", "five", "six"]
            .into_iter()
            .enumerate()
        {
            let msg = ArchivedMessage {
                timestamp: base + ChronoDuration::seconds(offset as i64),
                body: Some(body.to_string()),
                ..ArchivedMessage::for_test(user_device(), archive_jid.clone())
            };
            storage.store_message(&archive, &msg).await.unwrap();
        }

        let last_page = storage
            .query_messages(
                &archive,
                &MamQuery {
                    max: Some(3),
                    before_id: Some(String::new()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let bodies: Vec<&str> = last_page
            .messages
            .iter()
            .map(|m| m.body.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(bodies, vec!["four", "five", "six"]);
        assert!(!last_page.complete);
    }

    fn archived_groupchat(archive: &BareJid) -> ArchivedMessage {
        ArchivedMessage {
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(archive_alice(archive), jid(&archive.to_string()))
        }
    }

    #[tokio::test]
    async fn xep_0424_tombstone_scrubs_parent_thread_id() {
        // XEP-0424 §Tombstones: replace `<body/>` and any related
        // elements which might leak information. `parent_thread_id`
        // identifies the parent thread and so must be cleared.
        use waddle_xmpp_core::mam::{ArchivedRichMessage, ArchivedTombstone, RichMessageId};

        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");
        let archive_jid = jid("room@conference.example.com");
        let msg = ArchivedMessage {
            body: Some("secret thread content".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "wire-id-1",
                archive_jid.clone(),
            )),
            thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::child(
                waddle_xmpp_core::mam::ThreadId::new("child-thread").expect("thread id"),
                waddle_xmpp_core::mam::ThreadId::new("root-thread").expect("parent id"),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client'><body>secret</body><thread parent='root-thread'>child-thread</thread></message>".to_string(),
            ),
            ..ArchivedMessage::for_test(archive_alice(&archive), archive_jid.clone())
        };
        let archive_id = storage.store_message(&archive, &msg).await.unwrap();

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

        assert!(row.body.is_none(), "body must be cleared");
        assert!(
            row.stanza_xml.is_none(),
            "stanza_xml must be cleared so the original wire form does not leak"
        );
        assert!(
            row.thread.is_none(),
            "thread (id and optional parent) is leak-prone, must be cleared"
        );
        assert!(
            row.reply.is_none(),
            "reply (id and optional sender JID) is leak-prone, must be cleared"
        );

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
        let archive = bare("room@conference.example.com");
        let archive_jid = jid("room@conference.example.com");
        let msg = ArchivedMessage {
            body: Some("moderated content".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "wire-id-2",
                archive_jid.clone(),
            )),
            thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::child(
                waddle_xmpp_core::mam::ThreadId::new("child-thread").expect("thread id"),
                waddle_xmpp_core::mam::ThreadId::new("root-thread").expect("parent id"),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client'><body>x</body><thread parent='root-thread'>child-thread</thread></message>".to_string(),
            ),
            ..ArchivedMessage::for_test(archive_alice(&archive), archive_jid.clone())
        };
        let archive_id = storage.store_message(&archive, &msg).await.unwrap();

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

        assert!(row.thread.is_none());
        assert!(row.body.is_none());
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

    #[tokio::test]
    async fn xep_0313_sqlx_archive_returns_messages_in_chronological_order() {
        // XEP-0313 §archive_order: results MUST be returned in the order the
        // client originally received them (chronological), with id used only
        // as a tiebreak. Sorting by id alone breaks this if id generation is
        // ever decoupled from receive time (custom assignment, backfill, etc.).
        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");
        let t0 = chrono::DateTime::parse_from_rfc3339("2026-05-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let t1 = t0 + chrono::Duration::seconds(10);

        // Earlier message gets the lexicographically *later* id, so id-only
        // ordering would invert the chronological sequence.
        let earlier = ArchivedMessage {
            id: "zzz-earlier".to_string(),
            timestamp: t0,
            body: Some("first".to_string()),
            ..archived_groupchat(&archive)
        };
        let later = ArchivedMessage {
            id: "aaa-later".to_string(),
            timestamp: t1,
            body: Some("second".to_string()),
            ..archived_groupchat(&archive)
        };

        storage.store_message(&archive, &later).await.unwrap();
        storage.store_message(&archive, &earlier).await.unwrap();

        let result = storage
            .query_messages(&archive, &MamQuery::default())
            .await
            .unwrap();

        let bodies: Vec<&str> = result
            .messages
            .iter()
            .map(|m| m.body.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(
            bodies,
            vec!["first", "second"],
            "MAM results must be in chronological order, not id order"
        );
    }

    #[tokio::test]
    async fn xep_0313_in_memory_archive_returns_messages_in_chronological_order() {
        let storage = InMemoryMamStorage::new();
        let archive = bare("room@conference.example.com");
        let t0 = chrono::DateTime::parse_from_rfc3339("2026-05-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let t1 = t0 + chrono::Duration::seconds(10);

        let earlier = ArchivedMessage {
            id: "zzz-earlier".to_string(),
            timestamp: t0,
            body: Some("first".to_string()),
            ..archived_groupchat(&archive)
        };
        let later = ArchivedMessage {
            id: "aaa-later".to_string(),
            timestamp: t1,
            body: Some("second".to_string()),
            ..archived_groupchat(&archive)
        };

        storage.store_message(&archive, &later).await.unwrap();
        storage.store_message(&archive, &earlier).await.unwrap();

        let result = storage
            .query_messages(&archive, &MamQuery::default())
            .await
            .unwrap();

        let bodies: Vec<&str> = result
            .messages
            .iter()
            .map(|m| m.body.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(
            bodies,
            vec!["first", "second"],
            "in-memory MAM ordering must be chronological"
        );
    }

    #[tokio::test]
    async fn xep_0313_sqlx_archive_uses_id_as_deterministic_tiebreak_when_timestamps_match() {
        // XEP-0313 §archive_order warns that "multiple messages may share the
        // same timestamp", so the order MUST still be deterministic. We use
        // archive id as the secondary key.
        let storage = create_test_storage().await;
        let archive = bare("room@conference.example.com");
        let t = chrono::DateTime::parse_from_rfc3339("2026-05-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let first = ArchivedMessage {
            id: "id-001".to_string(),
            timestamp: t,
            body: Some("first".to_string()),
            ..archived_groupchat(&archive)
        };
        let second = ArchivedMessage {
            id: "id-002".to_string(),
            timestamp: t,
            body: Some("second".to_string()),
            ..archived_groupchat(&archive)
        };

        // Insert out of id order to make the assertion meaningful.
        storage.store_message(&archive, &second).await.unwrap();
        storage.store_message(&archive, &first).await.unwrap();

        let result = storage
            .query_messages(&archive, &MamQuery::default())
            .await
            .unwrap();
        let ids: Vec<&str> = result.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["id-001", "id-002"],
            "tied timestamps must be ordered by archive id ascending"
        );
    }
}
