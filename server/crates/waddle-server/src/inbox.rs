//! libSQL-backed storage for XEP-0430 inbox projections.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use jid::BareJid;
use libsql::params::IntoParams;
use tracing::{info, instrument};
use waddle_xmpp::inbox::storage::{InboxStorage, InboxStorageError};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};

use crate::db::Database;

#[derive(Clone)]
pub struct LibSqlInboxStorage {
    db: Database,
}

impl LibSqlInboxStorage {
    pub async fn open(path: Option<&Path>) -> Result<Self, InboxStorageError> {
        let db = match path {
            Some(path) => Database::open_local("inbox", path)
                .await
                .map_err(|error| InboxStorageError::Other(error.to_string()))?,
            None => Database::in_memory("inbox")
                .await
                .map_err(|error| InboxStorageError::Other(error.to_string()))?,
        };
        let storage = Self { db };
        storage.initialize().await?;
        match path {
            Some(path) => info!(path = %path.display(), "Inbox storage initialized"),
            None => info!("Inbox storage initialized in memory"),
        }
        Ok(storage)
    }

    async fn initialize(&self) -> Result<(), InboxStorageError> {
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS inbox_entries (
                user_jid TEXT NOT NULL,
                partner_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                kind TEXT NOT NULL,
                last_stanza_id TEXT NOT NULL,
                last_updated INTEGER NOT NULL,
                unread INTEGER NOT NULL DEFAULT 0,
                preview TEXT,
                thread_title TEXT,
                reply_count INTEGER NOT NULL DEFAULT 0,
                author TEXT,
                PRIMARY KEY (user_jid, partner_jid, thread_id)
            );
            "#,
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_inbox_entries_user_updated ON inbox_entries (user_jid, last_updated DESC)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_inbox_entries_user_room_threads ON inbox_entries (user_jid, partner_jid, thread_id) WHERE thread_id != ''",
            (),
        )
        .await?;
        Ok(())
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<libsql::Rows, InboxStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))
    }

    async fn execute(&self, sql: &str, params: impl IntoParams) -> Result<u64, InboxStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))
    }

    fn decode_row(row: &libsql::Row) -> Result<InboxEntry, InboxStorageError> {
        let partner_raw: String = row
            .get(0)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let partner: BareJid = partner_raw
            .parse()
            .map_err(|error| InboxStorageError::Other(format!("invalid partner JID: {error}")))?;
        let thread_id_raw: String = row
            .get(1)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let kind_raw: String = row
            .get(2)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let last_stanza_id: String = row
            .get(3)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let last_updated: i64 = row
            .get(4)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let unread: i64 = row
            .get(5)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let preview: Option<String> = row
            .get(6)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let thread_title: Option<String> = row
            .get(7)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let reply_count: i64 = row
            .get(8)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let author: Option<String> = row
            .get(9)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;

        Ok(InboxEntry {
            partner,
            kind: decode_kind(&kind_raw)?,
            last_stanza_id,
            last_updated,
            unread: unread.max(0) as u32,
            preview,
            thread_id: if thread_id_raw.is_empty() {
                None
            } else {
                Some(thread_id_raw)
            },
            thread_title,
            reply_count: reply_count.max(0) as u32,
            author,
        })
    }
}

fn encode_kind(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Direct => "direct",
        ConversationKind::MucRoom => "muc",
    }
}

fn decode_kind(raw: &str) -> Result<ConversationKind, InboxStorageError> {
    match raw {
        "direct" => Ok(ConversationKind::Direct),
        "muc" => Ok(ConversationKind::MucRoom),
        other => Err(InboxStorageError::Other(format!(
            "unknown inbox conversation kind '{other}'"
        ))),
    }
}

const SELECT_COLS: &str = "partner_jid, thread_id, kind, last_stanza_id, last_updated, unread, preview, thread_title, reply_count, author";

#[async_trait]
impl InboxStorage for LibSqlInboxStorage {
    #[instrument(skip(self), fields(user = %user))]
    async fn list(&self, user: &BareJid) -> Result<Vec<InboxEntry>, InboxStorageError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM inbox_entries WHERE user_jid = ? AND thread_id = '' ORDER BY last_updated DESC, partner_jid ASC"
        );
        let mut rows = self.query(&sql, libsql::params![user.to_string()]).await?;

        let mut entries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        {
            entries.push(Self::decode_row(&row)?);
        }
        Ok(entries)
    }

    #[instrument(skip(self), fields(user = %user, room = %room))]
    async fn list_threads(
        &self,
        user: &BareJid,
        room: &BareJid,
    ) -> Result<Vec<InboxEntry>, InboxStorageError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM inbox_entries WHERE user_jid = ? AND partner_jid = ? AND thread_id != '' ORDER BY last_updated DESC"
        );
        let mut rows = self
            .query(&sql, libsql::params![user.to_string(), room.to_string()])
            .await?;

        let mut entries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        {
            entries.push(Self::decode_row(&row)?);
        }
        Ok(entries)
    }

    #[instrument(skip(self, entry), fields(user = %user, partner = %entry.partner))]
    async fn upsert(
        &self,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<InboxEntry, InboxStorageError> {
        let increment = i64::from(u8::from(increment_unread));
        let thread_id = entry.thread_id.as_deref().unwrap_or("");
        let is_thread = !thread_id.is_empty();
        let sql = format!(
            r#"
            INSERT INTO inbox_entries (
                user_jid, partner_jid, thread_id, kind, last_stanza_id, last_updated,
                unread, preview, thread_title, reply_count, author
            ) VALUES (?, ?, ?, ?, ?, ?, CASE WHEN ? != 0 THEN 1 ELSE 0 END, ?, ?, ?, ?)
            ON CONFLICT(user_jid, partner_jid, thread_id) DO UPDATE SET
                kind = excluded.kind,
                last_stanza_id = excluded.last_stanza_id,
                last_updated = excluded.last_updated,
                preview = excluded.preview,
                unread = CASE
                    WHEN ? != 0 THEN inbox_entries.unread + 1
                    ELSE inbox_entries.unread
                END,
                thread_title = COALESCE(excluded.thread_title, inbox_entries.thread_title),
                reply_count = CASE
                    WHEN {is_thread} THEN inbox_entries.reply_count + 1
                    ELSE inbox_entries.reply_count
                END,
                author = COALESCE(excluded.author, inbox_entries.author)
            RETURNING {SELECT_COLS}
            "#,
            is_thread = if is_thread { "1" } else { "0" },
        );
        let mut rows = self
            .query(
                &sql,
                libsql::params![
                    user.to_string(),
                    entry.partner.to_string(),
                    thread_id.to_string(),
                    encode_kind(entry.kind),
                    entry.last_stanza_id,
                    entry.last_updated,
                    increment,
                    entry.preview,
                    entry.thread_title,
                    entry.reply_count as i64,
                    entry.author,
                    increment,
                ],
            )
            .await?;

        let row = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
            .ok_or_else(|| InboxStorageError::Other("RETURNING produced no row".to_string()))?;

        Self::decode_row(&row)
    }

    #[instrument(skip(self), fields(user = %user, partner = %partner))]
    async fn mark_read(
        &self,
        user: &BareJid,
        partner: &BareJid,
        thread_id: Option<&str>,
    ) -> Result<(), InboxStorageError> {
        let tid = thread_id.unwrap_or("");
        self.execute(
            "UPDATE inbox_entries SET unread = 0 WHERE user_jid = ? AND partner_jid = ? AND thread_id = ?",
            libsql::params![user.to_string(), partner.to_string(), tid.to_string()],
        )
        .await?;
        Ok(())
    }

    #[instrument(skip(self), fields(user = %user))]
    async fn total_unread(&self, user: &BareJid) -> Result<u64, InboxStorageError> {
        let mut rows = self
            .query(
                "SELECT COALESCE(SUM(unread), 0) FROM inbox_entries WHERE user_jid = ? AND thread_id = ''",
                libsql::params![user.to_string()],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        else {
            return Ok(0);
        };
        let total: i64 = row
            .get(0)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        Ok(total.max(0) as u64)
    }
}

pub async fn build_inbox_storage(
    path: Option<PathBuf>,
) -> Result<Arc<dyn InboxStorage>, InboxStorageError> {
    Ok(Arc::new(LibSqlInboxStorage::open(path.as_deref()).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(value: &str) -> BareJid {
        value.parse().expect("valid JID")
    }

    #[tokio::test]
    async fn libsql_inbox_storage_round_trips_entries() {
        let storage = LibSqlInboxStorage::open(None).await.expect("storage");
        let user = jid("me@example.com");
        storage
            .upsert(
                &user,
                InboxEntry::new(jid("alice@example.com"), ConversationKind::Direct, "s1", 10)
                    .with_preview("hello"),
                true,
            )
            .await
            .expect("upsert");
        storage
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room@muc.example.com"),
                    ConversationKind::MucRoom,
                    "s2",
                    20,
                ),
                false,
            )
            .await
            .expect("upsert");

        let entries = storage.list(&user).await.expect("list");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].partner, jid("room@muc.example.com"));
        assert_eq!(storage.total_unread(&user).await.expect("unread"), 1);

        storage
            .mark_read(&user, &jid("alice@example.com"), None)
            .await
            .expect("mark read");
        assert_eq!(storage.total_unread(&user).await.expect("unread"), 0);
    }

    #[tokio::test]
    async fn libsql_inbox_storage_thread_entries() {
        let storage = LibSqlInboxStorage::open(None).await.expect("storage");
        let user = jid("me@example.com");
        let room = jid("room@muc.example.com");

        // Channel-level entry
        storage
            .upsert(
                &user,
                InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s1", 100),
                true,
            )
            .await
            .expect("upsert channel");

        // Thread entry
        let thread_entry = storage
            .upsert(
                &user,
                InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s2", 200)
                    .with_thread("t1")
                    .with_thread_title("Discussion")
                    .with_author("alice"),
                true,
            )
            .await
            .expect("upsert thread");
        assert_eq!(thread_entry.thread_id.as_deref(), Some("t1"));
        assert_eq!(thread_entry.thread_title.as_deref(), Some("Discussion"));

        // Channel list excludes threads
        let channels = storage.list(&user).await.expect("list");
        assert_eq!(channels.len(), 1);
        assert!(channels[0].thread_id.is_none());

        // Thread list for room
        let threads = storage
            .list_threads(&user, &room)
            .await
            .expect("list_threads");
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].thread_id.as_deref(), Some("t1"));
        assert_eq!(threads[0].author.as_deref(), Some("alice"));

        // Reply increments reply_count
        let updated = storage
            .upsert(
                &user,
                InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s3", 300)
                    .with_thread("t1"),
                true,
            )
            .await
            .expect("upsert reply");
        assert_eq!(updated.reply_count, 1);
        assert_eq!(updated.unread, 2);
        // Title preserved from first upsert
        assert_eq!(updated.thread_title.as_deref(), Some("Discussion"));

        // Mark thread read
        storage
            .mark_read(&user, &room, Some("t1"))
            .await
            .expect("mark thread read");
        let threads = storage
            .list_threads(&user, &room)
            .await
            .expect("list_threads");
        assert_eq!(threads[0].unread, 0);

        // Channel unread unaffected
        assert_eq!(storage.total_unread(&user).await.expect("unread"), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn libsql_inbox_storage_persists_file_backing() {
        let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        let path = artifacts.join(format!("inbox-{}.db", uuid::Uuid::new_v4()));
        let user = jid("me@example.com");

        {
            let storage = LibSqlInboxStorage::open(Some(&path))
                .await
                .expect("storage");
            storage
                .upsert(
                    &user,
                    InboxEntry::new(
                        jid("alice@example.com"),
                        ConversationKind::Direct,
                        "persisted",
                        30,
                    )
                    .with_preview("persisted"),
                    true,
                )
                .await
                .expect("upsert");
        }

        let reopened = LibSqlInboxStorage::open(Some(&path))
            .await
            .expect("reopened storage");
        let entries = reopened.list(&user).await.expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].last_stanza_id, "persisted");
        assert_eq!(entries[0].preview.as_deref(), Some("persisted"));
        assert_eq!(reopened.total_unread(&user).await.expect("unread"), 1);

        for cleanup in [
            path.clone(),
            PathBuf::from(format!("{}-shm", path.display())),
            PathBuf::from(format!("{}-wal", path.display())),
        ] {
            let _ = std::fs::remove_file(cleanup);
        }
    }
}
