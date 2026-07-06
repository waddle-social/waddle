use chrono::{DateTime, Utc};
use minidom::Element;

/// Presence state for a connected resource (show, status, priority, idle).
#[derive(Debug, Clone, Default)]
pub struct PresenceState {
    /// Presence show value (away, chat, dnd, xa) or None for default "available"
    pub show: Option<String>,
    /// Presence status text
    pub status: Option<String>,
    /// Presence priority (-128..127)
    pub priority: i8,
    /// Extension children of the resource's last available presence
    /// (XEP-0115 `<c/>`, `<idle/>`, and any other payloads), relayed verbatim
    /// on probe/subscription delivery so the contact's own advertisements are
    /// never replaced by server-rebuilt ones (issue #1101).
    pub payloads: Vec<Element>,
}

/// Last recorded offline activity for a bare JID.
#[derive(Debug, Clone)]
pub struct LastActivityState {
    /// Timestamp when the user last became offline.
    pub timestamp: DateTime<Utc>,
    /// Optional status text from the last unavailable presence.
    pub status: Option<String>,
}
