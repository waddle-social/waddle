//! Presence management.
//!
//! Handles XMPP presence stanzas including online/offline status,
//! custom status messages, typing indicators (XEP-0085), and
//! RFC 6121 presence subscription management.

pub mod subscription;

pub use subscription::{
    build_available_presence, build_subscription_presence, build_unavailable_presence,
    parse_subscription_presence, PendingSubscription, PresenceAction, PresenceSubscriptionRequest,
    SubscriptionStateMachine, SubscriptionType,
};
pub use waddle_xmpp_core::presence::{ChatState, Show, UserPresence};
