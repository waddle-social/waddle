//! Message Archive Management (MAM) implementation.
//!
//! Implements XEP-0313 for message history storage and retrieval.
//!
//! ## Storage
//!
//! The [`storage`] module provides persistent storage backends for archived
//! messages. The primary implementation uses libSQL for database storage.
//!
//! ## IQ Handling
//!
//! MAM queries are received as IQ stanzas with the `urn:xmpp:mam:2` namespace.
//! The [`query`] module handles parsing and responding to these queries.

pub mod query;
pub mod storage;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use query::{
    add_stanza_id, build_fin_iq, build_result_messages, is_mam_query, parse_mam_query,
    MAM_NS, RSM_NS, STANZA_ID_NS,
};
pub use storage::{LibSqlMamStorage, MamStorage, MamStorageError};

/// Archived message metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedMessage {
    /// Unique message ID
    pub id: String,
    /// Timestamp when the message was received
    pub timestamp: DateTime<Utc>,
    /// Sender JID
    pub from: String,
    /// Recipient JID (room JID for MUC)
    pub to: String,
    /// Message body
    pub body: String,
    /// Original stanza ID (if present)
    pub stanza_id: Option<String>,
}

/// MAM query parameters.
#[derive(Debug, Clone, Default)]
pub struct MamQuery {
    /// Start time filter
    pub start: Option<DateTime<Utc>>,
    /// End time filter
    pub end: Option<DateTime<Utc>>,
    /// Filter by sender
    pub with: Option<String>,
    /// Maximum results to return
    pub max: Option<u32>,
    /// Pagination: before this ID
    pub before_id: Option<String>,
    /// Pagination: after this ID
    pub after_id: Option<String>,
}

/// MAM query result.
#[derive(Debug, Clone)]
pub struct MamResult {
    /// Retrieved messages
    pub messages: Vec<ArchivedMessage>,
    /// Whether there are more messages available
    pub complete: bool,
    /// First message ID in the result set
    pub first_id: Option<String>,
    /// Last message ID in the result set
    pub last_id: Option<String>,
    /// Total count (if available)
    pub count: Option<u32>,
}
