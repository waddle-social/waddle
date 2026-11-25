//! Serializable thread listing records shared by the Flutter and browser clients.

use super::{FetchThreadsQuery, ThreadSort, ThreadStatusFilter, ThreadsPage};
use jid::BareJid;
use serde::{Deserialize, Serialize};

/// XEP-style payload for one entry in a urn:waddle:threads:0 response,
/// shaped for JS consumption.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleThreadEntry {
    /// Channel JID used to scope a thread or listing.
    pub channel: String,
    /// Identifier of the thread represented by this entry.
    pub thread_id: String,
    /// Stable identifier of the conversation or thread's latest message.
    pub last_stanza_id: String,
    /// RFC 3339 timestamp.
    pub last_activity: String,
    /// Number of unread messages in this conversation or thread.
    pub unread: u32,
    /// Number of replies recorded for this thread.
    pub reply_count: u32,
    /// Whether this thread contains unread messages.
    pub has_unread: bool,
    /// Author JID of the thread's root message, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_author: Option<String>,
    /// Optional short preview of the most recent or root message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Optional display title of the thread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_title: Option<String>,
    /// Call-thread anchor summary, present when this thread anchors a call.
    #[serde(rename = "callThread", skip_serializing_if = "Option::is_none")]
    pub call_thread: Option<WaddleThreadCallAnchor>,
    /// Ended-call summary, present when the anchored call has ended.
    #[serde(rename = "callThreadEnded", skip_serializing_if = "Option::is_none")]
    pub call_thread_ended: Option<WaddleThreadCallEnded>,
}

/// Call-thread anchor summary on a `urn:waddle:threads:0` entry.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleThreadCallAnchor {
    /// `"muc"` or `"dm"`.
    pub kind: String,
    /// Ordered media tokens, e.g. `["audio", "video"]`.
    pub media: Vec<String>,
}

/// Ended-call summary on a `urn:waddle:threads:0` entry.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleThreadCallEnded {
    /// RFC 3339 timestamp when the call ended.
    pub ended: String,
    /// ISO-8601 duration, e.g. `"PT5M"`.
    pub duration: String,
}

/// Paged response to a urn:waddle:threads:0 query, shaped for JS.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WaddleThreadsPage {
    /// Total number of threads matching the query.
    pub total: u64,
    /// Number of matching threads that contain unread messages.
    pub unread_threads: u64,
    /// Thread entries returned in this page.
    pub entries: Vec<WaddleThreadEntry>,
    /// Opaque cursor for requesting the next page, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Subset of threads requested from the server.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaddleThreadStatusFilter {
    /// All threads matching the other query filters.
    All,
    /// Only threads containing unread messages.
    Unread,
    /// Only threads followed by the requesting account.
    Following,
}

/// Ordering applied by the server to a thread listing.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaddleThreadSort {
    /// Most recently active threads first.
    Recent,
    /// Highest unread count first, with most recent activity breaking count ties.
    Unread,
    /// Threads with the most replies first.
    Replies,
}

/// Options bag for `fetch_threads`. All fields optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleFetchThreadsOptions {
    /// Maximum number of thread entries requested per page.
    pub page_size: Option<u32>,
    /// Opaque cursor returned by the preceding thread page.
    pub after_cursor: Option<String>,
    /// Filter for all, unread, or followed threads.
    pub status: Option<WaddleThreadStatusFilter>,
    /// RFC 3339 lower bound on the thread's most recent activity.
    pub active_since: Option<String>,
    /// Channel JID used to scope a thread or listing.
    pub channel: Option<String>,
    /// Text filter applied to the thread listing.
    pub search: Option<String>,
    /// Requested server-side ordering of thread entries.
    pub sort: Option<WaddleThreadSort>,
}

/// Validate user-selected filters and borrow them for one XMPP IQ.
///
/// # Errors
/// Returns an error for a malformed channel JID or activity timestamp.
pub fn fetch_threads_query_from_options(
    opts: &WaddleFetchThreadsOptions,
) -> Result<FetchThreadsQuery<'_>, String> {
    let status = match opts.status {
        Some(WaddleThreadStatusFilter::All) | None => ThreadStatusFilter::All,
        Some(WaddleThreadStatusFilter::Unread) => ThreadStatusFilter::Unread,
        Some(WaddleThreadStatusFilter::Following) => ThreadStatusFilter::Following,
    };
    let sort = match opts.sort {
        Some(WaddleThreadSort::Recent) | None => ThreadSort::Recent,
        Some(WaddleThreadSort::Unread) => ThreadSort::Unread,
        Some(WaddleThreadSort::Replies) => ThreadSort::Replies,
    };
    let active_since = match opts.active_since.as_deref() {
        Some(raw) => Some(
            chrono::DateTime::parse_from_rfc3339(raw)
                .map_err(|err| format!("invalid active_since: {err}"))?
                .with_timezone(&chrono::Utc),
        ),
        None => None,
    };
    let channel = match opts.channel.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(
            raw.parse::<BareJid>()
                .map_err(|err| format!("invalid channel: {err}"))?,
        ),
        _ => None,
    };

    Ok(FetchThreadsQuery {
        page_size: opts.page_size,
        after_cursor: opts.after_cursor.as_deref(),
        status,
        active_since,
        channel,
        search: opts.search.as_deref(),
        sort,
    })
}

impl From<ThreadsPage> for WaddleThreadsPage {
    fn from(page: ThreadsPage) -> Self {
        Self {
            total: page.total,
            unread_threads: page.unread_threads,
            entries: page
                .entries
                .into_iter()
                .map(|e| WaddleThreadEntry {
                    channel: e.channel,
                    thread_id: e.thread_id,
                    last_stanza_id: e.last_stanza_id,
                    last_activity: e.last_activity,
                    unread: e.unread,
                    reply_count: e.reply_count,
                    has_unread: e.has_unread,
                    root_author: e.root_author,
                    preview: e.preview,
                    thread_title: e.thread_title,
                    call_thread: e.call_thread.map(|c| WaddleThreadCallAnchor {
                        kind: c.kind.as_token().to_owned(),
                        media: c.media.token_list(),
                    }),
                    call_thread_ended: e.call_thread_ended.map(|c| WaddleThreadCallEnded {
                        ended: c.ended,
                        duration: c.duration,
                    }),
                })
                .collect(),
            next_cursor: page.next_cursor,
        }
    }
}
