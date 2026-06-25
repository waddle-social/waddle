use chrono::{DateTime, Utc};

/// Presence state for a connected resource (show, status, priority, idle).
#[derive(Debug, Clone, Default)]
pub struct PresenceState {
    /// Presence show value (away, chat, dnd, xa) or None for default "available"
    pub show: Option<String>,
    /// Presence status text
    pub status: Option<String>,
    /// Presence priority (-128..127)
    pub priority: i8,
    /// XEP-0319 last-interaction instant from the resource's `<idle/>`, so a
    /// probing contact's rebuilt presence carries the idle stamp too. `None`
    /// when the resource is interacting (no `<idle/>`).
    pub idle_since: Option<DateTime<Utc>>,
}

/// Last recorded offline activity for a bare JID.
#[derive(Debug, Clone)]
pub struct LastActivityState {
    /// Timestamp when the user last became offline.
    pub timestamp: DateTime<Utc>,
    /// Optional status text from the last unavailable presence.
    pub status: Option<String>,
}
