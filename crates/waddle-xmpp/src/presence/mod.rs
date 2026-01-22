//! Presence management.
//!
//! Handles XMPP presence stanzas including online/offline status,
//! custom status messages, typing indicators (XEP-0085), and
//! RFC 6121 presence subscription management.

pub mod subscription;

pub use subscription::{
    build_available_presence, build_subscription_presence, build_unavailable_presence,
    parse_subscription_presence, PendingSubscription, PresenceAction,
    PresenceSubscriptionRequest, SubscriptionStateMachine, SubscriptionType,
};

use chrono::{DateTime, Utc};
use jid::FullJid;
use serde::{Deserialize, Serialize};

/// User presence show status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Show {
    /// Available (default)
    #[default]
    Available,
    /// Away
    Away,
    /// Extended away
    Xa,
    /// Do not disturb
    Dnd,
    /// Free for chat
    Chat,
}

impl std::fmt::Display for Show {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Show::Available => write!(f, "available"),
            Show::Away => write!(f, "away"),
            Show::Xa => write!(f, "xa"),
            Show::Dnd => write!(f, "dnd"),
            Show::Chat => write!(f, "chat"),
        }
    }
}

/// User presence information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPresence {
    /// Full JID (with resource)
    pub jid: String,
    /// Show status
    pub show: Show,
    /// Custom status message
    pub status: Option<String>,
    /// Priority (-128 to 127)
    pub priority: i8,
    /// Last updated timestamp
    pub updated_at: DateTime<Utc>,
}

impl UserPresence {
    /// Create a new presence for a user coming online.
    pub fn online(jid: FullJid) -> Self {
        Self {
            jid: jid.to_string(),
            show: Show::Available,
            status: None,
            priority: 0,
            updated_at: Utc::now(),
        }
    }

    /// Check if this presence indicates the user is available.
    pub fn is_available(&self) -> bool {
        !matches!(self.show, Show::Dnd | Show::Xa)
    }
}

/// Chat state (XEP-0085).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatState {
    /// User is actively typing
    Composing,
    /// User was typing but stopped
    Paused,
    /// User is active in the chat
    Active,
    /// User is not active
    Inactive,
    /// User has left the chat
    Gone,
}

impl std::fmt::Display for ChatState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatState::Composing => write!(f, "composing"),
            ChatState::Paused => write!(f, "paused"),
            ChatState::Active => write!(f, "active"),
            ChatState::Inactive => write!(f, "inactive"),
            ChatState::Gone => write!(f, "gone"),
        }
    }
}
