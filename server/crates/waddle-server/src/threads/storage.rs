//! Storage read for the global threads view. Pulls per-thread rows from
//! the existing `inbox_entries` table — no schema changes.

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jid::BareJid;
use serde::{Deserialize, Serialize};
use waddle_xmpp::inbox::storage::InboxStorageError;
use waddle_xmpp::inbox::InboxEntry;

use super::query::{ThreadEntry, ThreadsPage, MAX_PAGE_SIZE};
use crate::db::{Database, IntoParams};
use crate::inbox::{decode_inbox_row, INBOX_SELECT_COLS};

/// Read trait for the threads view. Implementations read from
/// `inbox_entries` (or a fixture for tests).
#[async_trait]
pub trait ThreadsStorage: Send + Sync {
    async fn page(
        &self,
        user_jid: &BareJid,
        page_size: u32,
        after_cursor: Option<&str>,
    ) -> Result<ThreadsPage, InboxStorageError>;
}

/// SQL-backed implementation, reading the same `inbox_entries` table as
/// the inbox feature.
#[derive(Clone)]
pub struct DatabaseThreadsStorage {
    db: Database,
}

impl DatabaseThreadsStorage {
    /// Construct from a shared logical database — the same one the
    /// inbox storage uses.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, InboxStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))
    }
}

/// Opaque RSM cursor — Base64URL-encoded JSON. Stable as long as the
/// `(last_updated, partner_jid, thread_id)` triple is preserved.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cursor {
    last_updated: i64,
    partner_jid: String,
    thread_id: String,
}

impl Cursor {
    fn from_entry(entry: &ThreadEntry) -> Self {
        Self {
            last_updated: entry.last_activity_secs,
            partner_jid: entry.channel.to_string(),
            thread_id: entry.thread_id.clone(),
        }
    }

    fn encode(&self) -> Result<String, InboxStorageError> {
        let json = serde_json::to_vec(self)
            .map_err(|error| InboxStorageError::Other(format!("cursor encode: {error}")))?;
        Ok(URL_SAFE_NO_PAD.encode(json))
    }

    fn decode(raw: &str) -> Result<Self, InboxStorageError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|error| InboxStorageError::Other(format!("cursor decode: {error}")))?;
        serde_json::from_slice::<Cursor>(&bytes)
            .map_err(|error| InboxStorageError::Other(format!("cursor json: {error}")))
    }
}

fn build_thread_entry(row: &InboxEntry) -> Option<ThreadEntry> {
    let thread_id = row.thread_id.clone()?;
    if thread_id.is_empty() {
        return None;
    }
    let root_author = row
        .author
        .as_ref()
        .and_then(|author| author.parse::<BareJid>().ok());
    Some(ThreadEntry {
        channel: row.partner.clone(),
        thread_id,
        last_stanza_id: row.last_stanza_id.clone(),
        last_activity_secs: row.last_updated,
        unread: row.unread,
        reply_count: row.reply_count,
        root_author,
        preview: row.preview.clone(),
        thread_title: row.thread_title.clone(),
    })
}

#[async_trait]
impl ThreadsStorage for DatabaseThreadsStorage {
    async fn page(
        &self,
        user_jid: &BareJid,
        page_size: u32,
        after_cursor: Option<&str>,
    ) -> Result<ThreadsPage, InboxStorageError> {
        let limit = page_size.clamp(1, MAX_PAGE_SIZE) as i64;

        // Page (seek-based pagination using the (last_updated, partner_jid,
        // thread_id) tuple as the sort key — stable under concurrent inserts).
        let mut entries: Vec<ThreadEntry> = Vec::new();
        let sql_base = format!(
            "SELECT {INBOX_SELECT_COLS} FROM inbox_entries \
             WHERE user_jid = ? AND thread_id != ''"
        );
        let mut rows = if let Some(cursor_raw) = after_cursor {
            let cursor = Cursor::decode(cursor_raw)?;
            let sql = format!(
                "{sql_base} AND (\
                    last_updated < ? \
                    OR (last_updated = ? AND partner_jid > ?) \
                    OR (last_updated = ? AND partner_jid = ? AND thread_id > ?)\
                  ) \
                  ORDER BY last_updated DESC, partner_jid ASC, thread_id ASC \
                  LIMIT ?"
            );
            self.query(
                &sql,
                crate::db_params![
                    user_jid.to_string(),
                    cursor.last_updated,
                    cursor.last_updated,
                    cursor.partner_jid.clone(),
                    cursor.last_updated,
                    cursor.partner_jid,
                    cursor.thread_id,
                    limit,
                ],
            )
            .await?
        } else {
            let sql = format!(
                "{sql_base} \
                 ORDER BY last_updated DESC, partner_jid ASC, thread_id ASC \
                 LIMIT ?"
            );
            self.query(&sql, crate::db_params![user_jid.to_string(), limit])
                .await?
        };

        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        {
            let inbox_entry = decode_inbox_row(&row)?;
            if let Some(thread_entry) = build_thread_entry(&inbox_entry) {
                entries.push(thread_entry);
            }
        }

        // Totals: a single round-trip over (count_all_threads,
        // count_unread_threads) for the same user.
        let mut totals_rows = self
            .query(
                "SELECT \
                   COUNT(*) AS total, \
                   COALESCE(SUM(CASE WHEN unread > 0 THEN 1 ELSE 0 END), 0) AS unread_threads \
                 FROM inbox_entries \
                 WHERE user_jid = ? AND thread_id != ''",
                crate::db_params![user_jid.to_string()],
            )
            .await?;
        let (total, unread_threads) = if let Some(row) = totals_rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        {
            let total: i64 = row
                .get(0)
                .map_err(|error| InboxStorageError::Other(error.to_string()))?;
            let unread_threads: i64 = row
                .get(1)
                .map_err(|error| InboxStorageError::Other(error.to_string()))?;
            (total.max(0) as u64, unread_threads.max(0) as u64)
        } else {
            (0, 0)
        };

        let first_cursor = entries.first().map(Cursor::from_entry);
        let last_cursor = entries.last().map(Cursor::from_entry);
        let first_cursor = match first_cursor {
            Some(c) => Some(c.encode()?),
            None => None,
        };
        let last_cursor = match last_cursor {
            Some(c) => Some(c.encode()?),
            None => None,
        };

        Ok(ThreadsPage {
            entries,
            total,
            unread_threads,
            first_cursor,
            last_cursor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::DatabaseInboxStorage;
    use waddle_xmpp::inbox::storage::InboxStorage;
    use waddle_xmpp::inbox::{ConversationKind, InboxEntry};

    fn jid(value: &str) -> BareJid {
        value.parse().expect("valid JID")
    }

    /// Build a paired (inbox storage, threads storage) backed by the same
    /// in-memory database. The inbox storage is used to seed rows; the
    /// threads storage reads via the same `Database` handle.
    async fn make_storage_pair() -> (DatabaseInboxStorage, DatabaseThreadsStorage) {
        let inbox = DatabaseInboxStorage::open(Some("sqlite::memory:"))
            .await
            .expect("open inbox storage");
        let threads = DatabaseThreadsStorage::new(inbox.db_handle());
        (inbox, threads)
    }

    #[tokio::test]
    async fn page_returns_only_thread_rows() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");

        // Seed: channel-level row (no thread) — must NOT appear.
        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room1@muc.example"),
                    ConversationKind::MucRoom,
                    "s0",
                    100,
                ),
                true,
            )
            .await
            .expect("upsert channel");

        // Seed: thread row in room1
        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room1@muc.example"),
                    ConversationKind::MucRoom,
                    "s1",
                    110,
                )
                .with_thread("t1"),
                true,
            )
            .await
            .expect("upsert t1");

        // Seed: thread row in room2
        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room2@muc.example"),
                    ConversationKind::MucRoom,
                    "s2",
                    120,
                )
                .with_thread("t2"),
                true,
            )
            .await
            .expect("upsert t2");

        let page = threads.page(&user, 50, None).await.expect("page");
        assert_eq!(
            page.entries.len(),
            2,
            "expected 2 thread rows, got {page:?}"
        );
        assert!(page.entries.iter().all(|e| !e.thread_id.is_empty()));
        assert_eq!(page.total, 2);
    }

    #[tokio::test]
    async fn page_orders_by_last_updated_desc() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");

        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room@muc.example"),
                    ConversationKind::MucRoom,
                    "older",
                    100,
                )
                .with_thread("t-old"),
                true,
            )
            .await
            .expect("upsert older");
        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room@muc.example"),
                    ConversationKind::MucRoom,
                    "newer",
                    200,
                )
                .with_thread("t-new"),
                true,
            )
            .await
            .expect("upsert newer");

        let page = threads.page(&user, 50, None).await.expect("page");
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].thread_id, "t-new");
        assert_eq!(page.entries[1].thread_id, "t-old");
    }

    #[tokio::test]
    async fn page_paginates_with_cursor() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");

        for (idx, ts) in [("t-a", 300_i64), ("t-b", 200), ("t-c", 100)] {
            inbox
                .upsert(
                    &user,
                    InboxEntry::new(
                        jid("room@muc.example"),
                        ConversationKind::MucRoom,
                        format!("s-{idx}"),
                        ts,
                    )
                    .with_thread(idx),
                    true,
                )
                .await
                .expect("upsert");
        }

        let first = threads.page(&user, 2, None).await.expect("first page");
        assert_eq!(first.entries.len(), 2);
        assert_eq!(first.entries[0].thread_id, "t-a");
        assert_eq!(first.entries[1].thread_id, "t-b");
        assert_eq!(first.total, 3);
        let cursor = first.last_cursor.clone().expect("last_cursor");

        let second = threads
            .page(&user, 2, Some(&cursor))
            .await
            .expect("second page");
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.entries[0].thread_id, "t-c");
        assert_eq!(second.total, 3);
    }

    #[tokio::test]
    async fn page_counts_unread_threads() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");

        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room@muc.example"),
                    ConversationKind::MucRoom,
                    "s1",
                    100,
                )
                .with_thread("t1"),
                true,
            )
            .await
            .expect("upsert t1");
        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room@muc.example"),
                    ConversationKind::MucRoom,
                    "s2",
                    200,
                )
                .with_thread("t2"),
                true,
            )
            .await
            .expect("upsert t2");
        // Mark t2 as read
        inbox
            .mark_read(&user, &jid("room@muc.example"), Some("t2"))
            .await
            .expect("mark read");

        let page = threads.page(&user, 50, None).await.expect("page");
        assert_eq!(page.total, 2);
        assert_eq!(page.unread_threads, 1);
    }
}
