use super::*;

#[derive(Debug, Clone)]
pub struct ActiveCallThread {
    pub anchor_origin_id: String,
    pub initiator: BareJid,
    pub media: waddle_xmpp::xep::CallThreadMedia,
    pub started: chrono::DateTime<chrono::Utc>,
    /// The anchor message's `urn:waddle:threads:0` thread id (the
    /// `<thread/>` value). Correlates the ended fastening back to the
    /// inbox/threads rows so [`InboxStorage::mark_call_thread_ended`]
    /// can stamp the ended timestamp + duration onto every
    /// subscriber's projection of `(room, thread_id)`.
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DmCallThreadKey {
    pub low_peer: BareJid,
    pub high_peer: BareJid,
    pub sid: xmpp_parsers::jingle::SessionId,
}

impl DmCallThreadKey {
    pub fn new(a: BareJid, b: BareJid, sid: xmpp_parsers::jingle::SessionId) -> Self {
        let (low_peer, high_peer) = if a.as_str() <= b.as_str() {
            (a, b)
        } else {
            (b, a)
        };
        Self {
            low_peer,
            high_peer,
            sid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingDmCallOffer {
    pub media: waddle_xmpp::xep::CallThreadMedia,
    pub initiator: BareJid,
    pub started: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DmPairKey {
    pub low_peer: BareJid,
    pub high_peer: BareJid,
}

impl DmPairKey {
    pub fn new(a: BareJid, b: BareJid) -> Self {
        let (low_peer, high_peer) = if a.as_str() <= b.as_str() {
            (a, b)
        } else {
            (b, a)
        };
        Self {
            low_peer,
            high_peer,
        }
    }

    pub fn contains(&self, jid: &BareJid) -> bool {
        self.low_peer == *jid || self.high_peer == *jid
    }
}

#[derive(Debug, Default)]
pub struct DmPinStore {
    entries: dashmap::DashMap<DmPairKey, Vec<waddle_xmpp::muc::PinnedEntry>>,
}

impl DmPinStore {
    pub fn apply_pin(&self, key: DmPairKey, entry: waddle_xmpp::muc::PinnedEntry) {
        let target_stanza_id = entry.target_stanza_id.clone();
        let mut entries = self.entries.entry(key).or_default();
        entries.retain(|existing| existing.target_stanza_id.id != target_stanza_id.id);
        entries.insert(0, entry);
        if entries.len() > waddle_xmpp::muc::pin::MAX_PINNED_ENTRIES {
            entries.pop();
        }
    }

    pub fn list(&self, key: &DmPairKey) -> Vec<waddle_xmpp::muc::PinnedEntry> {
        self.entries
            .get(key)
            .map(|entries| entries.clone())
            .unwrap_or_default()
    }

    pub fn unpin(
        &self,
        key: &DmPairKey,
        target_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    ) -> bool {
        let Some(mut entries) = self.entries.get_mut(key) else {
            return false;
        };
        let before = entries.len();
        entries.retain(|entry| entry.target_stanza_id.id != target_stanza_id.id);
        entries.len() != before
    }

    pub fn contains(
        &self,
        key: &DmPairKey,
        target_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    ) -> bool {
        self.entries
            .get(key)
            .map(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.target_stanza_id.id == target_stanza_id.id)
            })
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct XmppServiceDomains {
    pub muc: String,
    pub spaces: String,
    pub upload: String,
    pub extensions: String,
    pub push: String,
    /// Hosts community-wide pubsub features that are NOT space-bookmark
    /// nodes (XEP-0472 social feed, XEP-0501 stories). Kept distinct
    /// from `spaces` so the spaces disco#items enumeration only
    /// surfaces actual community spaces.
    pub community: String,
}

impl XmppServiceDomains {
    /// `muc` and `spaces` are the deployment-authoritative service domains —
    /// overridable via `WADDLE_MUC_DOMAIN` / `WADDLE_SPACES_JID` — and MUST be
    /// the exact values the MUC room registry and spaces component are built
    /// from. disco#info routing compares targets against `muc` (#757); a
    /// re-derived `muc.<domain>` would diverge from the registry under an
    /// override and silently break room disco#info. The remaining components
    /// have no override and are derived from `xmpp_domain`.
    pub fn new(xmpp_domain: &str, muc: &str, spaces: &str) -> Self {
        Self {
            muc: muc.to_string(),
            spaces: spaces.to_string(),
            upload: format!("upload.{xmpp_domain}"),
            extensions: format!("extensions.{xmpp_domain}"),
            push: format!("push.{xmpp_domain}"),
            community: format!("community.{xmpp_domain}"),
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
    /// Operator controls for server-side link preview enrichment.
    pub link_preview: crate::config::LinkPreviewConfig,
    /// Snapshot of `WADDLE_PROVIDER_*_WEBHOOK_*` env vars, built once at
    /// startup; the extension webhook handler reads from this instead of
    /// re-parsing env on every request.
    pub provider_ingress: Arc<crate::server::routes::extension_webhooks::ProviderIngressRegistry>,
    /// Tracker for spawned provider webhook dispatch tasks, drained on
    /// graceful shutdown so in-flight deliveries can finish updating the
    /// ledger before runtime teardown.
    pub provider_dispatch_tasks: crate::server::routes::extension_webhooks::ProviderDispatchTracker,
    /// RFC 7395 §3.8 keepalive knobs (issue #1090), parsed and
    /// validated at startup from `WADDLE_WS_KEEPALIVE_*` env vars.
    pub ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig,
    /// Ecdysis graceful-shutdown view (issue #1091): the accept path
    /// mints a [`waddle_ecdysis::ConnectionGuard`] per connection so
    /// `drain()` tracks real connections, and the per-connection loop
    /// observes the stop token to close live sessions with
    /// `<stream:error><system-shutdown/>`.
    pub shutdown: waddle_ecdysis::ShutdownHandle,
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
    /// Per-thread cross-channel view (urn:waddle:threads:0). Reads
    /// from the same backend as the inbox projection.
    pub threads_storage: Arc<dyn crate::threads::storage::ThreadsStorage>,
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
    /// First-party XMPP Push Service state and fake provider dispatch.
    pub push_service: Arc<crate::push_service::DatabasePushServiceStore>,
    /// User-server notification candidate/outbox state. This coalesces
    /// committed XMPP state into durable XEP-0357 PubSub publish jobs while
    /// leaving canonical registration state in `push_store`.
    pub notification_outbox: Arc<crate::notification_outbox::NotificationOutboxStore>,
    /// Durable derived projection of XEP-0492 notification settings from
    /// canonical XMPP state.
    pub notification_settings_projection:
        Arc<crate::notification_settings_projection::NotificationSettingsProjectionStore>,
    /// Durable derived projection of the user's `urn:waddle:dnd:0` PEP
    /// item (#367). Lookup keys: `owner_bare_jid` → typed
    /// [`waddle_xmpp::xep::xep_waddle_dnd::WaddleDnd`]. Consulted at
    /// T1 push dispatch via [`crate::dnd_reader::PepDndReader`].
    pub dnd_projection: Arc<crate::dnd_projection::DndProjectionStore>,
    /// PEP-backed DND reader plumbed into the T1 push gate
    /// (`notification_outbox`). Reads [`dnd_projection`] + a
    /// `chrono::Utc::now()` clock and resolves the typed
    /// [`crate::notification_outbox::DndState`] used to suppress
    /// candidates with [`crate::notification_outbox::SuppressedReason::WaddleDnd`].
    pub dnd_reader: Arc<crate::dnd_reader::PepDndReader>,
    /// Durable per-(user, conversation) activity projection backing
    /// the XEP-0513 `<active/>` push filter. Ingested from typed
    /// XEP-0085 chat-state changes, XEP-0490 read-marker advances,
    /// outbound message commits, and XEP-0045 presence events; read
    /// by the T1 push-gate evaluator (`notification_outbox`).
    pub notification_activity: Arc<crate::notification_activity::NotificationActivityStore>,
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
    /// PEP → community feed bridge. Observes successful PEP publishes
    /// (mood / activity / tune / avatar / vCard4) and shadow-publishes
    /// a typed feed entry on `community.<domain>` so the community
    /// Feed pane surfaces user activity automatically. Holds per-
    /// (user, kind) throttle state.
    pub pep_feed_bridge: Arc<crate::pep_feed_bridge::PepFeedBridge>,
    /// Active MUC call-thread anchors keyed by room bare JID. The room
    /// anchor message records a XEP-0359 origin-id so the SFU webhook path
    /// can later fasten a historical `<call-thread-ended/>` record to that
    /// exact anchor with XEP-0422 `<apply-to/>`.
    pub call_threads: Arc<dashmap::DashMap<BareJid, ActiveCallThread>>,
    /// Active 1:1 call-thread anchors keyed by the two bare peers plus
    /// JMI/Jingle sid. `proceed` creates the anchor; `finish` later
    /// consumes the entry to stamp the ended summary.
    pub dm_call_threads: Arc<dashmap::DashMap<DmCallThreadKey, ActiveCallThread>>,
    /// Shared pair-scoped 1:1 pinned-message projection. The pair key
    /// deliberately ignores direction so both participants fetch the
    /// same list using their peer's bare JID.
    pub dm_pin_store: Arc<DmPinStore>,
    /// Per-owner projection guard for 1:1 call-thread anchors. `ArchiveDirect`
    /// runs once per user archive row; this lets each owner receive its own
    /// MAM id exactly once while duplicate/replayed proceeds remain inert.
    pub dm_call_thread_projections: Arc<dashmap::DashSet<(BareJid, DmCallThreadKey)>>,
    /// JMI proposals keyed by the two bare peers plus sid so the later
    /// bodyless `<proceed/>` can author a typed call-thread anchor with
    /// the originally offered media.
    pub pending_dm_call_offers: Arc<dashmap::DashMap<DmCallThreadKey, PendingDmCallOffer>>,
    /// LiveKit SFU bridge — `Some` iff `LIVEKIT_*` env vars are set
    /// and `register_call_handlers` was invoked at startup. The
    /// WebSocket cleanup paths call
    /// [`waddle_sfu::SfuService::unregister_call_participant`] when a
    /// session leaves a MUC so a participant's SFU presence doesn't
    /// outlive their XMPP presence (graceful unavailable, tab close,
    /// SM-expiry — all go through `cleanup_muc_presence`).
    pub sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
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
    /// Per-stream XEP-0191 blocklist-interest state restored only on true SM resume.
    pub(super) blocklist_interested: bool,
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
    /// Inbound text frames pulled off the socket by the mid-batch ack
    /// drain (issue #1089) that were NOT `<a/>` acks. The batch writer
    /// may only consume acks out of order; everything else must reach
    /// the main frame dispatcher in arrival order, so the connection
    /// loop processes this queue before polling the socket again.
    pub(super) deferred_inbound: std::collections::VecDeque<axum::extract::ws::Utf8Bytes>,
    /// Per-connection sans-I/O state machine.
    ///
    /// Initialized in the `Unauthenticated` phase at WS upgrade by
    /// [`Self::init_prebind_state_machine`] so the RFC 7395 §3.8
    /// keepalive policy (issue #1090) covers the connection from its
    /// very first instant — a client that wedges before authenticating
    /// is reaped by the same liveness clock. Replaced at bind /
    /// SM-resume by [`Self::ensure_state_machine`] with a `Ready`
    /// machine carrying the bound user's session snapshot.
    ///
    /// The per-connection main loop dispatches [`OutboundStanza`]
    /// entries on their [`DeliveryKind`]: `PeerStanza` values feed
    /// [`InboundEvent::StanzaFromPeer`] into this state machine and
    /// the resulting outbound events are resolved via
    /// [`crate::server::routes::interpret::interpret`] before any wire
    /// write (an `Unauthenticated` machine drops them with a WARN, as
    /// the previous `None` guard did). `DirectFrame` values bypass the
    /// state machine entirely and write straight to the wire.
    ///
    /// [`InboundEvent::StanzaFromPeer`]: waddle_xmpp::protocol::InboundEvent::StanzaFromPeer
    pub(super) state_machine: Option<XmppStateMachine>,
    /// Deployment keepalive knobs, retained so the bind-time machine
    /// replacement in [`Self::ensure_state_machine`] re-seeds the
    /// policy with the same configuration the pre-bind machine used.
    pub(super) keepalive_config: waddle_xmpp::protocol::KeepaliveConfig,
}

impl WsConnState {
    pub(super) fn new() -> Self {
        Self {
            phase: ConnectionPhase::new(),
            authenticated_session: None,
            sm_state: StreamManagementState::new(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            pending_resume_stream_id: None,
            pending_resume_h: None,
            registry_owner: None,
            suppress_sm_record_next_batch: false,
            deferred_inbound: std::collections::VecDeque::new(),
            state_machine: None,
            keepalive_config: waddle_xmpp::protocol::KeepaliveConfig::default(),
        }
    }

    /// Initialize the per-connection [`XmppStateMachine`] in the
    /// `Unauthenticated` phase, seeding the RFC 7395 §3.8 keepalive
    /// policy (issue #1090) with the deployment config. Called at WS
    /// upgrade, before the connection loop's first `select!` (the
    /// caller then feeds `InboundEvent::TransportReady` to arm the
    /// keepalive clock), and again by the failed-SM-resume reset so
    /// the machine — and with it the keepalive tick chain — is never
    /// absent mid-connection.
    pub(super) fn init_prebind_state_machine(
        &mut self,
        domain: &str,
        dispatcher: &Arc<StanzaDispatcher>,
        keepalive: waddle_xmpp::protocol::KeepaliveConfig,
    ) {
        self.keepalive_config = keepalive;
        let mut sm = XmppStateMachine::new(domain.to_string(), (**dispatcher).clone());
        sm.set_keepalive_config(keepalive);
        self.state_machine = Some(sm);
    }

    /// Feed transport-level liveness evidence (any inbound WS frame —
    /// text, ping, pong, or binary) into the keepalive policy. The
    /// `KeepaliveAck` event never produces outbound effects.
    pub(super) fn note_transport_activity(&mut self) {
        if let Some(sm) = self.state_machine.as_mut() {
            let events = sm.handle(InboundEvent::KeepaliveAck);
            debug_assert!(events.is_empty(), "KeepaliveAck must be effect-free");
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
        sm.set_keepalive_config(self.keepalive_config);
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
