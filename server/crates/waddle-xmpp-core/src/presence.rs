//! Presence management helpers shared by server and client crates.

pub mod subscription;

pub use subscription::{
    build_available_presence, build_subscription_presence, build_unavailable_presence,
    parse_subscription_presence, PendingSubscription, PresenceAction, PresenceSubscriptionRequest,
    SubscriptionType,
};

use chrono::{DateTime, Utc};
use jid::FullJid;
use serde::{Deserialize, Serialize};

/// User presence show status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Show {
    #[default]
    Available,
    Away,
    Xa,
    Dnd,
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
    pub jid: String,
    pub show: Show,
    pub status: Option<String>,
    pub priority: i8,
    pub updated_at: DateTime<Utc>,
}

impl UserPresence {
    pub fn online(jid: FullJid) -> Self {
        Self {
            jid: jid.to_string(),
            show: Show::Available,
            status: None,
            priority: 0,
            updated_at: Utc::now(),
        }
    }

    pub fn is_available(&self) -> bool {
        !matches!(self.show, Show::Dnd | Show::Xa)
    }
}

/// Chat state (XEP-0085).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatState {
    Composing,
    Paused,
    Active,
    Inactive,
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
