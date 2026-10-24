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
    /// Available for message delivery.
    Available,
    /// Temporarily away from the client.
    Away,
    /// Away for an extended period.
    Xa,
    /// Requests that contacts avoid interruptions.
    Dnd,
    /// Available and actively interested in chatting.
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
    /// XMPP address identifying the referenced entity.
    pub jid: String,
    /// Advertised availability state.
    pub show: Show,
    /// Optional human-readable status text.
    pub status: Option<String>,
    /// Resource priority used by XMPP presence routing.
    pub priority: i8,
    /// Time this presence state was last updated.
    pub updated_at: DateTime<Utc>,
}

impl UserPresence {
    /// Create online presence with default priority and the current update timestamp.
    #[must_use]
    pub fn online(jid: FullJid) -> Self {
        Self {
            jid: jid.to_string(),
            show: Show::Available,
            status: None,
            priority: 0,
            updated_at: Utc::now(),
        }
    }

    /// Whether this presence state permits message delivery.
    #[must_use]
    pub fn is_available(&self) -> bool {
        !matches!(self.show, Show::Dnd | Show::Xa)
    }
}

/// Chat state (XEP-0085).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatState {
    /// The user is composing a message.
    Composing,
    /// The user has paused composition.
    Paused,
    /// The user is actively viewing the conversation.
    Active,
    /// The user is no longer actively viewing the conversation.
    Inactive,
    /// The user has left the conversation.
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
