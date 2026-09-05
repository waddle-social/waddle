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

use crate::Stanza;

mod connections;
mod outbound;
mod presence;
mod resources;
mod sending;
mod state;
mod subscriptions;

pub use outbound::{
    BroadcastOutcome, ConnectionEntry, DeliveryKind, ForceDetachOrigin, ForceDetachOutcome,
    ForceDetachRequest, OutboundStanza, OutboundWriteAcceptance, SendResult,
};
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
    /// Best-effort reverse index from XEP-0198 SM stream id to the full JID
    /// currently publishing it (ADR-0017 Phase 3 Slice 6). "Best-effort":
    /// entries are never proactively swept on every connection teardown
    /// path, so a caller MUST re-verify the returned JID's own
    /// `ConnectionEntry::sm_stream_id()` still matches before acting on
    /// it — see [`Self::sm_stream_owner`]'s doc comment. Used by the
    /// cross-node resume bridge to find which live connection (if any) to
    /// ask to force-detach.
    sm_stream_owners: DashMap<crate::pending_delivery::SmSessionId, FullJid>,
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
            sm_stream_owners: DashMap::new(),
        }
    }

    /// Publish (or clear) the XEP-0198 SM stream id a connection owns,
    /// updating both the entry's own field and the registry-wide reverse
    /// index (ADR-0017 Phase 3 Slice 6). Supersedes bare
    /// `ConnectionEntry::set_sm_stream_id` at every call site so the two
    /// never drift apart.
    pub fn set_sm_stream_id(
        &self,
        jid: &FullJid,
        stream_id: Option<crate::pending_delivery::SmSessionId>,
    ) {
        let Some(entry) = self.connections.get(jid) else {
            return;
        };
        if let Some(previous) = entry.sm_stream_id() {
            self.sm_stream_owners.remove(&previous);
        }
        entry.set_sm_stream_id(stream_id.clone());
        if let Some(stream_id) = stream_id {
            self.sm_stream_owners.insert(stream_id, jid.clone());
        }
    }

    /// Publish an SM stream id only while `owner` still owns the full-JID
    /// slot. The connection entry guard serializes the ownership check,
    /// entry update, and reverse-index update against same-JID replacement.
    /// Publish `generation` onto `jid`'s entry, owner-gated like the SM
    /// stream id: a racing same-FullJID replacement keeps its own (#1703).
    pub fn set_occupancy_session_if_owner(
        &self,
        jid: &FullJid,
        owner: &Arc<AtomicBool>,
        generation: waddle_xmpp_core::OccupancySessionGeneration,
    ) -> bool {
        let Some(entry) = self.connections.get(jid) else {
            return false;
        };
        if !Arc::ptr_eq(&entry.carbons_enabled, owner) {
            return false;
        }
        if let Ok(mut guard) = entry.occupancy_session.lock() {
            *guard = Some(generation);
        }
        true
    }

    /// The live connection's occupancy generation for `jid`, when published.
    pub fn occupancy_session_of(
        &self,
        jid: &FullJid,
    ) -> Option<waddle_xmpp_core::OccupancySessionGeneration> {
        self.connections
            .get(jid)
            .and_then(|entry| entry.occupancy_session())
    }

    pub fn set_sm_stream_id_if_owner(
        &self,
        jid: &FullJid,
        owner: &Arc<AtomicBool>,
        stream_id: Option<crate::pending_delivery::SmSessionId>,
    ) -> bool {
        let Some(entry) = self.connections.get(jid) else {
            return false;
        };
        if !Arc::ptr_eq(&entry.carbons_enabled, owner) {
            return false;
        }
        if let Some(previous) = entry.sm_stream_id() {
            self.sm_stream_owners.remove(&previous);
        }
        entry.set_sm_stream_id(stream_id.clone());
        if let Some(stream_id) = stream_id {
            self.sm_stream_owners.insert(stream_id, jid.clone());
        }
        true
    }

    /// Look up which full JID currently publishes `stream_id`, if any
    /// (ADR-0017 Phase 3 Slice 6). **Best-effort**: this index is not
    /// proactively swept on every teardown path, so callers MUST re-verify
    /// the returned entry's `ConnectionEntry::sm_stream_id()` still equals
    /// `stream_id` before acting on it (a stale reverse-index hit is always
    /// safely detectable this way, never silently acted upon).
    pub fn sm_stream_owner(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Option<FullJid> {
        self.sm_stream_owners
            .get(stream_id)
            .map(|entry| entry.value().clone())
    }

    /// Number of bare JIDs holding queued subscription stanzas for
    /// offline peers. Used by the server's `/debug/state-inventory`
    /// endpoint.
    pub fn pending_subscription_count(&self) -> usize {
        self.pending_subscription_stanzas.len()
    }

    /// Number of bare JIDs with tracked presence state. Used by the
    /// server's `/debug/state-inventory` endpoint.
    pub fn presence_state_count(&self) -> usize {
        self.presence_states.len()
    }

    /// Number of bare JIDs with recorded last-activity timestamps.
    /// Used by the server's `/debug/state-inventory` endpoint.
    pub fn last_activity_count(&self) -> usize {
        self.last_activity.len()
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
