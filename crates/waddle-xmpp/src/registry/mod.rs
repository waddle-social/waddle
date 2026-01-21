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
//! ConnectionActor (user1@domain/resource1) <-> ConnectionRegistry <-> ConnectionActor (user2@domain/resource2)
//!            |                                       |                           |
//!            v                                       v                           v
//!      mpsc::Sender                            DashMap<FullJid,              mpsc::Sender
//!                                               mpsc::Sender>
//! ```

mod connection_registry;

pub use connection_registry::{ConnectionRegistry, OutboundStanza};
