//! Typed request and response values for the threads query.

use chrono::{DateTime, Utc};
use jid::BareJid;
use waddle_xmpp::xep::{CallThreadDuration, CallThreadKind, CallThreadMedia};

/// `urn:waddle:threads:0` namespace.
pub const NS_THREADS: &str = "urn:waddle:threads:0";

/// Maximum entries the server will return on a single page when the
/// client omits `<set><max>`. Mirrors the existing inbox cap.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Hard cap on `<set><max>` requested by clients. Same cap as inbox.
pub const MAX_PAGE_SIZE: u32 = 200;

/// Status filter for the global threads view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreadStatusFilter {
    #[default]
    All,
    Unread,
    Following,
}

impl ThreadStatusFilter {
    pub fn parse(raw: &str) -> Result<Self, ThreadsError> {
        match raw {
            "all" => Ok(Self::All),
            "unread" => Ok(Self::Unread),
            "following" => Ok(Self::Following),
            other => Err(ThreadsError::InvalidStatus(other.to_string())),
        }
    }
}

/// Sort order for the global threads view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreadSort {
    #[default]
    Recent,
    Unread,
    Replies,
}

impl ThreadSort {
    pub fn parse(raw: &str) -> Result<Self, ThreadsError> {
        match raw {
            "recent" => Ok(Self::Recent),
            "unread" => Ok(Self::Unread),
            "replies" => Ok(Self::Replies),
            other => Err(ThreadsError::InvalidSort(other.to_string())),
        }
    }
}

/// A `<query xmlns='urn:waddle:threads:0'/>` request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadsQuery {
    /// RSM `<max>` requested by the client. Storage clamps this to
    /// `MAX_PAGE_SIZE` after parsing and treats `0` as a count-only request.
    pub page_size: Option<u32>,
    /// RSM `<after>` cursor — opaque string the server emitted previously.
    pub after_cursor: Option<String>,
    pub status: ThreadStatusFilter,
    /// RFC 3339 `active-since` converted to Unix seconds.
    pub active_since_secs: Option<i64>,
    pub channel: Option<BareJid>,
    pub search: Option<String>,
    pub sort: ThreadSort,
}

/// Display identity of the thread starter as materialised by the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRootAuthor(String);

impl ThreadRootAuthor {
    pub fn parse(raw: impl Into<String>) -> Option<Self> {
        let value = raw.into().trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ThreadRootAuthor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row in a `<threads>` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadEntry {
    pub channel: BareJid,
    pub thread_id: String,
    pub last_stanza_id: String,
    /// Unix seconds since epoch.
    pub last_activity_secs: i64,
    pub unread: u32,
    pub reply_count: u32,
    pub root_author: Option<ThreadRootAuthor>,
    pub preview: Option<String>,
    pub thread_title: Option<String>,
    /// Kind of call anchored to this thread (DM or MUC). `None` for
    /// non-call threads.
    pub call_thread_kind: Option<CallThreadKind>,
    /// Media negotiated for the anchored call (audio and/or video). `None`
    /// for non-call threads.
    pub call_thread_media: Option<CallThreadMedia>,
    /// When the anchored call ended. `Some` only once the call has ended.
    pub call_ended_at: Option<DateTime<Utc>>,
    /// Duration of the ended call (ISO 8601 `PT…`). `Some` only once the
    /// call has ended.
    pub call_duration: Option<CallThreadDuration>,
}

impl ThreadEntry {
    pub fn has_unread(&self) -> bool {
        self.unread > 0
    }
}

/// Full response payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadsPage {
    pub entries: Vec<ThreadEntry>,
    pub total: u64,
    pub unread_threads: u64,
    /// `<first>` cursor from RSM, opaque to clients.
    pub first_cursor: Option<String>,
    /// `<last>` cursor from RSM.
    pub last_cursor: Option<String>,
}

/// Errors returned by threads stanza parsing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ThreadsError {
    #[error("expected <{0}/> in '{NS_THREADS}'")]
    ExpectedElement(&'static str),
    #[error("invalid integer '{0}'")]
    InvalidInteger(String),
    #[error("invalid status filter '{0}'")]
    InvalidStatus(String),
    #[error("invalid sort '{0}'")]
    InvalidSort(String),
    #[error("invalid active-since timestamp '{0}'")]
    InvalidTimestamp(String),
    #[error("invalid channel JID '{0}'")]
    InvalidChannel(String),
    #[error("invalid RSM cursor")]
    InvalidCursor,
    #[error("payload is not the expected IQ type")]
    WrongIqType,
}
