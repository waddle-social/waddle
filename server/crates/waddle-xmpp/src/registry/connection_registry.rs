//! Connection Registry implementation.
//!
//! Tracks active XMPP connections by their full JID for message routing.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use jid::{BareJid, FullJid};
use tokio::sync::mpsc;
use tracing::{debug, info, instrument};

use crate::prometheus;
use crate::Stanza;

mod connections;
mod outbound;
mod presence;
mod resources;
mod sending;
mod state;
mod subscriptions;

pub use outbound::{BroadcastOutcome, ConnectionEntry, DeliveryKind, OutboundStanza, SendResult};
pub use state::{LastActivityState, PresenceState};

/// Registry for tracking active XMPP connections.
///
/// Thread-safe registry that maps full JIDs to connection entries.
/// Uses DashMap for concurrent access without explicit locking.
pub struct ConnectionRegistry {
    /// Map of full JID to connection entry (includes sender and carbons status)
    connections: DashMap<FullJid, ConnectionEntry>,
    /// Pending subscription stanzas for offline users (RFC 6121).
    pending_subscription_stanzas: DashMap<BareJid, Vec<Stanza>>,
    /// Per-resource presence state (show/status/priority) for probe responses.
    presence_states: DashMap<FullJid, PresenceState>,
    /// Last recorded offline activity for each bare JID.
    last_activity: DashMap<BareJid, LastActivityState>,
    /// Server start time used for XEP-0012 uptime responses.
    started_at: Instant,
}

impl ConnectionRegistry {
    /// Create a new connection registry.
    pub fn new() -> Self {
        info!("Creating connection registry");
        Self {
            connections: DashMap::new(),
            pending_subscription_stanzas: DashMap::new(),
            presence_states: DashMap::new(),
            last_activity: DashMap::new(),
            started_at: Instant::now(),
        }
    }
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ConnectionRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionRegistry")
            .field("connection_count", &self.connections.len())
            .finish()
    }
}

#[cfg(test)]
mod tests;
