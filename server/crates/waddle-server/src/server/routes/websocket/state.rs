use super::*;

#[derive(Debug, Clone)]
pub struct XmppServiceDomains {
    pub muc: String,
    pub spaces: String,
    pub upload: String,
    pub extensions: String,
}

impl XmppServiceDomains {
    pub fn new(xmpp_domain: &str) -> Self {
        Self {
            muc: format!("muc.{xmpp_domain}"),
            spaces: format!("spaces.{xmpp_domain}"),
            upload: format!("upload.{xmpp_domain}"),
            extensions: format!("extensions.{xmpp_domain}"),
        }
    }
}

/// WebSocket route dependencies kept narrower than the full server graph.
pub struct WebSocketState {
    pub deps: WebSocketDeps,
}

pub struct WebSocketDeps {
    /// Core app state for accessing the global and per-waddle databases.
    pub app_state: Arc<AppState>,
    /// Authentication state for session validation.
    pub auth_state: Arc<AuthState>,
    /// Authoritative XMPP component/service JIDs used by this deployment.
    pub service_domains: XmppServiceDomains,
    /// Protocol/runtime services used by the WebSocket C2S path.
    pub protocol: ProtocolServices,
    /// Per-deployment XEP-0421 occupant-id HMAC key. Cheap-clone via the
    /// inner `Arc<[u8]>`; shared with `RoomRegistryActor` so every
    /// stamping site uses the same key.
    pub occupant_id_secret: OccupantIdSecret,
    /// Snapshot of `WADDLE_PROVIDER_*_WEBHOOK_*` env vars, built once at
    /// startup; the extension webhook handler reads from this instead of
    /// re-parsing env on every request.
    pub provider_ingress: Arc<crate::server::routes::extension_webhooks::ProviderIngressRegistry>,
    /// Tracker for spawned provider webhook dispatch tasks, drained on
    /// graceful shutdown so in-flight deliveries can finish updating the
    /// ledger before runtime teardown.
    pub provider_dispatch_tasks: crate::server::routes::extension_webhooks::ProviderDispatchTracker,
}

pub struct ProtocolServices {
    /// Registry for tracking active connections by JID.
    pub connection_registry: Arc<ConnectionRegistry>,
    /// Actor-backed registry for MUC rooms.
    pub room_registry: ActorRef<RoomRegistryActor>,
    /// Shared XMPP MAM storage for archived message history.
    pub mam_storage: Arc<dyn MamStorage>,
    /// Shared Waddle inbox projection storage.
    pub inbox_storage: Arc<dyn InboxStorage>,
    /// Shared XEP-0191 blocking-list storage. Used by the headless
    /// offline-recipient pass (#229 PR15) to seed a transient
    /// recipient state machine's blocklist when delivering to a
    /// local-domain bare JID with no available resources.
    pub blocking_storage: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage>,
    /// XEP-0160 offline-message storage. Used by the
    /// [`crate::server::routes::interpret::interpret`] arm for
    /// `OutboundEvent::QueueOfflineDelivery` to persist DM stanzas
    /// during the headless recipient pass (issue #209).
    pub pending_delivery_storage:
        Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>,
    /// Registry for ad-hoc commands exposed over the WebSocket transport.
    pub command_registry: Arc<CommandRegistry>,
    /// Runtime extension manager for message embeds + feature advertisements.
    pub extension_manager: Arc<ExtensionManager>,
    /// Sans-I/O stanza dispatcher. Handlers migrated so far (ping, session,
    /// carbons) are routed through this before falling back to the
    /// legacy string-matching code paths below.
    pub dispatcher: Arc<StanzaDispatcher>,
    /// Shared PubSub/PEP storage (XEP-0060/XEP-0163).
    pub pubsub_storage: Arc<dyn PubSubStorage>,
    /// XEP-0357 push subscription storage.
    pub push_store: Arc<dyn waddle_xmpp::push::PushSubscriptionStore>,
    /// Durable derived projection of XEP-0492 notification settings from
    /// canonical XMPP state.
    pub notification_settings_projection:
        Arc<crate::notification_settings_projection::NotificationSettingsProjectionStore>,
    /// XEP-0397 Instant Stream Resumption token store.
    pub isr_token_store: waddle_xmpp::isr::SharedIsrTokenStore,
    /// XEP-0198 detached-session registry — holds state for clients whose
    /// WebSocket has closed but may still resume within the session timeout.
    pub sm_session_registry: Arc<InMemorySmSessionRegistry>,
    /// XEP-0115 entity-capabilities resolver. Maintains the
    /// process-wide hash-keyed `CapsCache` plus per-resource caps
    /// mappings used by XEP-0163 §3 fan-out filtering.
    pub caps_resolver: Arc<crate::server::caps_resolution::CapsResolver>,
    /// Per-(BareJid) mutex set guarding the `user_avatar_source`
    /// read-then-publish critical section. Both the OIDC publish
    /// chain (`profile::publish::ensure_pep_profile_published`) and
    /// the wire avatar-publish hook (`pubsub_dispatch` calling
    /// `record_self_published`) acquire the same mutex by
    /// `BareJid`, closing the TOCTOU race where OIDC could read
    /// `'oidc'` between the user's wire publish and the user's
    /// provenance flip and then wipe the just-set avatar.
    ///
    /// Entries are evicted by `AvatarLockGuard::drop` when no
    /// acquirer is in flight, so the map stays bounded by current
    /// contention rather than growing to the lifetime user set.
    pub avatar_source_locks: Arc<crate::profile::AvatarLockMap>,
    /// Tracker for the OIDC → PEP profile-publish background tasks.
    /// The auth callback registers each `tokio::spawn` here so the
    /// graceful-shutdown drain can `wait()` on in-flight publishes
    /// before tearing down the runtime — preventing the split-state
    /// where the empty `<metadata/>` is published but vcard-temp
    /// PHOTO is still set.
    pub profile_publish_tracker: tokio_util::task::TaskTracker,
    /// Sidecar map keyed by SM stream id, holding the authenticated `Session`
    /// so that a resumed stream doesn't lose its authorization context and
    /// can serve IQs that check channel membership, etc. Entries are
    /// populated on detach and removed on take/resume (or swept when the
    /// corresponding SM session expires).
    pub resumable_sessions: Arc<dashmap::DashMap<String, Session>>,
}

/// Per-connection mutable state for a single WebSocket XMPP transport.
///
/// Bundles the typed lifecycle phase and the remaining transport/session
/// adjuncts (SM state, carbons flag, suppress-SM-record flag) into a single
/// value threaded through the frame dispatcher.
pub(super) struct WsConnState {
    pub(super) phase: ConnectionPhase,
    /// The authenticated backend Session for this connection, if any.
    /// Populated on SASL success and used for SM resume/detach.
    pub(super) authenticated_session: Option<Session>,
    /// XEP-0198 state for this WebSocket. Counts stanzas in both directions
    /// once enabled and holds the unacked queue used for resumption.
    pub(super) sm_state: StreamManagementState,
    /// Per-connection XEP-0280 opt-in state. Updated when this resource
    /// sends `<enable/>` / `<disable/>` and restored from detached SM state
    /// on resume so re-registration preserves carbons behavior.
    pub(super) carbons_enabled: bool,
    /// Per-stream RFC 6121 roster-interest state restored only on true SM resume.
    pub(super) roster_interested: bool,
    /// Presence availability restored from XEP-0198 detached state.
    pub(super) presence_available: bool,
    pub(super) presence_show: Option<xmpp_parsers::presence::Show>,
    pub(super) presence_status: Option<String>,
    pub(super) presence_priority: i8,
    /// SM stream id restored by `<resume/>` but not yet removed from the
    /// detached registry. The main loop clears it after live routing is
    /// registered, closing the take-before-register fanout gap.
    pub(super) pending_resume_stream_id: Option<String>,
    pub(super) pending_resume_h: Option<u32>,
    /// Ownership handle for the current connection-registry entry.
    pub(super) registry_owner: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// One-shot flag: when set, the main loop must NOT push the current
    /// frame's responses into `sm_state.record_outbound`. The flag is
    /// raised by `handle_sm_resume` because the responses it returns are
    /// replayed stanzas that were already pushed into the unacked queue
    /// before detach; re-recording them would double-count `outbound_count`
    /// and duplicate queue entries. The main loop resets the flag to
    /// `false` after skipping the record step.
    pub(super) suppress_sm_record_next_batch: bool,
    /// Per-connection sans-I/O state machine for the message
    /// pipeline (issue #229 PR11). `None` until `transition_to_ready`
    /// is called on bind — see [`Self::ensure_state_machine`].
    ///
    /// When `Some`, the per-connection main loop dispatches
    /// [`OutboundStanza`] entries on their [`DeliveryKind`]:
    /// `PeerStanza` values feed [`InboundEvent::StanzaFromPeer`]
    /// into this state machine and the resulting outbound events are
    /// resolved via [`crate::server::routes::interpret::interpret`]
    /// before any wire write. `DirectFrame` values bypass the state
    /// machine entirely and write straight to the wire.
    ///
    /// [`InboundEvent::StanzaFromPeer`]: waddle_xmpp::protocol::InboundEvent::StanzaFromPeer
    pub(super) state_machine: Option<XmppStateMachine>,
}

impl WsConnState {
    pub(super) fn new() -> Self {
        Self {
            phase: ConnectionPhase::new(),
            authenticated_session: None,
            sm_state: StreamManagementState::new(),
            carbons_enabled: false,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            pending_resume_stream_id: None,
            pending_resume_h: None,
            registry_owner: None,
            suppress_sm_record_next_batch: false,
            state_machine: None,
        }
    }

    /// Initialize the per-connection [`XmppStateMachine`] in
    /// [`ConnectionPhase::Ready`] for `full_jid`, cloning the
    /// process-wide handler dispatcher into a per-connection copy
    /// (cheap — handlers are `Arc`-shared).
    ///
    /// Called by the bind / SM-resume transition paths so the SM is
    /// ready to handle [`InboundEvent::StanzaFromPeer`] dispatches
    /// from the outbound channel as soon as routing peers can target
    /// this connection. Idempotent — re-initialization on resume
    /// replaces the previous machine, dropping any pending callback
    /// state from before the detach (which is correct: the SM-level
    /// `pending_ops` table belongs to the prior dispatch context and
    /// the resumed wire state is replayed via XEP-0198, not via SM
    /// callback completion).
    ///
    /// `blocklist` is the bound user's persisted XEP-0191 blocklist,
    /// loaded once from `DatabaseBlockingStorage` immediately before
    /// this call; it seeds the SM's session-state snapshot consumed
    /// by the message pipeline (#229 PR13). Per #229 Q5 the snapshot
    /// is frozen for the duration of the session — see
    /// [`XmppStateMachine::set_blocklist`].
    ///
    /// [`InboundEvent::StanzaFromPeer`]: waddle_xmpp::protocol::InboundEvent::StanzaFromPeer
    pub(super) fn ensure_state_machine(
        &mut self,
        domain: &str,
        dispatcher: &Arc<StanzaDispatcher>,
        full_jid: jid::FullJid,
        resumed: bool,
        blocklist: Blocklist,
    ) {
        let mut sm = XmppStateMachine::new(domain.to_string(), (**dispatcher).clone());
        sm.transition_to_ready(full_jid, resumed);
        sm.set_blocklist(blocklist);
        self.state_machine = Some(sm);
    }

    /// Mirror the legacy [`Self::phase`] tracker's current value into
    /// the per-connection [`XmppStateMachine`]'s phase. Called after
    /// every `handle_xmpp_frame` round-trip + at start of
    /// `cleanup_connection_shutdown` so a phase transition driven by
    /// a deeply-nested helper (e.g. SASL failure or stream-error
    /// inside `handle_xmpp_frame`) doesn't leave the SM stuck in
    /// `Ready` and willing to accept late `PeerStanza` dispatches.
    ///
    /// Currently only the `Ready → Closing` direction needs explicit
    /// mirroring (the `Ready` direction is handled lazily by
    /// [`Self::ensure_state_machine`] at bind / SM-resume).
    pub(super) fn sync_state_machine_phase(&mut self) {
        if let Some(sm) = self.state_machine.as_mut() {
            if matches!(self.phase, ConnectionPhase::Closing { .. })
                && !matches!(sm.phase(), ConnectionPhase::Closing { .. })
            {
                sm.transition_to_closing();
            }
        }
    }
}
