//! Connection Registry implementation.
//!
//! Tracks active XMPP connections by their full JID for message routing.

use std::fmt;
use std::time::Instant;

use chrono::{DateTime, Utc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use jid::{BareJid, FullJid};
use tokio::sync::mpsc;
use tracing::{debug, info, instrument};

use crate::prometheus;
use crate::Stanza;

/// How the destination connection should handle an inbound
/// [`OutboundStanza`].
///
/// Introduced in #229 PR10 as type-level infrastructure for the
/// staged sans-I/O cutover. PR10 itself defaults every existing
/// caller to [`DeliveryKind::DirectFrame`] so behavior is unchanged.
/// PR11 wires the per-connection main loop to dispatch on this kind;
/// PR12 switches the [`OutboundEvent::RouteToConnection`] interpreter
/// arm to emit [`DeliveryKind::PeerStanza`] so the recipient pass
/// runs in production for peer-routed stanzas.
///
/// [`OutboundEvent::RouteToConnection`]: crate::protocol::OutboundEvent::RouteToConnection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryKind {
    /// **Peer-routed stanza** — the destination connection's main
    /// loop must feed this through its [`crate::protocol::XmppStateMachine`]
    /// as [`crate::protocol::InboundEvent::StanzaFromPeer`] so the
    /// recipient pass of the message pipeline runs (XEP-0191
    /// incoming block, XEP-0359 recipient stamp, XEP-0313 archive,
    /// XEP-0280 received-carbons, inbox projection) before any wire
    /// write. Unused in PR10; PR12 starts emitting it.
    PeerStanza,
    /// **Direct frame to wire** — the destination connection's main
    /// loop serializes the stanza and writes it to its WebSocket
    /// without any further protocol processing. Used for
    /// XEP-0280 carbon copies (already wrapped + processed by the
    /// sender), XEP-0198 SM acks, IQ replies built by the sender's
    /// state machine, and other server-generated frames.
    DirectFrame,
}

/// A stanza to be sent to a connection.
///
/// This is the message type sent through the outbound channel to
/// deliver stanzas to connected clients. The [`DeliveryKind`] tells
/// the destination's main loop whether the stanza should be fed
/// through the recipient-pass pipeline before reaching the wire.
#[derive(Debug, Clone)]
pub struct OutboundStanza {
    /// The stanza to send.
    pub stanza: Stanza,
    /// How the destination connection should handle the stanza.
    pub kind: DeliveryKind,
    /// `pending_delivery` row id when this stanza is the replay of a
    /// queued offline-delivery row (locked Q7b SM-ack lifecycle). The
    /// destination's main loop reads this after `record_outbound`
    /// assigns a new XEP-0198 outbound counter, then stamps the
    /// counter onto the row via
    /// [`crate::pending_delivery::storage::PendingDeliveryStorage::record_pushed_at`]
    /// so a subsequent SM `<a h>` ack can range-delete only those
    /// rows whose flush stanza was actually acknowledged. `None` for
    /// every other outbound (the common case).
    pub pending_row_id: Option<crate::pending_delivery::PendingRowId>,
    /// `original_receipt_at` of the source `pending_delivery` row
    /// when this stanza is a flush replay. The destination's main
    /// loop uses this to call
    /// [`crate::stream_management::StreamManagementState::record_outbound_with_receipt_at`]
    /// instead of `record_outbound`, so the SM unacked queue
    /// preserves the row's original receipt time. If the client
    /// disconnects pre-ack and the SM session later expires, Q6
    /// promotion re-creates a `pending_delivery` row whose
    /// `original_receipt_at` matches the original — so the eventual
    /// XEP-0203 `<delay/>` advertises the real failed-delivery time.
    /// (Greptile/Copilot/Qodo P1 review on PR #361.)
    pub pending_row_original_receipt_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl OutboundStanza {
    /// Create an outbound stanza that the destination's main loop
    /// writes directly to its wire — [`DeliveryKind::DirectFrame`].
    /// This is the default for all server-generated frames (carbon
    /// copies, IQ replies, SM acks, …). Until PR12 switches
    /// `RouteToConnection` to [`OutboundStanza::peer_stanza`],
    /// peer-routed stanzas also use this constructor and so do not
    /// trigger a recipient pass on the destination.
    pub fn new(stanza: Stanza) -> Self {
        Self {
            stanza,
            kind: DeliveryKind::DirectFrame,
            pending_row_id: None,
            pending_row_original_receipt_at: None,
        }
    }

    /// Create an outbound stanza tagged for the **recipient pass** —
    /// [`DeliveryKind::PeerStanza`]. The destination's main loop is
    /// expected to feed this through its state machine before any
    /// wire write. Used by the [`crate::protocol::OutboundEvent::RouteToConnection`]
    /// interpreter arm starting in #229 PR12.
    pub fn peer_stanza(stanza: Stanza) -> Self {
        Self {
            stanza,
            kind: DeliveryKind::PeerStanza,
            pending_row_id: None,
            pending_row_original_receipt_at: None,
        }
    }

    /// Create an outbound stanza that replays a queued
    /// `pending_delivery` row to a recovering session (locked Q7b
    /// SM-ack lifecycle). The destination's main loop uses
    /// [`Self::pending_row_id`] to bind the stanza's assigned
    /// XEP-0198 outbound counter back to the row so subsequent SM
    /// `<a h>` acks can range-delete it. `original_receipt_at` is
    /// the source row's receipt time — the recipient's main loop
    /// stamps it onto the unacked-queue entry so a future SM-expiry
    /// promotion re-creates the pending row with the correct
    /// XEP-0203 `<delay/>` time.
    pub fn for_pending_flush(
        stanza: Stanza,
        row_id: crate::pending_delivery::PendingRowId,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            stanza,
            kind: DeliveryKind::DirectFrame,
            pending_row_id: Some(row_id),
            pending_row_original_receipt_at: Some(original_receipt_at),
        }
    }
}

/// Connection state stored in the registry.
///
/// Contains the outbound sender and shared state that can be queried
/// by the registry (like carbons_enabled status for XEP-0280).
#[derive(Debug, Clone)]
pub struct ConnectionEntry {
    /// Channel to send stanzas to this connection
    pub sender: mpsc::Sender<OutboundStanza>,
    /// Whether XEP-0280 Message Carbons is enabled for this connection
    pub carbons_enabled: Arc<AtomicBool>,
    /// Whether this resource is currently available (presence type != unavailable)
    pub presence_available: Arc<AtomicBool>,
    /// Last advertised priority for this resource (-128..127)
    pub presence_priority: Arc<std::sync::atomic::AtomicI8>,
    /// Whether this stream requested its roster during the current session.
    pub roster_interested: Arc<AtomicBool>,
    /// Whether this stream has already received its XEP-0160 offline-message
    /// flush. Set on first non-negative-priority presence (locked Q7a +
    /// Q7d) so subsequent presence updates do not re-flush an already-
    /// drained `pending_delivery` queue (issue #209).
    pub offline_flushed: Arc<AtomicBool>,
    /// Per-connection XEP-0198 SM session id, set when the client
    /// enables SM (or resumes onto this connection). `None` while SM
    /// is disabled. Used directly by the offline-flush path for
    /// `claim_for_session` so each SM session has a distinct id and
    /// reconnect-on-same-resource never collides with the dead
    /// session's claimed pending_delivery rows (locked Q7b
    /// SM-ack lifecycle, issue #209). Stored as the typed
    /// [`crate::pending_delivery::SmSessionId`] so the registry
    /// boundary stays typed end-to-end (Qodo review on PR #358:
    /// previous `Option<String>` form violated the typed-payloads
    /// rule).
    pub sm_stream_id: Arc<std::sync::Mutex<Option<crate::pending_delivery::SmSessionId>>>,
}

impl ConnectionEntry {
    /// Create a new connection entry with carbons disabled by default.
    pub fn new(sender: mpsc::Sender<OutboundStanza>) -> Self {
        Self {
            sender,
            carbons_enabled: Arc::new(AtomicBool::new(false)),
            presence_available: Arc::new(AtomicBool::new(false)),
            presence_priority: Arc::new(std::sync::atomic::AtomicI8::new(0)),
            roster_interested: Arc::new(AtomicBool::new(false)),
            offline_flushed: Arc::new(AtomicBool::new(false)),
            sm_stream_id: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Get the carbons_enabled handle for this connection.
    ///
    /// The returned Arc can be used by the WebSocket C2S adapter to update
    /// the carbons status when enable/disable IQs are received.
    pub fn carbons_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.carbons_enabled)
    }

    /// Check if carbons is enabled for this connection.
    pub fn is_carbons_enabled(&self) -> bool {
        self.carbons_enabled.load(Ordering::Relaxed)
    }

    /// Check if this resource is currently available.
    pub fn is_presence_available(&self) -> bool {
        self.presence_available.load(Ordering::Relaxed)
    }

    /// Get the last advertised presence priority.
    pub fn presence_priority(&self) -> i8 {
        self.presence_priority.load(Ordering::Relaxed)
    }

    /// Atomically check-and-set the XEP-0160 offline-flushed flag. Returns
    /// `true` exactly once per connection (the first call), `false` on
    /// every subsequent call. Used to ensure the per-user-bare-JID flush
    /// (locked Q7c) only fires once per fresh session.
    pub fn claim_offline_flush(&self) -> bool {
        self.offline_flushed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Publish the XEP-0198 SM session id for this connection.
    /// Called by the websocket main loop after `<enable/>` (fresh
    /// SM session) and after `<resume/>` (continuation onto a
    /// previously-stored session). Used by the offline-flush path
    /// for `claim_for_session` (locked Q7b SM-ack lifecycle).
    pub fn set_sm_stream_id(&self, session_id: Option<crate::pending_delivery::SmSessionId>) {
        if let Ok(mut guard) = self.sm_stream_id.lock() {
            *guard = session_id;
        }
    }

    /// Read the XEP-0198 SM session id for this connection, if SM is
    /// enabled. Returns `None` while SM is disabled (no
    /// `pending_delivery` row will be claimed under an SM session id
    /// in that case — the flush falls back to the delete-on-push
    /// path).
    pub fn sm_stream_id(&self) -> Option<crate::pending_delivery::SmSessionId> {
        self.sm_stream_id
            .lock()
            .ok()
            .and_then(|g| g.as_ref().cloned())
    }
}

/// Result of attempting to send a message to a connection.
#[derive(Debug)]
pub enum SendResult {
    /// Message was successfully queued for delivery
    Sent,
    /// The recipient is not currently connected
    NotConnected,
    /// The channel to the recipient is closed
    ChannelClosed,
}

impl SendResult {
    /// True when the stanza was queued for delivery.
    pub fn is_sent(&self) -> bool {
        matches!(self, SendResult::Sent)
    }
}

/// Outcome of a non-blocking fan-out send via `try_send_to`.
///
/// Returning a typed outcome (rather than `bool`) forces callers to
/// distinguish delivery, absence, and the two silent-drop cases — the
/// previous `bool` API conflated them and they were all observed as
/// "just didn't get the message" from the recipient side. Each variant
/// bumps the matching Prometheus counter inside `try_send_to` so
/// per-site aggregation is optional for callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastOutcome {
    /// Stanza enqueued on the recipient's outbound channel.
    Delivered,
    /// No registry entry for the recipient (e.g. disconnected or SM-detached).
    NotConnected,
    /// Recipient's outbound channel is full; stanza dropped. The consumer
    /// is backpressured — a persistent non-zero rate of this outcome is
    /// the silent-message-loss symptom that PR #160's fan-out
    /// fire-and-forget path introduced.
    DroppedFull,
    /// Recipient's outbound channel was closed; stanza dropped and the
    /// stale registry entry has been evicted.
    DroppedClosed,
}

impl BroadcastOutcome {
    /// True iff the stanza was enqueued for delivery.
    pub fn is_delivered(self) -> bool {
        matches!(self, BroadcastOutcome::Delivered)
    }
}

/// Registry for tracking active XMPP connections.
///
/// Thread-safe registry that maps full JIDs to connection entries.
/// Uses DashMap for concurrent access without explicit locking.
///
/// ## Usage
///
/// ```ignore
/// let registry = ConnectionRegistry::new();
///
/// // When a connection is established:
/// let (tx, rx) = mpsc::channel(256);
/// let carbons_handle = registry.register(full_jid.clone(), tx);
///
/// // The connection can update carbons_handle when enable/disable IQs are received
/// carbons_handle.store(true, Ordering::Relaxed);
///
/// // When routing a message:
/// let result = registry.send_to(&recipient_jid, stanza).await;
///
/// // When a connection closes:
/// registry.unregister(&full_jid);
/// ```
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

    /// Register a connection with its outbound channel.
    ///
    /// Returns a handle to the carbons_enabled flag that the WebSocket C2S adapter
    /// can use to update the carbons status when enable/disable IQs are received.
    ///
    /// If a connection with the same JID already exists, it will be replaced.
    /// This handles reconnection scenarios where a client reconnects with
    /// the same resource before the old connection is cleaned up.
    #[instrument(skip(self, sender), fields(jid = %jid))]
    pub fn register(&self, jid: FullJid, sender: mpsc::Sender<OutboundStanza>) -> Arc<AtomicBool> {
        self.register_with_carbons(jid, sender, false)
    }

    /// Register a connection and seed its XEP-0280 carbons opt-in to
    /// `carbons_enabled`. Used by the XEP-0198 stream-resume path so a
    /// resumed stream keeps the carbons flag it negotiated before the
    /// disconnect instead of silently reverting to the disabled default.
    #[instrument(skip(self, sender), fields(jid = %jid, carbons = carbons_enabled))]
    pub fn register_with_carbons(
        &self,
        jid: FullJid,
        sender: mpsc::Sender<OutboundStanza>,
        carbons_enabled: bool,
    ) -> Arc<AtomicBool> {
        self.register_with_stream_state(jid, sender, carbons_enabled, false)
    }

    /// Register a connection and seed per-stream feature state.
    #[instrument(skip(self, sender), fields(jid = %jid, carbons = carbons_enabled, roster_interested = roster_interested))]
    pub fn register_with_stream_state(
        &self,
        jid: FullJid,
        sender: mpsc::Sender<OutboundStanza>,
        carbons_enabled: bool,
        roster_interested: bool,
    ) -> Arc<AtomicBool> {
        let entry = ConnectionEntry::new(sender);
        if carbons_enabled {
            entry.carbons_enabled.store(true, Ordering::Relaxed);
        }
        if roster_interested {
            entry.roster_interested.store(true, Ordering::Relaxed);
        }
        let carbons_handle = entry.carbons_handle();
        let existing = self.connections.insert(jid.clone(), entry);
        if existing.is_some() {
            debug!("Replaced existing connection registration");
        } else {
            prometheus::increment_connected_users();
            debug!("Registered new connection");
        }
        carbons_handle
    }

    /// Unregister a connection.
    ///
    /// Returns the connection entry if the connection was registered, None otherwise.
    #[instrument(skip(self), fields(jid = %jid))]
    pub fn unregister(&self, jid: &FullJid) -> Option<ConnectionEntry> {
        let removed = self.connections.remove(jid);
        if removed.is_some() {
            prometheus::decrement_connected_users();
            self.presence_states.remove(jid);
            debug!("Unregistered connection");
        } else {
            debug!("Connection was not registered");
        }
        removed.map(|(_, entry)| entry)
    }

    /// Unregister a connection only if the current registry entry belongs to
    /// the provided carbons handle (i.e. this actor still owns the slot).
    #[instrument(skip(self, carbons_handle), fields(jid = %jid))]
    pub fn unregister_if_owner(
        &self,
        jid: &FullJid,
        carbons_handle: &Arc<AtomicBool>,
    ) -> Option<ConnectionEntry> {
        let removed = self.connections.remove_if(jid, |_, entry| {
            Arc::ptr_eq(&entry.carbons_enabled, carbons_handle)
        });
        if removed.is_some() {
            prometheus::decrement_connected_users();
            self.presence_states.remove(jid);
            debug!("Unregistered owned connection");
        } else {
            debug!("Skipped unregister: ownership moved to replacement connection");
        }
        removed.map(|(_, entry)| entry)
    }

    /// Return the current entry only if it still belongs to the provided owner.
    pub fn entry_if_owner(
        &self,
        jid: &FullJid,
        carbons_handle: &Arc<AtomicBool>,
    ) -> Option<ConnectionEntry> {
        self.connections.get(jid).and_then(|entry| {
            if Arc::ptr_eq(&entry.carbons_enabled, carbons_handle) {
                Some(entry.clone())
            } else {
                None
            }
        })
    }

    /// Check if a JID is currently connected.
    pub fn is_connected(&self, jid: &FullJid) -> bool {
        self.connections.contains_key(jid)
    }

    /// Look up the [`ConnectionEntry`] for a registered full JID.
    ///
    /// Used by handlers that need to inspect or atomically transition
    /// per-connection flags (e.g. the XEP-0160 offline-flush CAS via
    /// [`ConnectionEntry::claim_offline_flush`]).
    pub fn get_entry(&self, jid: &FullJid) -> Option<ConnectionEntry> {
        self.connections.get(jid).map(|entry| entry.clone())
    }

    /// Get the number of active connections.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Send a stanza to a connected user as a [`DeliveryKind::DirectFrame`]
    /// — the destination's main loop writes it straight to the wire
    /// without running the recipient pass.
    ///
    /// This is the right call for server-generated frames (carbons,
    /// IQ replies, SM acks, …). For peer-routed stanzas that should
    /// run through the recipient pipeline, use [`Self::send_peer_to`].
    ///
    /// This waits for outbound channel capacity instead of dropping stanzas when
    /// a connection is temporarily backpressured. Closed channels are treated as
    /// stale connections and removed from the registry.
    #[instrument(skip(self, stanza), fields(to = %jid))]
    pub async fn send_to(&self, jid: &FullJid, stanza: Stanza) -> SendResult {
        self.send_to_with_kind(jid, stanza, DeliveryKind::DirectFrame)
            .await
    }

    /// Send a [`pending_delivery`](crate::pending_delivery) flush stanza
    /// to a recovering session. Identical to [`Self::send_to`] except
    /// the queued [`OutboundStanza`] carries the source row id so the
    /// destination's main loop can bind the stanza's assigned XEP-0198
    /// outbound counter back to the row (locked Q7b SM-ack lifecycle).
    #[instrument(skip(self, stanza), fields(to = %jid, row = %row_id))]
    pub async fn send_pending_flush(
        &self,
        jid: &FullJid,
        stanza: Stanza,
        row_id: crate::pending_delivery::PendingRowId,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    ) -> SendResult {
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().sender.clone(),
            None => {
                debug!("Recipient not connected for pending flush");
                return SendResult::NotConnected;
            }
        };
        let outbound = OutboundStanza::for_pending_flush(stanza, row_id, original_receipt_at);
        match sender.send(outbound).await {
            Ok(()) => SendResult::Sent,
            Err(_) => {
                self.remove_if_sender_closed_owner(jid, &sender);
                SendResult::ChannelClosed
            }
        }
    }

    /// Send a stanza to a connected user as a [`DeliveryKind::PeerStanza`]
    /// — the destination's main loop feeds
    /// [`crate::protocol::InboundEvent::StanzaFromPeer`] into its
    /// state machine so the recipient-pass pipeline (XEP-0191
    /// incoming block, XEP-0359 recipient stamp, XEP-0313 archive,
    /// XEP-0280 received-carbons, inbox projection) runs before any
    /// wire write.
    ///
    /// Used by the [`crate::protocol::OutboundEvent::RouteToConnection`]
    /// interpreter arm starting in #229 PR12.
    #[instrument(skip(self, stanza), fields(to = %jid))]
    pub async fn send_peer_to(&self, jid: &FullJid, stanza: Stanza) -> SendResult {
        self.send_to_with_kind(jid, stanza, DeliveryKind::PeerStanza)
            .await
    }

    /// Inner helper shared by [`Self::send_to`] and [`Self::send_peer_to`]
    /// so the queueing / replacement / channel-closed paths stay in
    /// one place. The only thing the public callers vary is the
    /// [`DeliveryKind`] tag on the queued [`OutboundStanza`], which
    /// is built via the same [`OutboundStanza::new`] / [`OutboundStanza::peer_stanza`]
    /// constructors every other call site uses — no manual struct
    /// literal here so the kind→constructor mapping stays in one
    /// place.
    async fn send_to_with_kind(
        &self,
        jid: &FullJid,
        stanza: Stanza,
        kind: DeliveryKind,
    ) -> SendResult {
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().sender.clone(),
            None => {
                debug!("Recipient not connected");
                return SendResult::NotConnected;
            }
        };

        let make_outbound = |s: Stanza| match kind {
            DeliveryKind::DirectFrame => OutboundStanza::new(s),
            DeliveryKind::PeerStanza => OutboundStanza::peer_stanza(s),
        };

        match sender.send(make_outbound(stanza.clone())).await {
            Ok(()) => {
                debug!(?kind, "Stanza queued for delivery");
                SendResult::Sent
            }
            Err(_) => {
                debug!("Outbound channel closed, connection may have dropped");
                self.remove_if_sender_closed_owner(jid, &sender);
                if let Some(entry) = self.connections.get(jid) {
                    let current = entry.value().sender.clone();
                    drop(entry);
                    if !current.same_channel(&sender) {
                        return match current.send(make_outbound(stanza)).await {
                            Ok(()) => {
                                debug!(?kind, "Stanza queued for replacement connection");
                                SendResult::Sent
                            }
                            Err(_) => {
                                self.remove_if_sender_closed_owner(jid, &current);
                                SendResult::ChannelClosed
                            }
                        };
                    }
                }
                SendResult::ChannelClosed
            }
        }
    }

    /// RFC 6121 §8.5.2.1 destination-resource selection for bare-JID
    /// 1:1 message routing.
    ///
    /// Returns every currently-connected resource of `bare_jid` whose
    /// advertised presence priority equals the maximum among the
    /// user's available resources. Per §8.5.2.1.1, only resources
    /// that have advertised availability (positive presence) are
    /// eligible; per §8.5.2.1.2, when multiple resources tie at the
    /// highest priority the server SHOULD deliver to all of them.
    ///
    /// Returns `Vec::new()` when the user has no available
    /// resources — caller should fall back to offline-storage
    /// semantics.
    pub fn select_routable_resources_for_user(&self, bare_jid: &BareJid) -> Vec<FullJid> {
        let candidates: Vec<(FullJid, i8)> = self
            .connections
            .iter()
            .filter(|entry| {
                entry.key().to_bare() == *bare_jid && entry.value().is_presence_available()
            })
            .map(|entry| (entry.key().clone(), entry.value().presence_priority()))
            .collect();
        let Some(max_priority) = candidates.iter().map(|(_, p)| *p).max() else {
            return Vec::new();
        };
        candidates
            .into_iter()
            .filter(|(_, p)| *p == max_priority)
            .map(|(jid, _)| jid)
            .collect()
    }

    /// Non-blocking send. Returns a typed `BroadcastOutcome` describing
    /// delivery, absence, or which silent-drop path was taken.
    ///
    /// Intended for fan-out paths (MUC broadcasts) where a slow or zombied
    /// consumer must never stall the producer task. On `Closed` the stale
    /// entry is evicted, but only if the current registry entry's sender is
    /// still closed — a concurrent `register` for the same FullJid may have
    /// installed a fresh, live sender between our `get` and `try_send`, and
    /// we must not wipe the newcomer. On `Full` the stanza is dropped
    /// without touching the registry (the consumer may just be catching up).
    ///
    /// Every outcome bumps a Prometheus counter so production drop rates
    /// are visible even when callers discard the return value.
    pub fn try_send_to(&self, jid: &FullJid, stanza: Stanza) -> BroadcastOutcome {
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().sender.clone(),
            None => {
                prometheus::increment_broadcast_not_connected();
                return BroadcastOutcome::NotConnected;
            }
        };

        let outbound = OutboundStanza::new(stanza);

        match sender.try_send(outbound) {
            Ok(()) => {
                prometheus::increment_broadcast_delivered();
                BroadcastOutcome::Delivered
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                prometheus::increment_broadcast_dropped_full();
                // Keep per-recipient detail at debug only — the
                // aggregated broadcast log at the call site already
                // reports a per-send `dropped_full` total, and
                // `waddle_broadcast_dropped_full_total` is always on.
                // A `warn!` here would turn into a log storm under
                // sustained fan-out backpressure (125+/s) and drown
                // out every other signal on the pod.
                debug!(
                    jid = %jid,
                    "Outbound channel full; broadcast stanza dropped"
                );
                BroadcastOutcome::DroppedFull
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                prometheus::increment_broadcast_dropped_closed();
                self.remove_if_sender_closed(jid);
                BroadcastOutcome::DroppedClosed
            }
        }
    }

    /// Race-safe eviction of a stale entry whose outbound channel is closed.
    ///
    /// Used on the non-blocking broadcast path to clean up zombies without
    /// risking the deletion of a live registration that happened to take
    /// over the slot between the caller's `get` and its `try_send`. If the
    /// currently-registered sender is still closed, the entry is removed
    /// and the connected-users metric and presence state are updated;
    /// otherwise this is a no-op.
    fn remove_if_sender_closed(&self, jid: &FullJid) {
        let removed = self
            .connections
            .remove_if(jid, |_, entry| entry.sender.is_closed());
        if removed.is_some() {
            prometheus::decrement_connected_users();
            self.presence_states.remove(jid);
            debug!(jid = %jid, "Evicted stale closed connection entry");
        }
    }

    /// Race-safe eviction for an awaited send failure.
    ///
    /// The async send path clones the sender before awaiting channel capacity.
    /// If another session replaces the same FullJid while the await is in
    /// progress, a failed send on the old channel must not unregister the new
    /// session. Match both closed state and channel identity.
    fn remove_if_sender_closed_owner(&self, jid: &FullJid, sender: &mpsc::Sender<OutboundStanza>) {
        let removed = self.connections.remove_if(jid, |_, entry| {
            entry.sender.is_closed() && entry.sender.same_channel(sender)
        });
        if removed.is_some() {
            prometheus::decrement_connected_users();
            self.presence_states.remove(jid);
            debug!(jid = %jid, "Evicted stale owned closed connection entry");
        }
    }

    /// Mark a connected resource as interested in roster pushes.
    ///
    /// RFC 6121 defines interested resources as those that requested the
    /// roster during this session. Roster pushes are sent only to these
    /// resources.
    pub fn mark_roster_interested(&self, jid: &FullJid) {
        if let Some(entry) = self.connections.get(jid) {
            entry
                .value()
                .roster_interested
                .store(true, Ordering::Relaxed);
        }
    }

    /// Check whether a connected resource is interested in roster pushes.
    pub fn is_roster_interested(&self, jid: &FullJid) -> bool {
        self.connections
            .get(jid)
            .map(|entry| entry.value().roster_interested.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// Get all connected interested resources for a bare JID.
    pub fn get_roster_interested_resources_for_user(&self, bare_jid: &BareJid) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| {
                entry.key().to_bare() == *bare_jid
                    && entry.value().roster_interested.load(Ordering::Relaxed)
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Send a stanza to multiple recipients.
    ///
    /// Returns a vector of (jid, result) pairs for each recipient.
    pub async fn send_to_many<'a, I>(
        &self,
        recipients: I,
        stanza: Stanza,
    ) -> Vec<(FullJid, SendResult)>
    where
        I: IntoIterator<Item = &'a FullJid>,
    {
        let mut results = Vec::new();

        for jid in recipients {
            let result = self.send_to(jid, stanza.clone()).await;
            results.push((jid.clone(), result));
        }

        results
    }

    /// List all connected JIDs.
    ///
    /// Useful for debugging and monitoring.
    pub fn list_connections(&self) -> Vec<FullJid> {
        self.connections.iter().map(|r| r.key().clone()).collect()
    }

    /// Snapshot every active connection's published XEP-0198 SM
    /// session id. Used by the `pending_delivery` claim-expiry
    /// janitor (issue #209 PR #360) to extend its "live SM session"
    /// set with currently-connected sessions — the
    /// `sm_session_registry` only knows about detached/resumable
    /// sessions, not active ones, so without this the janitor would
    /// wrongly treat actively-claimed-but-not-yet-acked rows as
    /// orphaned and release them. (Codex/Qodo review on PR #360.)
    pub fn active_sm_stream_ids(&self) -> Vec<crate::pending_delivery::SmSessionId> {
        self.connections
            .iter()
            .filter_map(|entry| entry.value().sm_stream_id())
            .collect()
    }

    /// Get all connected resources for a bare JID, excluding a specific full JID.
    ///
    /// Returns all full JIDs that match the bare JID except the excluded one.
    /// This does NOT filter by carbons status — callers that are routing
    /// XEP-0280 carbon copies should use [`Self::get_other_carbon_resources_for_user`]
    /// instead so that non-opted-in resources are not sent carbon-wrapped stanzas.
    pub fn get_other_resources_for_user(
        &self,
        bare_jid: &BareJid,
        exclude_jid: &FullJid,
    ) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| {
                let jid = entry.key();
                // Match bare JID but exclude the specific full JID
                jid.to_bare() == *bare_jid && jid != exclude_jid
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get all resources for a bare JID that have XEP-0280 Message Carbons
    /// enabled, excluding a specific full JID.
    ///
    /// Per XEP-0280 §5, carbons must be enabled per-resource. The server must
    /// only deliver `<sent>` and `<received>` carbon copies to resources that
    /// have explicitly opted in via `<enable xmlns='urn:xmpp:carbons:2'/>`.
    pub fn get_other_carbon_resources_for_user(
        &self,
        bare_jid: &BareJid,
        exclude_jid: &FullJid,
    ) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| {
                let jid = entry.key();
                jid.to_bare() == *bare_jid
                    && jid != exclude_jid
                    && entry.value().is_carbons_enabled()
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Check whether the given full JID has XEP-0280 Message Carbons enabled.
    ///
    /// Returns false if the JID is not connected.
    pub fn is_carbons_enabled(&self, jid: &FullJid) -> bool {
        self.connections
            .get(jid)
            .map(|entry| entry.value().is_carbons_enabled())
            .unwrap_or(false)
    }

    /// Update the XEP-0280 Message Carbons opt-in flag for a connected resource.
    ///
    /// Returns false when the resource is not currently connected.
    pub fn set_carbons_enabled(&self, jid: &FullJid, enabled: bool) -> bool {
        if let Some(entry) = self.connections.get(jid) {
            entry
                .value()
                .carbons_enabled
                .store(enabled, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Get all connected resources for a bare JID.
    ///
    /// Returns all full JIDs that match the given bare JID.
    /// Used for routing messages to all connected clients of a user.
    pub fn get_resources_for_user(&self, bare_jid: &BareJid) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| entry.key().to_bare() == *bare_jid)
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Queue a subscription stanza for an offline bare JID.
    ///
    /// These stanzas are delivered when the user next becomes available.
    pub fn queue_pending_subscription_stanza(&self, bare_jid: &BareJid, stanza: Stanza) {
        let mut pending = self
            .pending_subscription_stanzas
            .entry(bare_jid.clone())
            .or_default();
        if let Stanza::Presence(presence) = &stanza {
            if presence.type_ == xmpp_parsers::presence::Type::Subscribe {
                if let Some(requester) = presence.from.as_ref().map(|from| from.to_bare()) {
                    pending.retain(|queued| {
                        !matches!(
                            queued,
                            Stanza::Presence(queued_presence)
                                if queued_presence.type_ == xmpp_parsers::presence::Type::Subscribe
                                    && queued_presence
                                        .from
                                        .as_ref()
                                        .is_some_and(|from| from.to_bare() == requester)
                        )
                    });
                }
            }
        }
        pending.push(stanza);
    }

    /// Remove queued inbound subscribe stanzas from `requester` to `recipient`.
    pub fn remove_pending_subscribe(&self, recipient: &BareJid, requester: &BareJid) -> usize {
        let Some(mut entry) = self.pending_subscription_stanzas.get_mut(recipient) else {
            return 0;
        };
        let before = entry.len();
        entry.retain(|stanza| {
            !matches!(
                stanza,
                Stanza::Presence(presence)
                    if presence.type_ == xmpp_parsers::presence::Type::Subscribe
                        && presence
                            .from
                            .as_ref()
                            .is_some_and(|from| from.to_bare() == *requester)
            )
        });
        before - entry.len()
    }

    /// Drain and return all pending subscription stanzas for a bare JID.
    pub fn drain_pending_subscription_stanzas(&self, bare_jid: &BareJid) -> Vec<Stanza> {
        self.pending_subscription_stanzas
            .remove(bare_jid)
            .map(|(_, stanzas)| stanzas)
            .unwrap_or_default()
    }

    /// Return queued subscription stanzas for a bare JID without removing
    /// them. RFC 6121 pending inbound subscribe requests are re-delivered
    /// whenever the contact becomes available until approval or denial.
    pub fn pending_subscription_stanzas(&self, bare_jid: &BareJid) -> Vec<Stanza> {
        self.pending_subscription_stanzas
            .get(bare_jid)
            .map(|stanzas| stanzas.clone())
            .unwrap_or_default()
    }

    /// Update presence state for a connected resource.
    ///
    /// Returns true if the resource was found and updated.
    pub fn update_presence(&self, jid: &FullJid, available: bool, priority: i8) -> bool {
        if let Some(entry) = self.connections.get(jid) {
            entry
                .value()
                .presence_available
                .store(available, Ordering::Relaxed);
            entry
                .value()
                .presence_priority
                .store(priority, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Update full presence state (show/status/priority) for a connected resource.
    pub fn update_presence_state(
        &self,
        jid: &FullJid,
        show: Option<String>,
        status: Option<String>,
        priority: i8,
    ) {
        self.presence_states.insert(
            jid.clone(),
            PresenceState {
                show,
                status,
                priority,
            },
        );
    }

    /// Get the stored presence state for a connected resource.
    pub fn get_presence_state(&self, jid: &FullJid) -> Option<PresenceState> {
        self.presence_states.get(jid).map(|r| r.value().clone())
    }

    /// Clear the stored presence state for a resource (e.g. on unavailable presence).
    pub fn clear_presence_state(&self, jid: &FullJid) {
        self.presence_states.remove(jid);
    }

    /// Record last offline activity for a bare JID.
    pub fn record_last_activity(&self, bare_jid: &BareJid, status: Option<String>) {
        self.last_activity.insert(
            bare_jid.clone(),
            LastActivityState {
                timestamp: Utc::now(),
                status,
            },
        );
    }

    /// Get the last recorded offline activity for a bare JID.
    pub fn get_last_activity(&self, bare_jid: &BareJid) -> Option<LastActivityState> {
        self.last_activity
            .get(bare_jid)
            .map(|entry| entry.value().clone())
    }

    /// Clear the last recorded offline activity for a bare JID.
    pub fn clear_last_activity(&self, bare_jid: &BareJid) {
        self.last_activity.remove(bare_jid);
    }

    /// Return the current server uptime in whole seconds.
    pub fn server_uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Get all available resources for a bare JID with their priorities.
    pub fn get_available_resources_for_user(&self, bare_jid: &BareJid) -> Vec<(FullJid, i8)> {
        self.connections
            .iter()
            .filter(|entry| {
                entry.key().to_bare() == *bare_jid && entry.value().is_presence_available()
            })
            .map(|entry| (entry.key().clone(), entry.value().presence_priority()))
            .collect()
    }

    /// Remove all stale connections (those with closed channels).
    ///
    /// This can be called periodically to clean up connections that
    /// were not properly unregistered.
    pub fn cleanup_stale(&self) -> usize {
        let mut removed = 0;
        let stale: Vec<FullJid> = self
            .connections
            .iter()
            .filter(|entry| entry.value().sender.is_closed())
            .map(|entry| entry.key().clone())
            .collect();

        for jid in stale {
            if self.unregister(&jid).is_some() {
                debug!(jid = %jid, "Removed stale connection");
                removed += 1;
            }
        }

        if removed > 0 {
            info!(count = removed, "Cleaned up stale connections");
        }

        removed
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
mod tests {
    use super::*;
    use jid::Jid;
    use std::time::Duration;
    use xmpp_parsers::message::{Message, MessageType};

    fn test_jid(user: &str) -> FullJid {
        format!("{}@example.com/resource", user).parse().unwrap()
    }

    fn make_test_message(to: &str) -> Message {
        let bare_jid: jid::BareJid = to.parse().unwrap();
        let mut msg = Message::new(Some(Jid::from(bare_jid)));
        msg.type_ = MessageType::Chat;
        msg
    }

    #[test]
    fn test_registry_creation() {
        let registry = ConnectionRegistry::new();
        assert_eq!(registry.connection_count(), 0);
    }

    #[test]
    fn outbound_stanza_new_defaults_to_direct_frame() {
        // The default constructor preserves PR1-PR9 behavior — every
        // existing caller treats the destination's main loop as a
        // dumb wire-writer. The recipient-pass plumbing
        // (DeliveryKind::PeerStanza) lands later in the #229 staged
        // cutover.
        let msg = make_test_message("alice@example.com");
        let outbound = OutboundStanza::new(Stanza::Message(msg));
        assert_eq!(outbound.kind, DeliveryKind::DirectFrame);
    }

    #[test]
    fn outbound_stanza_peer_stanza_marks_kind_for_recipient_pass() {
        // The opt-in constructor used by `RouteToConnection` once
        // PR12 lands. The destination's main loop will dispatch on
        // `kind` and feed PeerStanza values through the recipient
        // pass before any wire write.
        let msg = make_test_message("bob@example.com");
        let outbound = OutboundStanza::peer_stanza(Stanza::Message(msg));
        assert_eq!(outbound.kind, DeliveryKind::PeerStanza);
    }

    #[test]
    fn test_register_connection() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, _rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);

        assert!(registry.is_connected(&jid));
        assert_eq!(registry.connection_count(), 1);
    }

    #[test]
    fn test_register_replaces_existing() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");

        let (tx1, _rx1) = mpsc::channel(16);
        let (tx2, _rx2) = mpsc::channel(16);

        registry.register(jid.clone(), tx1);
        registry.register(jid.clone(), tx2);

        // Should still only have one connection
        assert_eq!(registry.connection_count(), 1);
    }

    #[test]
    fn test_register_replacement_does_not_inherit_roster_interest() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "user@example.com/web".parse().unwrap();
        let bare = jid.to_bare();

        let (tx1, _rx1) = mpsc::channel(16);
        let (tx2, _rx2) = mpsc::channel(16);

        registry.register(jid.clone(), tx1);
        registry.mark_roster_interested(&jid);
        assert!(registry.is_roster_interested(&jid));

        registry.register(jid.clone(), tx2);
        assert!(!registry.is_roster_interested(&jid));
        assert!(registry
            .get_roster_interested_resources_for_user(&bare)
            .is_empty());
    }

    #[test]
    fn test_unregister_connection() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, _rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);
        assert!(registry.is_connected(&jid));

        let removed = registry.unregister(&jid);
        assert!(removed.is_some());
        assert!(!registry.is_connected(&jid));
        assert_eq!(registry.connection_count(), 0);
    }

    #[test]
    fn test_unregister_nonexistent() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");

        let removed = registry.unregister(&jid);
        assert!(removed.is_none());
    }

    #[tokio::test]
    async fn test_send_to_connected_user() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, mut rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);

        let msg = make_test_message("user1@example.com");
        let stanza = Stanza::Message(msg);

        let result = registry.send_to(&jid, stanza).await;
        assert!(matches!(result, SendResult::Sent));

        // Verify the message was received
        let received = rx.recv().await;
        assert!(received.is_some());
    }

    #[tokio::test]
    async fn test_send_to_disconnected_user() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");

        let msg = make_test_message("user1@example.com");
        let stanza = Stanza::Message(msg);

        let result = registry.send_to(&jid, stanza).await;
        assert!(matches!(result, SendResult::NotConnected));
    }

    #[tokio::test]
    async fn test_send_to_closed_channel() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);

        // Drop the receiver to close the channel
        drop(rx);

        let msg = make_test_message("user1@example.com");
        let stanza = Stanza::Message(msg);

        let result = registry.send_to(&jid, stanza).await;
        assert!(matches!(result, SendResult::ChannelClosed));

        // Connection should have been removed
        assert!(!registry.is_connected(&jid));
    }

    #[tokio::test]
    async fn test_send_to_closed_channel_does_not_remove_replacement() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (old_tx, old_rx) = mpsc::channel(1);
        let (new_tx, mut new_rx) = mpsc::channel(16);

        registry.register(jid.clone(), old_tx);
        assert!(matches!(
            registry
                .send_to(
                    &jid,
                    Stanza::Message(make_test_message("user1@example.com"))
                )
                .await,
            SendResult::Sent
        ));

        let send = registry.send_to(
            &jid,
            Stanza::Message(make_test_message("user1@example.com")),
        );
        tokio::pin!(send);
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut send)
            .await
            .is_err());

        registry.register(jid.clone(), new_tx);
        drop(old_rx);

        assert!(matches!(send.await, SendResult::Sent));
        assert!(new_rx.recv().await.is_some());
        assert!(registry.is_connected(&jid));
        assert!(matches!(
            registry
                .send_to(
                    &jid,
                    Stanza::Message(make_test_message("user1@example.com"))
                )
                .await,
            SendResult::Sent
        ));
        assert!(new_rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn test_send_to_waits_for_capacity_instead_of_dropping() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, mut rx) = mpsc::channel(1);

        registry.register(jid.clone(), tx);

        // Fill the channel so the next send must wait for capacity.
        let msg1 = make_test_message("user1@example.com");
        assert!(matches!(
            registry.send_to(&jid, Stanza::Message(msg1)).await,
            SendResult::Sent
        ));

        let msg2 = make_test_message("user1@example.com");
        let send = registry.send_to(&jid, Stanza::Message(msg2));
        tokio::pin!(send);

        assert!(tokio::time::timeout(Duration::from_millis(50), &mut send)
            .await
            .is_err());

        let first = rx.recv().await;
        assert!(first.is_some(), "first stanza should remain queued");

        let result = tokio::time::timeout(Duration::from_secs(1), &mut send)
            .await
            .expect("second send should complete once capacity is available");
        assert!(matches!(result, SendResult::Sent));

        let second = rx.recv().await;
        assert!(
            second.is_some(),
            "second stanza should be delivered after backpressure"
        );
    }

    #[test]
    fn test_list_connections() {
        let registry = ConnectionRegistry::new();

        let jid1 = test_jid("user1");
        let jid2 = test_jid("user2");

        let (tx1, _rx1) = mpsc::channel(16);
        let (tx2, _rx2) = mpsc::channel(16);

        registry.register(jid1.clone(), tx1);
        registry.register(jid2.clone(), tx2);

        let connections = registry.list_connections();
        assert_eq!(connections.len(), 2);
        assert!(connections.contains(&jid1));
        assert!(connections.contains(&jid2));
    }

    #[test]
    fn test_cleanup_stale() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);
        assert!(registry.is_connected(&jid));

        // Drop the receiver to make the channel stale
        drop(rx);

        let removed = registry.cleanup_stale();
        assert_eq!(removed, 1);
        assert!(!registry.is_connected(&jid));
    }

    #[tokio::test]
    async fn test_send_to_many() {
        let registry = ConnectionRegistry::new();

        let jid1 = test_jid("user1");
        let jid2 = test_jid("user2");
        let jid3 = test_jid("user3"); // Not registered

        let (tx1, mut rx1) = mpsc::channel(16);
        let (tx2, mut rx2) = mpsc::channel(16);

        registry.register(jid1.clone(), tx1);
        registry.register(jid2.clone(), tx2);

        let msg = make_test_message("room@muc.example.com");
        let stanza = Stanza::Message(msg);

        let recipients = vec![&jid1, &jid2, &jid3];
        let results = registry.send_to_many(recipients, stanza).await;

        assert_eq!(results.len(), 3);

        // Check results
        let result_map: std::collections::HashMap<_, _> = results.into_iter().collect();
        assert!(matches!(result_map.get(&jid1), Some(SendResult::Sent)));
        assert!(matches!(result_map.get(&jid2), Some(SendResult::Sent)));
        assert!(matches!(
            result_map.get(&jid3),
            Some(SendResult::NotConnected)
        ));

        // Verify messages were received
        assert!(rx1.recv().await.is_some());
        assert!(rx2.recv().await.is_some());
    }

    #[test]
    fn test_update_presence_and_get_available_resources() {
        let registry = ConnectionRegistry::new();

        let jid1: FullJid = "user@example.com/one".parse().unwrap();
        let jid2: FullJid = "user@example.com/two".parse().unwrap();
        let bare: BareJid = "user@example.com".parse().unwrap();

        let (tx1, _rx1) = mpsc::channel(16);
        let (tx2, _rx2) = mpsc::channel(16);
        registry.register(jid1.clone(), tx1);
        registry.register(jid2.clone(), tx2);

        // Default is unavailable until initial presence is sent.
        assert!(registry.get_available_resources_for_user(&bare).is_empty());

        assert!(registry.update_presence(&jid1, true, 5));
        assert!(registry.update_presence(&jid2, true, -1));

        let mut resources = registry.get_available_resources_for_user(&bare);
        resources.sort_by_key(|a| a.0.to_string());
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].0, jid1);
        assert_eq!(resources[0].1, 5);
        assert_eq!(resources[1].0, jid2);
        assert_eq!(resources[1].1, -1);
    }

    #[test]
    fn test_update_presence_missing_jid_returns_false() {
        let registry = ConnectionRegistry::new();
        let missing: FullJid = "missing@example.com/resource".parse().unwrap();
        assert!(!registry.update_presence(&missing, true, 1));
    }

    #[test]
    fn test_queue_and_drain_pending_subscription_stanzas() {
        let registry = ConnectionRegistry::new();
        let bare: BareJid = "user@example.com".parse().unwrap();

        let mut subscribe =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Subscribe);
        subscribe.to = Some(jid::Jid::from(bare.clone()));

        let mut unsubscribed =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unsubscribed);
        unsubscribed.to = Some(jid::Jid::from(bare.clone()));

        registry.queue_pending_subscription_stanza(&bare, Stanza::Presence(subscribe));
        registry.queue_pending_subscription_stanza(&bare, Stanza::Presence(unsubscribed));

        let drained = registry.drain_pending_subscription_stanzas(&bare);
        assert_eq!(drained.len(), 2);
        assert!(
            matches!(&drained[0], Stanza::Presence(p) if p.type_ == xmpp_parsers::presence::Type::Subscribe)
        );
        assert!(
            matches!(&drained[1], Stanza::Presence(p) if p.type_ == xmpp_parsers::presence::Type::Unsubscribed)
        );

        // Draining again should be empty.
        assert!(registry
            .drain_pending_subscription_stanzas(&bare)
            .is_empty());
    }

    #[test]
    fn test_pending_subscribe_is_deduplicated_and_not_drained_by_read() {
        let registry = ConnectionRegistry::new();
        let recipient: BareJid = "alice@example.com".parse().unwrap();
        let requester: BareJid = "bob@example.com".parse().unwrap();

        for _ in 0..2 {
            let mut subscribe =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Subscribe);
            subscribe.from = Some(jid::Jid::from(requester.clone()));
            subscribe.to = Some(jid::Jid::from(recipient.clone()));
            registry.queue_pending_subscription_stanza(&recipient, Stanza::Presence(subscribe));
        }

        assert_eq!(registry.pending_subscription_stanzas(&recipient).len(), 1);
        assert_eq!(registry.pending_subscription_stanzas(&recipient).len(), 1);
        assert_eq!(registry.remove_pending_subscribe(&recipient, &requester), 1);
        assert!(registry.pending_subscription_stanzas(&recipient).is_empty());
    }

    #[test]
    fn test_presence_state_tracking() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "user@example.com/resource".parse().unwrap();

        let (tx, _rx) = mpsc::channel(16);
        registry.register(jid.clone(), tx);

        // No state initially
        assert!(registry.get_presence_state(&jid).is_none());

        // Store presence state
        registry.update_presence_state(
            &jid,
            Some("away".to_string()),
            Some("Gone fishing".to_string()),
            5,
        );

        let state = registry.get_presence_state(&jid).expect("should exist");
        assert_eq!(state.show.as_deref(), Some("away"));
        assert_eq!(state.status.as_deref(), Some("Gone fishing"));
        assert_eq!(state.priority, 5);

        // Update with different values
        registry.update_presence_state(&jid, None, None, 0);
        let state = registry.get_presence_state(&jid).expect("should exist");
        assert!(state.show.is_none());
        assert!(state.status.is_none());
        assert_eq!(state.priority, 0);

        // Clean up on unregister
        registry.unregister(&jid);
        assert!(registry.get_presence_state(&jid).is_none());
    }

    /// XEP-0198 + XEP-0280: when a stream resumes the client expects its
    /// previous carbons opt-in to still be in effect. `register` creates a
    /// fresh entry with carbons disabled, so the resume path needs a variant
    /// that seeds the flag to the value captured when the session detached.
    #[test]
    fn test_register_with_carbons_seeds_initial_flag() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, _rx) = mpsc::channel(16);

        let handle = registry.register_with_carbons(jid.clone(), tx, true);

        assert!(
            handle.load(Ordering::Relaxed),
            "handle returned by register_with_carbons(.., true) should start enabled"
        );
        assert!(
            registry.is_carbons_enabled(&jid),
            "registry should report carbons as enabled for the seeded entry"
        );
    }

    #[test]
    fn test_register_with_carbons_false_leaves_disabled() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user2");
        let (tx, _rx) = mpsc::channel(16);

        let handle = registry.register_with_carbons(jid.clone(), tx, false);

        assert!(!handle.load(Ordering::Relaxed));
        assert!(!registry.is_carbons_enabled(&jid));
    }

    #[test]
    fn test_set_carbons_enabled_updates_existing_entry() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user3");
        let (tx, _rx) = mpsc::channel(16);
        registry.register(jid.clone(), tx);

        assert!(registry.set_carbons_enabled(&jid, true));
        assert!(registry.is_carbons_enabled(&jid));

        assert!(registry.set_carbons_enabled(&jid, false));
        assert!(!registry.is_carbons_enabled(&jid));
    }

    #[test]
    fn test_set_carbons_enabled_returns_false_for_missing_entry() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("missing");
        assert!(!registry.set_carbons_enabled(&jid, true));
    }

    #[test]
    fn test_try_send_to_dropped_full_keeps_connection_registered() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("full");
        let (tx, _rx) = mpsc::channel(1);
        registry.register(jid.clone(), tx);

        let first =
            registry.try_send_to(&jid, Stanza::Message(make_test_message("full@example.com")));
        let second =
            registry.try_send_to(&jid, Stanza::Message(make_test_message("full@example.com")));

        assert_eq!(first, BroadcastOutcome::Delivered);
        assert_eq!(second, BroadcastOutcome::DroppedFull);
        assert!(
            registry.is_connected(&jid),
            "full channel should not be evicted"
        );
    }

    #[test]
    fn test_try_send_to_closed_evicts_connection() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("closed");
        let (tx, rx) = mpsc::channel(1);
        registry.register(jid.clone(), tx);
        drop(rx);

        let outcome = registry.try_send_to(
            &jid,
            Stanza::Message(make_test_message("closed@example.com")),
        );

        assert_eq!(outcome, BroadcastOutcome::DroppedClosed);
        assert!(
            !registry.is_connected(&jid),
            "closed channel should be cleaned up"
        );
    }

    #[test]
    fn test_remove_if_sender_closed_keeps_new_live_registration() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("racy");

        let (tx_closed, rx_closed) = mpsc::channel(1);
        registry.register(jid.clone(), tx_closed);
        drop(rx_closed);

        let (tx_live, _rx_live) = mpsc::channel(1);
        registry.register(jid.clone(), tx_live);

        registry.remove_if_sender_closed(&jid);

        assert!(
            registry.is_connected(&jid),
            "race-safe stale cleanup must not remove a newly registered live sender"
        );
    }

    #[test]
    fn test_try_send_to_load_reports_single_delivery_then_drops() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("load");
        let (tx, _rx) = mpsc::channel(1);
        registry.register(jid.clone(), tx);

        let mut delivered = 0;
        let mut dropped_full = 0;
        for _ in 0..32 {
            match registry.try_send_to(&jid, Stanza::Message(make_test_message("load@example.com")))
            {
                BroadcastOutcome::Delivered => delivered += 1,
                BroadcastOutcome::DroppedFull => dropped_full += 1,
                other => panic!("unexpected outcome during load test: {other:?}"),
            }
        }

        assert_eq!(delivered, 1);
        assert_eq!(dropped_full, 31);
    }
}
