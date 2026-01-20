//! Message Archive Management (MAM) implementation.
//!
//! Implements XEP-0313 for message history storage and retrieval.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
