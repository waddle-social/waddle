use super::endpoint::{BindingOutboundEndpoint, SocketOutboundEndpoint};
use super::{mpsc, Arc, Ordering, Stanza};
use crate::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use crate::pending_delivery::PendingRowId;
use crate::pending_delivery::SmSessionId;
use crate::stream_management::SmRelayedAppendObligation;
use std::sync::atomic::AtomicBool;
use tokio_util::sync::CancellationToken;

/// Pending-flush wakeups collected during a failed resume. The owner must
/// outlive the bind and publication guards: dropping it invokes task wakers.
#[derive(Default)]
pub struct DeferredPendingFlushWakeups {
    signals: Vec<CancellationToken>,
}

impl Drop for DeferredPendingFlushWakeups {
    fn drop(&mut self) {
        for signal in self.signals.drain(..) {
            signal.cancel();
        }
    }
}

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
/// One-shot notification that the destination connection has accepted an
/// outbound frame into its write/recovery path.  It intentionally travels
/// with the frame rather than the registry send result: enqueueing in the
/// registry mpsc is not a durable delivery handoff.
#[derive(Clone)]
pub struct OutboundWriteAcceptance(
    std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
);

impl std::fmt::Debug for OutboundWriteAcceptance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OutboundWriteAcceptance(..)")
    }
}

impl OutboundWriteAcceptance {
    pub fn new() -> (Self, tokio::sync::oneshot::Receiver<()>) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        (
            Self(std::sync::Arc::new(std::sync::Mutex::new(Some(sender)))),
            receiver,
        )
    }

    /// Shared acceptance handles may acknowledge only once.
    pub fn acknowledge(&self) {
        if let Ok(mut sender) = self.0.lock() {
            if let Some(sender) = sender.take() {
                let _ = sender.send(());
            }
        }
    }
}

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
    pub pending_row_id: Option<PendingRowId>,
    /// Original receipt time for pending replay or a transient relayed copy.
    /// This timestamp carries no pending-row identity or ingress append authority.
    pub original_receipt_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional acknowledgement for a durable producer waiting until this
    /// frame reaches the connection's write/recovery-owning path.
    pub write_acceptance: Option<OutboundWriteAcceptance>,
    /// The ingress obligation for a local or relayed frame, unverified (#1789, #1805).
    /// The live handler retains it on the unacknowledged SM entry; detach authorizes
    /// it before proving the XEP-0198 replay append. Live writes do not read canonical state.
    pub ingress_append: Option<SmRelayedAppendObligation>,
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
            original_receipt_at: None,
            write_acceptance: None,
            ingress_append: None,
        }
    }

    /// Attach the delivery origin's ingress obligation.
    pub fn with_ingress_append(mut self, obligation: SmRelayedAppendObligation) -> Self {
        self.ingress_append = Some(obligation);
        self
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
            original_receipt_at: None,
            write_acceptance: None,
            ingress_append: None,
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
        row_id: PendingRowId,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            stanza,
            kind: DeliveryKind::DirectFrame,
            pending_row_id: Some(row_id),
            original_receipt_at: Some(original_receipt_at),
            write_acceptance: None,
            ingress_append: None,
        }
    }

    /// Attach a write-path acknowledgement to a server-generated direct
    /// frame.  The receiver returned by [`OutboundWriteAcceptance::new`] is
    /// resolved by the destination connection, never by registry enqueue.
    pub fn with_write_acceptance(stanza: Stanza, acceptance: OutboundWriteAcceptance) -> Self {
        Self {
            stanza,
            kind: DeliveryKind::DirectFrame,
            pending_row_id: None,
            original_receipt_at: None,
            write_acceptance: Some(acceptance),
            ingress_append: None,
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
    /// A fresh bind must terminally retire this occupancy before replacing it.
    FreshBindReplacement,
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
    /// Exact local binding targeted by an interrupting request. A request
    /// queued before rollback may reach the receiver after that binding has
    /// retired; the connection owner must reject it before force cleanup.
    pub target_binding: Option<BindingRef>,
    /// Reply for this request. An unanswered or dropped sender is
    /// inconclusive; only [`ForceDetachOutcome::Detached`] proves the
    /// force-detach completion required by a resumer.
    pub ack: tokio::sync::oneshot::Sender<ForceDetachOutcome>,
}

impl ForceDetachRequest {
    /// An unstamped request retains its legacy semantics. A stamped request
    /// may act only on the exact binding captured when it was queued.
    pub fn targets_binding(&self, current: Option<&BindingRef>) -> bool {
        self.target_binding
            .as_ref()
            .is_none_or(|target| Some(target) == current)
    }
}

/// Outcome of a [`ForceDetachRequest`], reported back to the asker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ForceDetachOutcome {
    /// Identity matched, the connection followed the force-detach path (with
    /// a bounded conflict-close attempt where possible), and XEP-0198 cleanup
    /// persisted a resumable snapshot (council-adjudicated fix: this is the ONLY
    /// variant that authorizes the asker to proceed with
    /// `steal_for_resume` — see [`Self::NotPersisted`] for every other
    /// outcome of that same cleanup pass).
    Detached,
    /// The requester's bare JID did not match this connection's own bound
    /// JID — the destructive close was refused.
    IdentityMismatch,
    /// This request did not obtain proof of a completed force-detach and a
    /// persisted resumable snapshot. Cleanup may have failed, or an ordinary
    /// shutdown may have already selected its own cleanup before this request
    /// was observed. This outcome does not prove that no snapshot exists:
    /// callers must re-check persistence and retry rather than treating it as
    /// [`Self::Detached`].
    NotPersisted,
}

/// Whether an accepted interrupting control request also stopped this socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceDetachInterruption {
    Stopped,
    /// The binding retired after enqueue. The request remains accepted and
    /// its acknowledgement is the only possible evidence of cleanup.
    QueuedAfterRetirement,
}

/// A control request was not enqueued, retaining the original request.
#[derive(Debug)]
pub enum ForceDetachOfferError {
    NoLocalBinding(Box<ForceDetachRequest>),
    Full(Box<ForceDetachRequest>),
    Closed(Box<ForceDetachRequest>),
}

/// Node-local identity of one published resource binding. This value is
/// equality evidence, not a node lease or durable endpoint authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalConnectionBindingId(uuid::Uuid);

impl LocalConnectionBindingId {
    pub fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Connection state stored in the registry.
///
/// Contains the outbound sender and shared state that can be queried
/// by the registry (like carbons_enabled status for XEP-0280).
#[derive(Debug, Clone)]
pub struct ConnectionEntry {
    binding_id: LocalConnectionBindingId,
    connection_ref: ConnectionRef,
    // The current v4 remote registration carries only the physical source.
    // A mirror must not fabricate a source binding attempt from its own ID.
    binding_ref: Option<BindingRef>,
    pub(super) binding_permit: Arc<super::binding_permit::BindingPermitState>,
    // Present only when supplied by this node's physical transport owner.
    local_io_relinquishment: Option<super::LocalIoRelinquishment>,
    pub(super) archive_dispatch:
        Arc<std::sync::Mutex<super::archive_dispatch::ArchiveDispatchFrontiers>>,
    hosting: ConnectionHosting,
    resume_control: Arc<std::sync::OnceLock<Arc<super::LiveResumeControl>>>,
    /// Bounds terminal ordering retry tasks to one per connection generation.
    terminal_ordering_retry: Arc<AtomicBool>,
    /// Opaque per-socket sink shared by every registry and UserActor clone.
    /// The raw Tokio sender is created and retained inside this endpoint.
    pub(super) endpoint: BindingOutboundEndpoint,
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
    pub sm_stream_id: Arc<std::sync::Mutex<Option<SmSessionId>>>,
    /// The connection's MUC occupancy generation (#1703), published
    /// owner-gated at registration so FullJID-keyed reconcilers can tell a
    /// live connection's memberships from a displaced generation's.
    pub occupancy_session:
        Arc<std::sync::Mutex<Option<waddle_xmpp_core::OccupancySessionGeneration>>>,
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
    /// `ResumeStealBridge::request_forced_detach` uses `try_send`, not a
    /// blocking `send().await` — a full or
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
    /// Per-generation socket I/O interruption, shared only with its owning
    /// transport. Cancel after successfully queueing an identity-verified
    /// force detach so blocked writes yield to connection-owned cleanup.
    pub write_interruption: super::SocketWriteInterruption,
}

/// See [`ConnectionEntry::force_detach_tx`]'s doc comment for why this is
/// not 1.
const FORCE_DETACH_CHANNEL_CAPACITY: usize = 8;

impl ConnectionEntry {
    /// Create a new connection entry with carbons disabled by default.
    pub fn new(endpoint: impl Into<SocketOutboundEndpoint>) -> Self {
        Self::new_for_connection(
            endpoint,
            ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        )
    }

    /// Create a new local registry entry for an already-identified physical
    /// connection. Each call still mints a new JID-binding token.
    pub fn new_for_connection(
        endpoint: impl Into<SocketOutboundEndpoint>,
        connection_ref: ConnectionRef,
    ) -> Self {
        Self::new_for_binding(endpoint, BindingRef::fresh(connection_ref))
    }

    /// Create a local registry entry for the exact bind or resume attempt.
    /// The independently minted local owner ID still fences this registry entry.
    pub fn new_for_binding(
        endpoint: impl Into<SocketOutboundEndpoint>,
        binding_ref: BindingRef,
    ) -> Self {
        Self::new_entry(
            endpoint.into(),
            binding_ref.connection_ref().clone(),
            Some(binding_ref),
            super::BindingAdmissionState::Active,
        )
    }

    /// Associate this local binding with its physical socket's observer. The
    /// caller must supply the observer created with this binding's transport;
    /// every later binding on that socket must retain the same observer.
    pub fn new_for_binding_with_local_io(
        endpoint: impl Into<SocketOutboundEndpoint>,
        binding_ref: BindingRef,
        witness: super::LocalIoRelinquishment,
    ) -> Self {
        let mut entry = Self::new_for_binding(endpoint, binding_ref);
        entry.local_io_relinquishment = Some(witness);
        entry
    }

    /// Create a non-admitting local binding candidate for the dark P3.1
    /// admission transition. The returned type cannot be passed to registry
    /// or UserActor publication APIs; exact activation converts it into a
    /// [`ConnectionEntry`]. Production registration still uses
    /// [`Self::new_for_binding`].
    pub fn new_provisional_for_binding(
        endpoint: impl Into<SocketOutboundEndpoint>,
        binding_ref: BindingRef,
    ) -> ProvisionalConnectionEntry {
        ProvisionalConnectionEntry {
            entry: Self::new_entry(
                endpoint.into(),
                binding_ref.connection_ref().clone(),
                Some(binding_ref),
                super::BindingAdmissionState::Provisional,
            ),
        }
    }

    /// Actor-owned publication requires an opaque channel and binds the full
    /// JID before either registry map can see the candidate.
    pub fn new_actor_provisional_for_binding(
        endpoint: super::ActorSocketOutboundEndpoint,
        binding_ref: BindingRef,
        jid: jid::FullJid,
    ) -> Option<ActorProvisionalConnectionEntry> {
        let raw = endpoint.into_inner();
        if raw
            .actor_custody()
            .is_none_or(|custody| custody.connection() != binding_ref.connection_ref())
        {
            return None;
        }
        Some(ActorProvisionalConnectionEntry {
            jid,
            entry: Self::new_entry(
                raw,
                binding_ref.connection_ref().clone(),
                Some(binding_ref),
                super::BindingAdmissionState::Provisional,
            ),
        })
    }

    fn new_entry(
        endpoint: SocketOutboundEndpoint,
        connection_ref: ConnectionRef,
        binding_ref: Option<BindingRef>,
        admission_state: super::BindingAdmissionState,
    ) -> Self {
        let (force_detach_tx, force_detach_rx) = mpsc::channel(FORCE_DETACH_CHANNEL_CAPACITY);
        let binding_id = LocalConnectionBindingId::fresh();
        let endpoint = endpoint.bind(binding_id, binding_ref.clone(), admission_state);
        let write_interruption = super::SocketWriteInterruption::new();
        if let Some(binding) = &binding_ref {
            let _ = write_interruption.publish_binding(binding, || ());
        }
        Self {
            binding_id,
            connection_ref,
            binding_ref,
            binding_permit: Arc::new(Default::default()),
            local_io_relinquishment: None,
            hosting: ConnectionHosting::Local,
            resume_control: Arc::new(std::sync::OnceLock::new()),
            terminal_ordering_retry: Arc::new(AtomicBool::new(false)),
            archive_dispatch: Arc::new(std::sync::Mutex::new(Default::default())),
            endpoint,
            carbons_enabled: Arc::new(AtomicBool::new(false)),
            presence_available: Arc::new(AtomicBool::new(false)),
            presence_priority: Arc::new(std::sync::atomic::AtomicI8::new(0)),
            roster_interested: Arc::new(AtomicBool::new(false)),
            blocklist_interested: Arc::new(AtomicBool::new(false)),
            offline_flushed: Arc::new(AtomicBool::new(false)),
            pending_subscribes_flushed: Arc::new(AtomicBool::new(false)),
            sm_stream_id: Arc::new(std::sync::Mutex::new(None)),
            occupancy_session: Arc::new(std::sync::Mutex::new(None)),
            force_detach_tx,
            force_detach_rx: Arc::new(std::sync::Mutex::new(Some(force_detach_rx))),
            write_interruption,
        }
    }

    /// Wire the owner-serviced control before publishing this entry.
    pub fn wire_resume_control(
        &self,
        owner: jid::FullJid,
        control: Arc<super::LiveResumeControl>,
    ) -> bool {
        if !control.bind(
            owner,
            Arc::clone(&self.sm_stream_id),
            self.binding_ref.clone(),
            self.force_detach_tx.clone(),
        ) {
            return false;
        }
        self.resume_control.set(control).is_ok()
    }

    pub fn resume_control(&self) -> Option<Arc<super::LiveResumeControl>> {
        self.resume_control.get().cloned()
    }

    pub fn remote_hosted(endpoint: impl Into<SocketOutboundEndpoint>) -> Self {
        Self::remote_hosted_for_connection(
            endpoint,
            ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        )
    }

    /// Create a remote-hosted account mirror which retains the originating
    /// physical connection reference while minting a mirror-local binding.
    pub fn remote_hosted_for_connection(
        endpoint: impl Into<SocketOutboundEndpoint>,
        connection_ref: ConnectionRef,
    ) -> Self {
        let mut entry = Self::new_entry(
            endpoint.into(),
            connection_ref,
            None,
            super::BindingAdmissionState::Active,
        );
        entry.hosting = ConnectionHosting::Remote;
        entry
    }

    /// Atomically activate this exact local binding candidate. A sealed
    /// binding cannot be reactivated. Only the provisional wrapper exposes
    /// the resulting entry to publication APIs.
    #[cfg(any(test, feature = "test-utils"))]
    pub(super) fn activate_binding(
        &self,
        owner: LocalConnectionBindingId,
        binding_ref: &BindingRef,
    ) -> bool {
        self.endpoint.activate(owner, binding_ref)
    }

    /// Atomically seal this exact local binding against future queue
    /// acceptance. Enqueue and reserved-permit commits hold the same admission
    /// lock, so a commit is ordered wholly before or after this transition.
    /// Returns false for an owner or binding-attempt mismatch. A successful
    /// seal also revokes the exact local pre-write permit.
    pub fn seal_binding(&self, owner: LocalConnectionBindingId, binding_ref: &BindingRef) -> bool {
        let sealed = self
            .binding_permit
            .revoke_on_success(|| self.endpoint.seal(owner, binding_ref).then_some(()))
            .is_some();
        if sealed {
            self.binding_permit.notify_revoked();
        }
        sealed
    }

    /// Seal the failed resume's exact binding and retain its pending-flush
    /// wakeup until the caller has released its outer registration guards.
    /// The existing pre-write permit notification retains its normal timing.
    pub fn seal_binding_for_failed_resume(
        &self,
        owner: LocalConnectionBindingId,
        binding_ref: &BindingRef,
        wakeups: &mut DeferredPendingFlushWakeups,
    ) -> bool {
        if !self.seal_binding(owner, binding_ref) {
            return false;
        }
        wakeups
            .signals
            .push(self.endpoint.pending_flush_seal_signal());
        true
    }

    /// Legacy replacements are unchanged. Every path that removes an actor
    /// candidate must close its captured admission handles first.
    pub(in crate::registry) fn seal_actor_binding_if_any(&self) {
        if let Some(binding_ref) = self.binding_ref.as_ref() {
            self.endpoint.seal_if_actor(self.binding_id, binding_ref);
        }
    }

    pub(in crate::registry) fn same_exact_local_binding(&self, other: &Self) -> bool {
        self.binding_id == other.binding_id
            && self.connection_ref == other.connection_ref
            && self.binding_ref == other.binding_ref
            && Arc::ptr_eq(&self.binding_permit, &other.binding_permit)
            && self.endpoint.same_binding(&other.endpoint)
    }

    pub(in crate::registry) fn actor_custody(&self) -> Option<Arc<super::ActorOutboundCustody>> {
        self.endpoint.actor_custody()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(in crate::registry) fn is_provisional(&self) -> bool {
        self.endpoint.is_provisional()
    }

    /// Recover this local socket's original outbound queue after its exact
    /// binding has been sealed, but only when no accepted frames or reserved
    /// permits remain. The returned endpoint shares the same queue and is
    /// intended for a new binding attempt on the same physical socket.
    pub fn into_socket_outbound_endpoint_if_empty(
        self,
        owner: LocalConnectionBindingId,
        binding_ref: &BindingRef,
    ) -> Option<SocketOutboundEndpoint> {
        if self.binding_id != owner || self.binding_ref.as_ref() != Some(binding_ref) {
            return None;
        }
        let Self { endpoint, .. } = self;
        endpoint
            .into_socket_endpoint_if_empty(owner, binding_ref, None)
            .ok()
    }

    /// Recover an exact sealed binding when its paired socket receiver is
    /// empty. Reservations on the old binding remain sealed and cannot commit.
    /// The socket owner must not have an outbound handler in flight.
    pub fn into_socket_outbound_endpoint_if_receiver_empty(
        self,
        owner: LocalConnectionBindingId,
        binding_ref: &BindingRef,
        receiver: &super::SocketOutboundReceiver,
    ) -> Option<SocketOutboundEndpoint> {
        if self.binding_id != owner || self.binding_ref.as_ref() != Some(binding_ref) {
            return None;
        }
        let Self { endpoint, .. } = self;
        endpoint
            .into_socket_endpoint_if_empty(owner, binding_ref, Some(receiver))
            .ok()
    }

    /// Immutable physical transport identity carried by this registry entry.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// This entry's source binding attempt when locally hosted. The current
    /// remote registration protocol carries only the source transport, so a
    /// remote-hosted mirror returns `None` instead of minting a false attempt.
    pub fn binding_ref(&self) -> Option<&BindingRef> {
        self.binding_ref.as_ref()
    }

    /// Local I/O evidence, when supplied by the actual socket owner. Remote
    /// mirrors and entries constructed without that owner have no witness.
    pub fn local_io_relinquishment(&self) -> Option<&super::LocalIoRelinquishment> {
        self.local_io_relinquishment.as_ref()
    }

    /// Read-only observation for the WebSocket queue characterization seam.
    #[cfg(feature = "test-utils")]
    pub fn outbound_queue_is_empty_for_test(&self) -> bool {
        self.endpoint.capacity() == self.endpoint.max_capacity()
    }

    /// Read-only observation for the WebSocket queue characterization seam.
    #[cfg(feature = "test-utils")]
    pub fn outbound_queue_is_closed_for_test(&self) -> bool {
        self.endpoint.is_closed()
    }

    pub fn is_locally_hosted(&self) -> bool {
        self.hosting == ConnectionHosting::Local
    }

    /// Acquire the sole terminal ordering retry slot for this connection.
    /// This bounds sleeping retry tasks; durable claim fencing still decides
    /// which task may deliver. Dropping the lease reopens the slot, including
    /// when its owning task is cancelled.
    pub fn try_acquire_terminal_ordering_retry(&self) -> Option<TerminalOrderingRetryLease> {
        self.terminal_ordering_retry
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| TerminalOrderingRetryLease(Arc::clone(&self.terminal_ordering_retry)))
    }

    /// Clone of this entry's force-detach sender, for a caller (the
    /// cross-node resume bridge) that has looked up this entry by JID/
    /// stream-id and wants to ask its owning connection task to force
    /// -detach.
    pub fn force_detach_sender(&self) -> mpsc::Sender<ForceDetachRequest> {
        self.force_detach_tx.clone()
    }

    /// Preserve ordinary queue and acknowledgement behavior, but when the
    /// socket's own control proves this exact local JID/binding, serialize a
    /// successful queue reservation with its pre-write permit revocation.
    /// Unverified or remote entries use the existing raw offer unchanged.
    pub fn try_force_detach_for_jid(
        &self,
        jid: &jid::FullJid,
        request: ForceDetachRequest,
    ) -> Result<(), ForceDetachOfferError> {
        let Some(binding) = self.binding_ref.as_ref() else {
            return self.try_force_detach_raw(request);
        };
        if !self.is_locally_hosted()
            || request.requester_bare_jid != jid.to_bare()
            || request.target_binding.as_ref() != Some(binding)
        {
            return self.try_force_detach_raw(request);
        }
        let Some(control) = self.resume_control.get() else {
            return self.try_force_detach_raw(request);
        };
        let Some(reservation) = control.with_exact_binding(jid, binding, || {
            self.binding_permit
                .revoke_on_ok(|| self.force_detach_tx.try_reserve())
        }) else {
            return self.try_force_detach_raw(request);
        };
        let reservation = match reservation {
            Ok(reservation) => reservation,
            Err(mpsc::error::TrySendError::Full(())) => {
                return Err(ForceDetachOfferError::Full(Box::new(request)));
            }
            Err(mpsc::error::TrySendError::Closed(())) => {
                return Err(ForceDetachOfferError::Closed(Box::new(request)));
            }
        };
        self.binding_permit.notify_revoked();
        reservation.send(request);
        Ok(())
    }

    fn try_force_detach_raw(
        &self,
        request: ForceDetachRequest,
    ) -> Result<(), ForceDetachOfferError> {
        self.force_detach_tx
            .try_send(request)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(request) => {
                    ForceDetachOfferError::Full(Box::new(request))
                }
                mpsc::error::TrySendError::Closed(request) => {
                    ForceDetachOfferError::Closed(Box::new(request))
                }
            })
    }

    /// Queue retirement before interrupting this captured connection's I/O.
    /// The caller must first verify requester identity, ownership, and any
    /// applicable stream or occupancy generation. Cleanup and acknowledgement
    /// remain the owning task's responsibility. A failed enqueue leaves I/O
    /// uninterrupted; callers that must preserve I/O use the ordinary sender.
    /// A successful offer remains accepted even if the binding retires before
    /// the stop transition. In that case the request is stamped with the old
    /// binding and must not interrupt or force cleanup of its successor.
    pub fn try_force_detach_and_interrupt(
        &self,
        request: ForceDetachRequest,
    ) -> Result<ForceDetachInterruption, ForceDetachOfferError> {
        self.try_force_detach_and_interrupt_after_offer(request, || {})
    }

    fn try_force_detach_and_interrupt_after_offer(
        &self,
        request: ForceDetachRequest,
        after_offer: impl FnOnce(),
    ) -> Result<ForceDetachInterruption, ForceDetachOfferError> {
        self.try_force_detach_and_revoke(request)?;
        after_offer();
        // The successful offer above proves a local binding was present.
        let Some(binding) = self.binding_ref.as_ref() else {
            unreachable!("successful local detach offer requires a binding");
        };
        Ok(if self.write_interruption.cancel_if_binding(binding) {
            ForceDetachInterruption::Stopped
        } else {
            ForceDetachInterruption::QueuedAfterRetirement
        })
    }

    /// Reserve a local detach and revoke its pre-write permit atomically.
    /// A full or closed queue does not prove detachment. A receiver closing
    /// after reservation still leaves an accepted but unconfirmed request;
    /// callers must observe lifecycle shutdown separately.
    pub fn try_force_detach_and_revoke(
        &self,
        request: ForceDetachRequest,
    ) -> Result<(), ForceDetachOfferError> {
        self.try_force_detach_and_revoke_after_reserve(request, || {})
    }

    fn try_force_detach_and_revoke_after_reserve(
        &self,
        mut request: ForceDetachRequest,
        after_reserve: impl FnOnce(),
    ) -> Result<(), ForceDetachOfferError> {
        let Some(binding) = self.binding_ref.as_ref() else {
            return Err(ForceDetachOfferError::NoLocalBinding(Box::new(request)));
        };
        request.target_binding = Some(binding.clone());
        // try_reserve performs only the semaphore CAS, without waking the
        // receiver. A successful reservation is the queue-offer decision;
        // send and its potentially reentrant wake run after the cell unlocks.
        let reservation = match self
            .binding_permit
            .revoke_on_ok(|| self.force_detach_tx.try_reserve())
        {
            Ok(reservation) => reservation,
            Err(mpsc::error::TrySendError::Full(())) => {
                return Err(ForceDetachOfferError::Full(Box::new(request)));
            }
            Err(mpsc::error::TrySendError::Closed(())) => {
                return Err(ForceDetachOfferError::Closed(Box::new(request)));
            }
        };
        // Wake parked writers before delivering the control message. Both
        // notification and the receiver wake run without the permit cell.
        self.binding_permit.notify_revoked();
        after_reserve();
        reservation.send(request);
        Ok(())
    }

    /// Test receiver closure between successful reservation and delivery.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn try_force_detach_and_revoke_after_reserve_for_test(
        &self,
        request: ForceDetachRequest,
        after_reserve: impl FnOnce(),
    ) -> Result<(), ForceDetachOfferError> {
        self.try_force_detach_and_revoke_after_reserve(request, after_reserve)
    }

    /// Pause the real interrupting path after its offer but before its stop.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn try_force_detach_and_interrupt_after_offer_for_test(
        &self,
        request: ForceDetachRequest,
        after_offer: impl FnOnce(),
    ) -> Result<ForceDetachInterruption, ForceDetachOfferError> {
        self.try_force_detach_and_interrupt_after_offer(request, after_offer)
    }

    /// Take this connection's force-detach receiver, wiring it into the
    /// owning connection's own select loop. Returns `Some` exactly once per
    /// connection — every clone of this entry (e.g. via [`super::ConnectionRegistry::entry`])
    /// shares the same underlying take-once slot, so only the connection
    /// task that actually registered this entry (immediately after
    /// registration — see `server::routes::websocket::registration`) ever
    /// receives `Some`; any later/racing caller observes `None`.
    pub fn take_force_detach_rx(&self) -> Option<mpsc::Receiver<ForceDetachRequest>> {
        self.force_detach_rx.lock().ok().and_then(|mut g| g.take())
    }

    /// Return this entry's stable local binding identity. Re-publishing a
    /// clone retains the identity; a new entry gets a new one.
    pub fn binding_id(&self) -> LocalConnectionBindingId {
        self.binding_id
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
    pub fn occupancy_session(&self) -> Option<waddle_xmpp_core::OccupancySessionGeneration> {
        self.occupancy_session.lock().ok().and_then(|guard| *guard)
    }

    pub fn set_sm_stream_id(&self, session_id: Option<SmSessionId>) {
        if let Ok(mut guard) = self.sm_stream_id.lock() {
            *guard = session_id;
        }
    }

    /// Read the XEP-0198 SM session id for this connection, if SM is
    /// enabled. Returns `None` while SM is disabled (no
    /// `pending_delivery` row will be claimed under an SM session id
    /// in that case — the flush falls back to the delete-on-push
    /// path).
    pub fn sm_stream_id(&self) -> Option<SmSessionId> {
        self.sm_stream_id
            .lock()
            .ok()
            .and_then(|g| g.as_ref().cloned())
    }
}

/// A local binding candidate that is not accepted by registry or actor
/// publication APIs. It can become a [`ConnectionEntry`] only by activating
/// the exact local owner and binding attempt that it carries.
///
/// ```compile_fail
/// use jid::FullJid;
/// use waddle_xmpp::registry::{ConnectionRegistry, ProvisionalConnectionEntry};
///
/// fn publish(
///     registry: &ConnectionRegistry,
///     jid: FullJid,
///     candidate: ProvisionalConnectionEntry,
/// ) {
///     registry.register_entry(jid, candidate);
/// }
/// ```
///
/// UserActor publication also accepts only [`ConnectionEntry`]:
///
/// ```compile_fail
/// use jid::FullJid;
/// use waddle_xmpp::registry::{user_actor::RegisterConnection, ProvisionalConnectionEntry};
///
/// fn publish_to_actor(jid: FullJid, candidate: ProvisionalConnectionEntry) {
///     let _ = RegisterConnection { jid, entry: candidate };
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ProvisionalConnectionEntry {
    entry: ConnectionEntry,
}

#[derive(Debug, Clone)]
pub struct ActorProvisionalConnectionEntry {
    jid: jid::FullJid,
    entry: ConnectionEntry,
}

impl ActorProvisionalConnectionEntry {
    pub fn jid(&self) -> &jid::FullJid {
        &self.jid
    }

    pub fn seal_binding(&self) -> bool {
        self.entry
            .binding_ref()
            .is_some_and(|binding| self.entry.seal_binding(self.entry.binding_id(), binding))
    }

    pub fn connection_ref(&self) -> &ConnectionRef {
        self.entry.connection_ref()
    }

    pub fn binding_ref(&self) -> Option<&BindingRef> {
        self.entry.binding_ref()
    }

    pub fn binding_id(&self) -> LocalConnectionBindingId {
        self.entry.binding_id()
    }

    pub fn actor_custody(&self) -> Option<Arc<super::ActorOutboundCustody>> {
        self.entry.actor_custody()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(in crate::registry) fn entry_for_publication(&self) -> ConnectionEntry {
        self.entry.clone()
    }

    pub(in crate::registry) fn matches_entry(&self, entry: &ConnectionEntry) -> bool {
        self.entry.same_exact_local_binding(entry)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(in crate::registry) fn is_provisional(&self) -> bool {
        self.entry.is_provisional()
    }
}

impl ProvisionalConnectionEntry {
    /// Return this candidate's stable local owner identity.
    pub fn binding_id(&self) -> LocalConnectionBindingId {
        self.entry.binding_id()
    }

    /// Activate the exact local owner and binding attempt, yielding the
    /// registerable entry only on success. A mismatch or sealed candidate is
    /// discarded and cannot be retried through this API.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn activate_binding(
        self,
        owner: LocalConnectionBindingId,
        binding_ref: &BindingRef,
    ) -> Option<ConnectionEntry> {
        if self.entry.activate_binding(owner, binding_ref) {
            Some(self.entry)
        } else {
            None
        }
    }
}

/// Exclusive ownership of a connection's terminal ordering retry task slot.
#[derive(Debug)]
#[must_use = "hold the lease for the lifetime of the terminal ordering retry task"]
pub struct TerminalOrderingRetryLease(Arc<AtomicBool>);

impl Drop for TerminalOrderingRetryLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionHosting {
    Local,
    Remote,
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

#[cfg(test)]
mod tests {
    use super::super::endpoint::{EndpointEnqueueOutcome, EndpointEnqueueRejection};
    use super::{ConnectionEntry, OutboundStanza, SocketOutboundEndpoint};
    use crate::ingress::{IngressEffectKind, MessageKey};
    use crate::mam::ArchiveOrdinal;
    use crate::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
    use crate::stream_management::{
        ArchiveDispatchPosition, SmIngressAppendKey, SmIngressReceiptKind,
        SmRelayedAppendObligation,
    };
    use crate::Stanza;
    use std::sync::atomic::Ordering;

    #[test]
    fn sealed_candidate_cannot_activate_or_enqueue() {
        let (endpoint, mut receiver) = SocketOutboundEndpoint::channel(1);
        let binding_ref =
            BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
        let candidate = ConnectionEntry::new_provisional_for_binding(endpoint, binding_ref.clone());
        let owner = candidate.binding_id();

        assert!(candidate.entry.seal_binding(owner, &binding_ref));
        assert!(matches!(
            candidate
                .entry
                .endpoint
                .try_send(OutboundStanza::new(Stanza::Message(
                    xmpp_parsers::message::Message::new(None),
                ))),
            EndpointEnqueueOutcome::ProvenNotEnqueued {
                reason: EndpointEnqueueRejection::Sealed,
                ..
            }
        ));
        assert!(candidate.activate_binding(owner, &binding_ref).is_none());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn carbon_opt_out_requires_the_exact_binding_to_be_active() {
        let resource: jid::FullJid = "bob@example.test/phone".parse().expect("resource");
        for kind in [IngressEffectKind::Carbons, IngressEffectKind::RelayCarbons] {
            for archived in [false, true] {
                let (endpoint, mut receiver) = SocketOutboundEndpoint::channel(1);
                let binding_ref =
                    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
                let candidate =
                    ConnectionEntry::new_provisional_for_binding(endpoint, binding_ref.clone());
                let owner = candidate.binding_id();
                let outbound = OutboundStanza::new(Stanza::Message(
                    xmpp_parsers::message::Message::new(Some(resource.clone().into())),
                ))
                .with_ingress_append(SmRelayedAppendObligation {
                    key: SmIngressAppendKey {
                        message_key: MessageKey::new(),
                        kind: SmIngressReceiptKind::from_storage(kind.storage_tag()),
                        semantic_identity_hash: [1; 32],
                        resource: resource.clone(),
                    },
                    sender_bare: "alice@example.test".parse().expect("sender"),
                    received_at: None,
                    archive_positions: if archived {
                        vec![ArchiveDispatchPosition {
                            archive: resource.to_bare(),
                            ordinal: ArchiveOrdinal::from_storage(1).expect("ordinal"),
                        }]
                    } else {
                        Vec::new()
                    },
                    dispatch_stream: None,
                });

                assert_eq!(
                    candidate.entry.try_send_archive_ordered(outbound.clone()),
                    Err(EndpointEnqueueRejection::Provisional)
                );
                assert!(receiver.try_recv().is_err());

                let entry = candidate
                    .activate_binding(owner, &binding_ref)
                    .expect("activate exact binding");
                assert_eq!(entry.try_send_archive_ordered(outbound.clone()), Ok(()));
                assert!(receiver.try_recv().is_err(), "active opt-out suppresses");

                entry.carbons_enabled.store(true, Ordering::Relaxed);
                assert_eq!(entry.try_send_archive_ordered(outbound.clone()), Ok(()));
                receiver
                    .try_recv()
                    .expect("opt-in enqueues the same obligation");

                entry.carbons_enabled.store(false, Ordering::Relaxed);
                assert!(entry.seal_binding(owner, &binding_ref));
                assert_eq!(
                    entry.try_send_archive_ordered(outbound),
                    Err(EndpointEnqueueRejection::Sealed)
                );
                assert!(receiver.try_recv().is_err());
            }
        }
    }
}
