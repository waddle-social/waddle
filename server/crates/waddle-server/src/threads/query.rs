//! Typed request and response values for the threads query.

use jid::BareJid;

/// `urn:waddle:threads:0` namespace.
pub const NS_THREADS: &str = "urn:waddle:threads:0";

/// Maximum entries the server will return on a single page when the
/// client omits `<set><max>`. Mirrors the existing inbox cap.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Hard cap on `<set><max>` requested by clients. Same cap as inbox.
pub const MAX_PAGE_SIZE: u32 = 200;

/// A `<query xmlns='urn:waddle:threads:0'/>` request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadsQuery {
    /// RSM `<max>` — clamped to `MAX_PAGE_SIZE` at parse time.
    pub page_size: Option<u32>,
    /// RSM `<after>` cursor — opaque string the server emitted previously.
    pub after_cursor: Option<String>,
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
    pub root_author: Option<BareJid>,
    pub preview: Option<String>,
    pub thread_title: Option<String>,
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
    #[error("payload is not the expected IQ type")]
    WrongIqType,
}
