//! Storage read for the global threads view. Pulls per-thread rows via
//! the existing `InboxStorage::list_all_threads` contract — no new
//! schema, and works with both the SQL and in-memory inbox backends.

use std::cmp::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jid::BareJid;
use serde::{Deserialize, Serialize};
use waddle_xmpp::inbox::storage::{InboxStorage, InboxStorageError};
use waddle_xmpp::inbox::InboxEntry;

use super::query::{
    ThreadEntry, ThreadRootAuthor, ThreadSort, ThreadStatusFilter, ThreadsError, ThreadsPage,
    ThreadsQuery, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};

#[derive(Debug, thiserror::Error)]
pub enum ThreadsStorageError {
    #[error("bad threads query: {0}")]
    BadRequest(#[from] ThreadsError),
    #[error(transparent)]
    Inbox(#[from] InboxStorageError),
}

/// Read trait for the threads view. Implementations read per-thread
/// inbox rows from whichever backend the inbox storage is using.
#[async_trait]
pub trait ThreadsStorage: Send + Sync {
    async fn page(
        &self,
        user_jid: &BareJid,
        query: &ThreadsQuery,
    ) -> Result<ThreadsPage, ThreadsStorageError>;
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

/// Opaque RSM cursor — Base64URL-encoded JSON. It records the active
/// sort mode plus the relevant sort keys (`last_updated`, `unread`,
/// `reply_count`) and stable tiebreakers (`partner_jid`, `thread_id`).
/// Reusing a cursor with a different sort mode is rejected as invalid.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cursor {
    sort: CursorSort,
    last_updated: i64,
    unread: u32,
    reply_count: u32,
    partner_jid: String,
    thread_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CursorSort {
    Recent,
    Unread,
    Replies,
}

impl From<ThreadSort> for CursorSort {
    fn from(sort: ThreadSort) -> Self {
        match sort {
            ThreadSort::Recent => Self::Recent,
            ThreadSort::Unread => Self::Unread,
            ThreadSort::Replies => Self::Replies,
        }
    }
}

impl Cursor {
    fn from_entry(entry: &ThreadEntry, sort: ThreadSort) -> Self {
        Self {
            sort: CursorSort::from(sort),
            last_updated: entry.last_activity_secs,
            unread: entry.unread,
            reply_count: entry.reply_count,
            partner_jid: entry.channel.to_string(),
            thread_id: entry.thread_id.clone(),
        }
    }

    fn encode(&self) -> Result<String, ThreadsStorageError> {
        let json = serde_json::to_vec(self)
            .map_err(|error| InboxStorageError::Other(format!("cursor encode: {error}")))?;
        Ok(URL_SAFE_NO_PAD.encode(json))
    }

    fn decode(raw: &str) -> Result<Self, ThreadsError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| ThreadsError::InvalidCursor)?;
        serde_json::from_slice::<Cursor>(&bytes).map_err(|_| ThreadsError::InvalidCursor)
    }
}

fn compare_entry_tiebreakers(
    left_channel: &BareJid,
    left_thread_id: &str,
    right_channel: &BareJid,
    right_thread_id: &str,
) -> Ordering {
    left_channel
        .cmp(right_channel)
        .then_with(|| left_thread_id.cmp(right_thread_id))
}

fn compare_entry_to_cursor_tiebreakers(
    entry_channel: &BareJid,
    entry_thread_id: &str,
    cursor_partner_jid: &str,
    cursor_thread_id: &str,
) -> Ordering {
    entry_channel
        .as_str()
        .cmp(cursor_partner_jid)
        .then_with(|| entry_thread_id.cmp(cursor_thread_id))
}

fn compare_entries(left: &ThreadEntry, right: &ThreadEntry, sort: ThreadSort) -> Ordering {
    match sort {
        ThreadSort::Recent => right.last_activity_secs.cmp(&left.last_activity_secs),
        ThreadSort::Unread => right
            .unread
            .cmp(&left.unread)
            .then_with(|| right.last_activity_secs.cmp(&left.last_activity_secs)),
        ThreadSort::Replies => right
            .reply_count
            .cmp(&left.reply_count)
            .then_with(|| right.last_activity_secs.cmp(&left.last_activity_secs)),
    }
    .then_with(|| {
        compare_entry_tiebreakers(
            &left.channel,
            &left.thread_id,
            &right.channel,
            &right.thread_id,
        )
    })
}

fn compare_entry_to_cursor(entry: &ThreadEntry, cursor: &Cursor, sort: ThreadSort) -> Ordering {
    match sort {
        ThreadSort::Recent => cursor.last_updated.cmp(&entry.last_activity_secs),
        ThreadSort::Unread => cursor
            .unread
            .cmp(&entry.unread)
            .then_with(|| cursor.last_updated.cmp(&entry.last_activity_secs)),
        ThreadSort::Replies => cursor
            .reply_count
            .cmp(&entry.reply_count)
            .then_with(|| cursor.last_updated.cmp(&entry.last_activity_secs)),
    }
    .then_with(|| {
        compare_entry_to_cursor_tiebreakers(
            &entry.channel,
            &entry.thread_id,
            &cursor.partner_jid,
            &cursor.thread_id,
        )
    })
}

fn entry_is_after_cursor(entry: &ThreadEntry, cursor: &Cursor, sort: ThreadSort) -> bool {
    compare_entry_to_cursor(entry, cursor, sort).is_gt()
}

fn channel_localpart(channel: &BareJid) -> String {
    channel
        .to_string()
        .split('@')
        .next()
        .unwrap_or("")
        .to_string()
}

fn string_matches(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(needle)
}

fn entry_matches_search(entry: &ThreadEntry, needle: &str) -> bool {
    entry
        .thread_title
        .as_deref()
        .is_some_and(|title| string_matches(title, needle))
        || entry
            .preview
            .as_deref()
            .is_some_and(|preview| string_matches(preview, needle))
        || entry
            .root_author
            .as_ref()
            .is_some_and(|author| string_matches(author.as_str(), needle))
        || string_matches(&entry.channel.to_string(), needle)
        || string_matches(&channel_localpart(&entry.channel), needle)
}

fn entry_matches_query(entry: &ThreadEntry, query: &ThreadsQuery) -> bool {
    match query.status {
        ThreadStatusFilter::All => {}
        ThreadStatusFilter::Unread if entry.unread == 0 => return false,
        ThreadStatusFilter::Following if entry.unread > 0 => return false,
        ThreadStatusFilter::Unread | ThreadStatusFilter::Following => {}
    }
    if let Some(active_since) = query.active_since_secs {
        if entry.last_activity_secs < active_since {
            return false;
        }
    }
    if let Some(ref channel) = query.channel {
        if &entry.channel != channel {
            return false;
        }
    }
    if let Some(ref search) = query.search {
        let needle = search.trim().to_lowercase();
        if !needle.is_empty() && !entry_matches_search(entry, &needle) {
            return false;
        }
    }
    true
}

fn page_size(query: &ThreadsQuery) -> usize {
    query
        .page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(0, MAX_PAGE_SIZE) as usize
}

fn build_thread_entry(row: &InboxEntry) -> Option<ThreadEntry> {
    let thread_id = row.thread_id.clone()?;
    if thread_id.is_empty() {
        return None;
    }
    let root_author = row.author.clone().and_then(ThreadRootAuthor::parse);
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
        query: &ThreadsQuery,
    ) -> Result<ThreadsPage, ThreadsStorageError> {
        let limit = page_size(query);
        let cursor = match query.after_cursor.as_deref() {
            Some(raw) => {
                let cursor = Cursor::decode(raw).map_err(ThreadsStorageError::BadRequest)?;
                if cursor.sort != CursorSort::from(query.sort) {
                    return Err(ThreadsStorageError::BadRequest(ThreadsError::InvalidCursor));
                }
                Some(cursor)
            }
            None => None,
        };

        let mut filtered: Vec<ThreadEntry> = self
            .inbox
            .list_all_threads(user_jid)
            .await?
            .iter()
            .filter_map(build_thread_entry)
            .filter(|entry| entry_matches_query(entry, query))
            .collect();
        filtered.sort_by(|left, right| compare_entries(left, right, query.sort));

        let total: u64 = filtered.len() as u64;
        let unread_threads: u64 = filtered.iter().filter(|e| e.unread > 0).count() as u64;
        if limit == 0 {
            return Ok(ThreadsPage {
                entries: Vec::new(),
                total,
                unread_threads,
                first_cursor: None,
                last_cursor: None,
            });
        }

        let mut entries: Vec<ThreadEntry> = Vec::with_capacity(limit);
        let mut has_more = false;
        for entry in filtered {
            if let Some(ref c) = cursor {
                if !entry_is_after_cursor(&entry, c, query.sort) {
                    continue;
                }
            }
            if entries.len() >= limit {
                has_more = true;
                break;
            }
            entries.push(entry);
        }

        let first_cursor = entries
            .first()
            .map(|entry| Cursor::from_entry(entry, query.sort));
        let last_cursor = has_more
            .then(|| {
                entries
                    .last()
                    .map(|entry| Cursor::from_entry(entry, query.sort))
            })
            .flatten();
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

    fn threads_query(page_size: u32) -> ThreadsQuery {
        ThreadsQuery {
            page_size: Some(page_size),
            ..Default::default()
        }
    }

    fn threads_query_after(page_size: u32, after_cursor: String) -> ThreadsQuery {
        ThreadsQuery {
            page_size: Some(page_size),
            after_cursor: Some(after_cursor),
            ..Default::default()
        }
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

        let page = threads.page(&user, &threads_query(50)).await.expect("page");
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

        let page = threads.page(&user, &threads_query(50)).await.expect("page");
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

        let first = threads
            .page(&user, &threads_query(2))
            .await
            .expect("first page");
        assert_eq!(first.entries.len(), 2);
        assert_eq!(first.entries[0].thread_id, "t-a");
        assert_eq!(first.entries[1].thread_id, "t-b");
        assert_eq!(first.total, 3);
        let cursor = first.last_cursor.clone().expect("last_cursor");

        let second = threads
            .page(&user, &threads_query_after(2, cursor))
            .await
            .expect("second page");
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.entries[0].thread_id, "t-c");
        assert_eq!(second.total, 3);
        assert!(
            second.last_cursor.is_none(),
            "final page should not advertise a continuation cursor"
        );
    }

    #[tokio::test]
    async fn page_size_zero_returns_count_only() {
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
            .expect("upsert");

        let page = threads.page(&user, &threads_query(0)).await.expect("page");
        assert!(page.entries.is_empty());
        assert_eq!(page.total, 1);
        assert_eq!(page.unread_threads, 1);
        assert!(page.first_cursor.is_none());
        assert!(page.last_cursor.is_none());
    }

    #[tokio::test]
    async fn malformed_cursor_is_bad_request() {
        let (_inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");

        let err = threads
            .page(&user, &threads_query_after(50, "not-a-cursor".into()))
            .await
            .expect_err("malformed cursor should fail");
        assert!(matches!(
            err,
            ThreadsStorageError::BadRequest(ThreadsError::InvalidCursor)
        ));
    }

    #[tokio::test]
    async fn cursor_reused_with_different_sort_is_bad_request() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");
        let room = jid("room@muc.example");

        upsert_unread_thread(&inbox, &user, &room, "high", 3, 100).await;
        upsert_unread_thread(&inbox, &user, &room, "low", 1, 300).await;

        let unread_page = threads
            .page(
                &user,
                &ThreadsQuery {
                    page_size: Some(1),
                    sort: ThreadSort::Unread,
                    ..Default::default()
                },
            )
            .await
            .expect("unread sorted page");
        let cursor = unread_page.last_cursor.expect("continuation cursor");

        let err = threads
            .page(
                &user,
                &ThreadsQuery {
                    page_size: Some(1),
                    after_cursor: Some(cursor),
                    sort: ThreadSort::Recent,
                    ..Default::default()
                },
            )
            .await
            .expect_err("cross-sort cursor reuse should fail");
        assert!(matches!(
            err,
            ThreadsStorageError::BadRequest(ThreadsError::InvalidCursor)
        ));
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

        let page = threads.page(&user, &threads_query(50)).await.expect("page");
        assert_eq!(page.total, 2);
        assert_eq!(page.unread_threads, 1);
    }

    #[tokio::test]
    async fn page_filters_by_status() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");
        let room = jid("room@muc.example");

        inbox
            .upsert(
                &user,
                InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s1", 100)
                    .with_thread("unread"),
                true,
            )
            .await
            .expect("upsert unread");
        inbox
            .upsert(
                &user,
                InboxEntry::new(room, ConversationKind::MucRoom, "s2", 200).with_thread("read"),
                false,
            )
            .await
            .expect("upsert read");

        let unread = threads
            .page(
                &user,
                &ThreadsQuery {
                    status: ThreadStatusFilter::Unread,
                    ..threads_query(50)
                },
            )
            .await
            .expect("unread page");
        assert_eq!(unread.entries.len(), 1);
        assert_eq!(unread.entries[0].thread_id, "unread");
        assert_eq!(unread.total, 1);
        assert_eq!(unread.unread_threads, 1);

        let following = threads
            .page(
                &user,
                &ThreadsQuery {
                    status: ThreadStatusFilter::Following,
                    ..threads_query(50)
                },
            )
            .await
            .expect("following page");
        assert_eq!(following.entries.len(), 1);
        assert_eq!(following.entries[0].thread_id, "read");
        assert_eq!(following.total, 1);
        assert_eq!(following.unread_threads, 0);
    }

    #[tokio::test]
    async fn page_filters_by_active_since() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");
        let room = jid("room@muc.example");

        for (thread_id, ts) in [("old", 100_i64), ("new", 200)] {
            inbox
                .upsert(
                    &user,
                    InboxEntry::new(room.clone(), ConversationKind::MucRoom, thread_id, ts)
                        .with_thread(thread_id),
                    true,
                )
                .await
                .expect("upsert");
        }

        let page = threads
            .page(
                &user,
                &ThreadsQuery {
                    active_since_secs: Some(150),
                    ..threads_query(50)
                },
            )
            .await
            .expect("page");
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].thread_id, "new");
    }

    #[tokio::test]
    async fn page_filters_by_channel_and_search() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");
        let chat = jid("chat@muc.waddle.chat");
        let other = jid("random@muc.waddle.chat");

        inbox
            .upsert(
                &user,
                InboxEntry::new(chat.clone(), ConversationKind::MucRoom, "s1", 100)
                    .with_thread("push")
                    .with_thread_title("Push debugging")
                    .with_preview("Notifications failed")
                    .with_author("alice"),
                true,
            )
            .await
            .expect("upsert push");
        inbox
            .upsert(
                &user,
                InboxEntry::new(other, ConversationKind::MucRoom, "s2", 200)
                    .with_thread("deploy")
                    .with_thread_title("Deploy notes")
                    .with_preview("Worker release"),
                true,
            )
            .await
            .expect("upsert deploy");

        let by_channel = threads
            .page(
                &user,
                &ThreadsQuery {
                    channel: Some(chat),
                    ..threads_query(50)
                },
            )
            .await
            .expect("channel page");
        assert_eq!(by_channel.total, 1);
        assert_eq!(by_channel.entries[0].thread_id, "push");

        let by_search = threads
            .page(
                &user,
                &ThreadsQuery {
                    search: Some("ALICE".into()),
                    ..threads_query(50)
                },
            )
            .await
            .expect("search page");
        assert_eq!(by_search.total, 1);
        assert_eq!(by_search.entries[0].thread_id, "push");

        let by_preview = threads
            .page(
                &user,
                &ThreadsQuery {
                    search: Some("worker".into()),
                    ..threads_query(50)
                },
            )
            .await
            .expect("preview search page");
        assert_eq!(by_preview.total, 1);
        assert_eq!(by_preview.entries[0].thread_id, "deploy");
    }

    #[tokio::test]
    async fn page_sorts_by_recent_unread_and_replies() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");
        let room = jid("room@muc.example");

        inbox
            .upsert(
                &user,
                InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s-recent", 300)
                    .with_thread("recent")
                    .with_reply_count(1),
                true,
            )
            .await
            .expect("upsert recent");
        inbox
            .upsert(
                &user,
                InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s-unread-1", 100)
                    .with_thread("unread"),
                true,
            )
            .await
            .expect("upsert unread 1");
        inbox
            .upsert(
                &user,
                InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s-unread-2", 110)
                    .with_thread("unread"),
                true,
            )
            .await
            .expect("upsert unread 2");
        inbox
            .upsert(
                &user,
                InboxEntry::new(room, ConversationKind::MucRoom, "s-replies", 200)
                    .with_thread("replies")
                    .with_reply_count(12),
                false,
            )
            .await
            .expect("upsert replies");

        let recent = threads
            .page(&user, &threads_query(50))
            .await
            .expect("recent page");
        assert_eq!(recent.entries[0].thread_id, "recent");

        let unread = threads
            .page(
                &user,
                &ThreadsQuery {
                    sort: ThreadSort::Unread,
                    ..threads_query(50)
                },
            )
            .await
            .expect("unread sort");
        assert_eq!(unread.entries[0].thread_id, "unread");

        let replies = threads
            .page(
                &user,
                &ThreadsQuery {
                    sort: ThreadSort::Replies,
                    ..threads_query(50)
                },
            )
            .await
            .expect("replies sort");
        assert_eq!(replies.entries[0].thread_id, "replies");
    }

    #[tokio::test]
    async fn page_cursor_stays_stable_with_filtered_sort() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");
        let room = jid("room@muc.example");

        for (thread_id, ts) in [("a", 300_i64), ("b", 200), ("c", 100)] {
            inbox
                .upsert(
                    &user,
                    InboxEntry::new(room.clone(), ConversationKind::MucRoom, thread_id, ts)
                        .with_thread(thread_id),
                    true,
                )
                .await
                .expect("upsert");
        }
        inbox
            .mark_read(&user, &room, Some("b"))
            .await
            .expect("mark read b");

        let first_query = ThreadsQuery {
            page_size: Some(1),
            status: ThreadStatusFilter::Unread,
            sort: ThreadSort::Recent,
            ..Default::default()
        };
        let first = threads.page(&user, &first_query).await.expect("first page");
        assert_eq!(first.entries.len(), 1);
        assert_eq!(first.entries[0].thread_id, "a");
        assert_eq!(first.total, 2);

        let second = threads
            .page(
                &user,
                &ThreadsQuery {
                    after_cursor: first.last_cursor.clone(),
                    ..first_query
                },
            )
            .await
            .expect("second page");
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.entries[0].thread_id, "c");
        assert_eq!(second.total, 2);
    }

    async fn upsert_unread_thread(
        inbox: &DatabaseInboxStorage,
        user: &BareJid,
        room: &BareJid,
        thread_id: &str,
        unread: u32,
        last_updated: i64,
    ) {
        for idx in 0..unread {
            inbox
                .upsert(
                    user,
                    InboxEntry::new(
                        room.clone(),
                        ConversationKind::MucRoom,
                        format!("s-{thread_id}-{idx}"),
                        last_updated,
                    )
                    .with_thread(thread_id),
                    true,
                )
                .await
                .expect("upsert unread thread");
        }
    }

    #[tokio::test]
    async fn page_cursor_stays_stable_with_unread_sort() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");
        let room = jid("room@muc.example");

        upsert_unread_thread(&inbox, &user, &room, "low", 1, 300).await;
        upsert_unread_thread(&inbox, &user, &room, "high", 3, 100).await;
        upsert_unread_thread(&inbox, &user, &room, "mid", 2, 200).await;

        let first_query = ThreadsQuery {
            page_size: Some(2),
            sort: ThreadSort::Unread,
            ..Default::default()
        };
        let first = threads.page(&user, &first_query).await.expect("first page");
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.thread_id.as_str())
                .collect::<Vec<_>>(),
            vec!["high", "mid"]
        );

        let second = threads
            .page(
                &user,
                &ThreadsQuery {
                    after_cursor: first.last_cursor.clone(),
                    ..first_query
                },
            )
            .await
            .expect("second page");
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.thread_id.as_str())
                .collect::<Vec<_>>(),
            vec!["low"]
        );
    }

    #[tokio::test]
    async fn page_cursor_stays_stable_with_replies_sort() {
        let (inbox, threads) = make_storage_pair().await;
        let user = jid("me@example.com");
        let room = jid("room@muc.example");

        for (thread_id, reply_count, last_updated) in
            [("few", 1_u32, 300_i64), ("many", 5, 100), ("some", 3, 200)]
        {
            inbox
                .upsert(
                    &user,
                    InboxEntry::new(
                        room.clone(),
                        ConversationKind::MucRoom,
                        format!("s-{thread_id}"),
                        last_updated,
                    )
                    .with_thread(thread_id)
                    .with_reply_count(reply_count),
                    false,
                )
                .await
                .expect("upsert replies thread");
        }

        let first_query = ThreadsQuery {
            page_size: Some(2),
            sort: ThreadSort::Replies,
            ..Default::default()
        };
        let first = threads.page(&user, &first_query).await.expect("first page");
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.thread_id.as_str())
                .collect::<Vec<_>>(),
            vec!["many", "some"]
        );

        let second = threads
            .page(
                &user,
                &ThreadsQuery {
                    after_cursor: first.last_cursor.clone(),
                    ..first_query
                },
            )
            .await
            .expect("second page");
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.thread_id.as_str())
                .collect::<Vec<_>>(),
            vec!["few"]
        );
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

        let page = threads.page(&user, &threads_query(50)).await.expect("page");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].thread_id, "t1");
    }
}
