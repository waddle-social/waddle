//! Connection Registry for real-time message routing.
//!
//! This module provides a thread-safe registry that tracks active XMPP connections
//! by their full JID, enabling real-time message routing between connections.
//!
//! ## Architecture
//!
//! Each connection registers a channel sender when it becomes established.
//! Messages can then be routed to any connected user by their JID.
//!
//! ```text
//! WebSocket C2S (user1@domain/resource1) <-> ConnectionRegistry <-> WebSocket C2S (user2@domain/resource2)
//!            |                                       |                           |
//!            v                                       v                           v
//!      mpsc::Sender                            DashMap<FullJid,              mpsc::Sender
//!                                               mpsc::Sender>
//! ```

mod connection_registry;
pub mod user_actor;
pub mod user_registry;

pub use connection_registry::{
    BroadcastOutcome, ConnectionEntry, ConnectionRegistry, DeliveryKind, OutboundStanza, SendResult,
};
pub use user_actor::delivery::{
    GetConnectionEntry, SelectRoutableResources, TrySendDirect, TrySendPeer, TrySendPendingFlush,
};
pub use user_registry::{
    GetOrCreateUser, GetUser, ListUsers, RegisterUserResource, RemoveUser, UnregisterUserResource,
    UserCount, UserRegistryActor, UserRegistryError,
};
