//! Storage read for the global threads view. Pulls per-thread rows via
//! the existing `InboxStorage::list_all_threads` contract — no new
//! schema, and works with both the SQL and in-memory inbox backends.

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jid::BareJid;
use serde::{Deserialize, Serialize};
use waddle_xmpp::inbox::storage::{InboxStorage, InboxStorageError};
use waddle_xmpp::inbox::InboxEntry;

use super::query::{ThreadEntry, ThreadsPage, MAX_PAGE_SIZE};

/// Read trait for the threads view. Implementations read per-thread
/// inbox rows from whichever backend the inbox storage is using.
#[async_trait]
pub trait ThreadsStorage: Send + Sync {
    async fn page(
        &self,
        user_jid: &BareJid,
        page_size: u32,
        after_cursor: Option<&str>,
    ) -> Result<ThreadsPage, InboxStorageError>;
}

/// Default implementation: layers RSM-style pagination on top of an
/// `InboxStorage::list_all_threads` snapshot. The pagination is seek-
/// based over `(last_updated, partner_jid, thread_id)` so it's stable
/// under concurrent inserts that change the suffix of the list.
#[derive(Clone)]
pub struct InboxBackedThreadsStorage {
    inbox: Arc<dyn InboxStorage>,
}

impl InboxBackedThreadsStorage {
    pub fn new(inbox: Arc<dyn InboxStorage>) -> Self {
        Self { inbox }
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

/// Sort order: `last_updated DESC`, then `partner_jid ASC`, then
/// `thread_id ASC`. Inverted because the row with the *greatest*
/// `last_updated` is considered the "smallest" position in the ordered
/// list (it appears first).
fn entry_is_after_cursor(entry: &ThreadEntry, cursor: &Cursor) -> bool {
    if entry.last_activity_secs != cursor.last_updated {
        return entry.last_activity_secs < cursor.last_updated;
    }
    let partner = entry.channel.to_string();
    if partner != cursor.partner_jid {
        return partner > cursor.partner_jid;
    }
    entry.thread_id > cursor.thread_id
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
impl ThreadsStorage for InboxBackedThreadsStorage {
    async fn page(
        &self,
        user_jid: &BareJid,
        page_size: u32,
        after_cursor: Option<&str>,
    ) -> Result<ThreadsPage, InboxStorageError> {
        let limit = page_size.clamp(1, MAX_PAGE_SIZE) as usize;
        let cursor = match after_cursor {
            Some(raw) => Some(Cursor::decode(raw)?),
            None => None,
        };

        let mut all_rows = self.inbox.list_all_threads(user_jid).await?;
        // Backend MAY return rows already ordered; re-sort defensively
        // to make this layer the single source of truth for ordering.
        all_rows.sort_by(|a, b| {
            b.last_updated
                .cmp(&a.last_updated)
                .then_with(|| a.partner.to_string().cmp(&b.partner.to_string()))
                .then_with(|| {
                    a.thread_id
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.thread_id.as_deref().unwrap_or(""))
                })
        });

        let total: u64 = all_rows.len() as u64;
        let unread_threads: u64 = all_rows.iter().filter(|e| e.unread > 0).count() as u64;

        let mut entries: Vec<ThreadEntry> = Vec::with_capacity(limit);
        for row in all_rows {
            let Some(entry) = build_thread_entry(&row) else {
                continue;
            };
            if let Some(ref c) = cursor {
                if !entry_is_after_cursor(&entry, c) {
                    continue;
                }
            }
            entries.push(entry);
            if entries.len() >= limit {
                break;
            }
        }

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
    use waddle_xmpp::inbox::{ConversationKind, InboxEntry};

    fn jid(value: &str) -> BareJid {
        value.parse().expect("valid JID")
    }

    async fn make_storage_pair() -> (Arc<DatabaseInboxStorage>, InboxBackedThreadsStorage) {
        let inbox = Arc::new(
            DatabaseInboxStorage::open(Some("sqlite::memory:"))
                .await
                .expect("open inbox storage"),
        );
        let threads = InboxBackedThreadsStorage::new(inbox.clone());
        (inbox, threads)
    }

    #[tokio::test]
    async fn page_returns_only_thread_rows() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");

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
        assert_eq!(page.entries.len(), 2, "expected 2 thread rows");
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
        inbox
            .mark_read(&user, &jid("room@muc.example"), Some("t2"))
            .await
            .expect("mark read");

        let page = threads.page(&user, 50, None).await.expect("page");
        assert_eq!(page.total, 2);
        assert_eq!(page.unread_threads, 1);
    }

    #[tokio::test]
    async fn page_works_against_in_memory_storage() {
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let threads = InboxBackedThreadsStorage::new(inbox.clone());
        let user = jid("me@example.com");

        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room@muc.example"),
                    ConversationKind::MucRoom,
                    "s0",
                    100,
                ),
                true,
            )
            .await
            .expect("upsert channel");
        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room@muc.example"),
                    ConversationKind::MucRoom,
                    "s1",
                    110,
                )
                .with_thread("t1"),
                true,
            )
            .await
            .expect("upsert thread");

        let page = threads.page(&user, 50, None).await.expect("page");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].thread_id, "t1");
    }
}
