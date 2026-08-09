use super::*;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// Why the connection task is being asked to force-detach its live stream.
///
/// The connection's cleanup owns the normal actor-registry mirror.  The
/// exception is stale-actor retirement: its `UserRegistryActor` handler is
/// already waiting for this request's acknowledgement and will atomically
/// remove the retired actor after that acknowledgement.  Asking that same
/// registry from the connection cleanup would therefore deadlock the actor's
/// single-message turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ForceDetachOrigin {
    /// A cross-node XEP-0198 resume is replacing a still-live stream.
    CrossNodeResume,
    /// A `UserRegistryActor` is retiring a stale actor before claim reuse.
    RegistryStaleActorRetirement,
    /// An external lifecycle owner will remove its own registry mirrors after
    /// the connection task acknowledges the detach.
    OwnerManagedRetirement,
}

/// A request to force-detach a live connection from its own connection task.
/// Introduced for cross-node XEP-0198 resume (ADR-0017 Phase 3 Slice 6,
/// element 8's "live, owned elsewhere" branch) plus owner-managed retirement
/// flows that must let the connection task perform the destructive close while
/// the caller retains authority over any subsequent registry bookkeeping.
/// Delivered through [`ConnectionEntry::force_detach_tx`] into the owning
/// connection's own select loop, so the destructive detach-flush +
/// `<conflict/>` close runs on the connection's own task (never from an
/// external task reaching into its state directly).
#[derive(Debug)]
pub struct ForceDetachRequest {
    /// The lifecycle operation that owns the actor-registry bookkeeping.
    pub origin: ForceDetachOrigin,
    /// The bare JID whose live resource is being retired. Compared against
    /// this connection's own bound JID so the destructive close is always
    /// identity-gated, including on the cross-node resume path that already
    /// checked the registry's reverse index before sending.
    pub requester_bare_jid: jid::BareJid,
    /// Answered exactly once, after the connection either force-detaches
    /// (identity matched) or declines (identity mismatch).
    pub ack: tokio::sync::oneshot::Sender<ForceDetachOutcome>,
}

/// Outcome of a [`ForceDetachRequest`], reported back to the asker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ForceDetachOutcome {
    /// Identity matched, the connection sent `<conflict/>` and closed, AND
    /// its subsequent XEP-0198 detach-for-resume cleanup actually persisted
    /// a resumable snapshot (council-adjudicated fix: this is the ONLY
    /// variant that authorizes the asker to proceed with
    /// `steal_for_resume` — see [`Self::NotPersisted`] for every other
    /// outcome of that same cleanup pass).
    Detached,
    /// The requester's bare JID did not match this connection's own bound
    /// JID — the destructive close was refused.
    IdentityMismatch,
    /// Identity matched and the connection closed, but its detach-for
    /// -resume cleanup did NOT end in a persisted, resumable snapshot
    /// (council-adjudicated fix — a storage-write failure that fell back to
    /// full cleanup, an ownership race that promoted the queue instead of
    /// storing it, no registry ownership at all, or any other early-return
    /// path in `cleanup_connection_shutdown`). Distinct from
    /// [`Self::Detached`] so the asker never proceeds with
    /// `steal_for_resume` against a snapshot that was never actually
    /// written — the caller maps this the same as
    /// `LocalForcedDetachOutcome::NotLiveLocally` (re-check persistence and
    /// retry).
    NotPersisted,
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
    /// Whether this stream requested its XEP-0191 blocklist during the current session.
    pub blocklist_interested: Arc<AtomicBool>,
    /// Whether this stream has already received its XEP-0160 offline-message
    /// flush. Set on first non-negative-priority presence (locked Q7a +
    /// Q7d) so subsequent presence updates do not re-flush an already-
    /// drained `pending_delivery` queue (issue #209).
    pub offline_flushed: Arc<AtomicBool>,
    /// Whether this stream has already received the queued inbound
    /// subscription requests for its bare JID. RFC 6121 §3.1.3: pending
    /// inbound `<presence type='subscribe'/>` stanzas are delivered on the
    /// resource's INITIAL available presence — not on every subsequent
    /// presence update within the session (issue #1104: auto-away flips
    /// must not re-prompt the user). Claimed once per connection via
    /// [`ConnectionEntry::claim_pending_subscribes_flush`].
    pub pending_subscribes_flushed: Arc<AtomicBool>,
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
    /// Sender half of this connection's force-detach control channel
    /// (ADR-0017 Phase 3 Slice 6). The receiver half is handed to the
    /// connection's own task exactly once via [`Self::take_force_detach_rx`]
    /// — a fresh `mpsc` pair is minted per `ConnectionEntry` in [`Self::new`],
    /// isolated from the stanza-delivery `sender`/`OutboundStanza` pipeline
    /// entirely. Capacity [`FORCE_DETACH_CHANNEL_CAPACITY`] (deliberately
    /// greater than 1, not the "only one ever needed" capacity 1): the
    /// connection's select loop `recv()`s and acts on at most one request
    /// before breaking out, so a second, concurrent force-detach ask (the
    /// two-simultaneous-live-resume race, ADR-0017 Phase 3 plan Slice 6)
    /// should not usually find the channel already full.
    ///
    /// **Council-adjudicated fix (updating this comment to match reality,
    /// not the original design intent)**: the capacity here is
    /// belt-and-suspenders, not the load-bearing bound it once was.
    /// `ResumeStealBridge::request_forced_detach` (the sole production
    /// sender) uses `try_send`, not a blocking `send().await` — a full or
    /// closed channel answers `NotLiveLocally` immediately rather than
    /// waiting for capacity that may never free up. This is belt-and
    /// -suspenders alongside (not a replacement for) the
    /// `RelayResumeSteal` handler's own delegated-reply fix (`relay.rs`):
    /// that fix means a wedged force-detach wait no longer blocks this
    /// node's relay mailbox at all, so even a hypothetical blocking send
    /// here would no longer wedge *other* relay traffic the way the
    /// original design worried about — but a blocking send could still
    /// hang the individual ask past its own caller-observed budget, which
    /// `try_send` avoids categorically. Whichever request the
    /// connection never gets around to reading is caught by that request's
    /// own asker-side ack timeout instead (a bounded, not indefinite, wait).
    force_detach_tx: mpsc::Sender<ForceDetachRequest>,
    force_detach_rx: Arc<std::sync::Mutex<Option<mpsc::Receiver<ForceDetachRequest>>>>,
}

/// See [`ConnectionEntry::force_detach_tx`]'s doc comment for why this is
/// not 1.
const FORCE_DETACH_CHANNEL_CAPACITY: usize = 8;

impl ConnectionEntry {
    /// Create a new connection entry with carbons disabled by default.
    pub fn new(sender: mpsc::Sender<OutboundStanza>) -> Self {
        let (force_detach_tx, force_detach_rx) = mpsc::channel(FORCE_DETACH_CHANNEL_CAPACITY);
        Self {
            sender,
            carbons_enabled: Arc::new(AtomicBool::new(false)),
            presence_available: Arc::new(AtomicBool::new(false)),
            presence_priority: Arc::new(std::sync::atomic::AtomicI8::new(0)),
            roster_interested: Arc::new(AtomicBool::new(false)),
            blocklist_interested: Arc::new(AtomicBool::new(false)),
            offline_flushed: Arc::new(AtomicBool::new(false)),
            pending_subscribes_flushed: Arc::new(AtomicBool::new(false)),
            sm_stream_id: Arc::new(std::sync::Mutex::new(None)),
            force_detach_tx,
            force_detach_rx: Arc::new(std::sync::Mutex::new(Some(force_detach_rx))),
        }
    }

    /// Clone of this entry's force-detach sender, for a caller (the
    /// cross-node resume bridge) that has looked up this entry by JID/
    /// stream-id and wants to ask its owning connection task to force
    /// -detach.
    pub fn force_detach_sender(&self) -> mpsc::Sender<ForceDetachRequest> {
        self.force_detach_tx.clone()
    }

    /// Take this connection's force-detach receiver, wiring it into the
    /// owning connection's own select loop. Returns `Some` exactly once per
    /// connection — every clone of this entry (e.g. via [`super::ConnectionRegistry::get_entry`])
    /// shares the same underlying take-once slot, so only the connection
    /// task that actually registered this entry (immediately after
    /// registration — see `server::routes::websocket::registration`) ever
    /// receives `Some`; any later/racing caller observes `None`.
    pub fn take_force_detach_rx(&self) -> Option<mpsc::Receiver<ForceDetachRequest>> {
        self.force_detach_rx.lock().ok().and_then(|mut g| g.take())
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

    /// Atomically check-and-set the pending-inbound-subscribes flag.
    /// Returns `true` exactly once per connection (the first call),
    /// `false` on every subsequent call. RFC 6121 §3.1.3: the queued
    /// inbound subscription requests are delivered on the session's
    /// initial available presence only (issue #1104).
    pub fn claim_pending_subscribes_flush(&self) -> bool {
        self.pending_subscribes_flushed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Re-open the offline-flush CAS after a flush that deferred rows
    /// on a transient MAM failure (issue #1122 follow-up). Without
    /// this, [`Self::claim_offline_flush`] — once per connection —
    /// would strand the released `pending_delivery` rows until a full
    /// reconnect, potentially forever on a long-lived session. The
    /// client's next presence update then re-claims and re-attempts
    /// the flush, naturally rate-limited by presence traffic. Runs on
    /// the same connection task that claimed the flush, so there is
    /// no concurrent claimant to race with.
    pub fn reset_offline_flush(&self) {
        self.offline_flushed.store(false, Ordering::Release);
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
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
