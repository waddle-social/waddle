use chrono::{DateTime, Utc};

/// Presence state for a connected resource (show, status, priority).
#[derive(Debug, Clone, Default)]
pub struct PresenceState {
    /// Presence show value (away, chat, dnd, xa) or None for default "available"
    pub show: Option<String>,
    /// Presence status text
    pub status: Option<String>,
    /// Presence priority (-128..127)
    pub priority: i8,
}

/// Last recorded offline activity for a bare JID.
#[derive(Debug, Clone)]
pub struct LastActivityState {
    /// Timestamp when the user last became offline.
    pub timestamp: DateTime<Utc>,
    /// Optional status text from the last unavailable presence.
    pub status: Option<String>,
}
