//! XMPP over WebSocket (RFC 7395) — WebSocket-only C2S transport.
//!
//! Provides the single WebSocket XMPP endpoint on port 443.  Each accepted
//! connection runs through a typed [`ConnectionPhase`] lifecycle state machine
//! (Unauthenticated → Authenticated → Ready → Closing) with no TCP fallback
//! and no legacy dispatch paths.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use futures::{Sink, SinkExt, StreamExt};
use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use quick_xml::{
    events::{BytesEnd, BytesStart, BytesText, Event},
    Writer,
};
use std::{str::FromStr, sync::Arc};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use waddle_xmpp::{
    auth::{parse_oauthbearer, OAuthBearerResult},
    commands::CommandRegistry,
    inbox::storage::InboxStorage,
    mam::MamStorage,
    muc::{
        room_actor::{LeaveByRealJid, RoomActor},
        room_registry_actor::{
            DestroyRoom, GetOrCreateRoom, GetRoom, IsMucJid, ListRooms, RoomRegistryActor,
        },
        RoomConfig,
    },
    protocol::{
        frame::{
            inject_client_ns_if_missing, parse_frame, InboundFrame, ParseError, MAX_FRAME_SIZE,
        },
        Blocklist, ConnectionPhase, InboundEvent, OutboundEvent, ScramPendingState,
        StanzaDispatcher, XmppStateMachine,
    },
    registry::{ConnectionRegistry, DeliveryKind, OutboundStanza},
    stream_management::{
        InMemorySmSessionRegistry, SmClaimCompletion, SmEnable, SmResume, SmSessionRegistry,
        SmStanza, StreamManagementState, SM_NS,
    },
    xep::xep0421::OccupantIdSecret,
    Stanza,
};
use xmpp_parsers::minidom::Element;

#[cfg(test)]
use waddle_xmpp::mam::InMemoryMamStorage;

use waddle_extensions::ExtensionManager;

use super::auth::AuthState;
use crate::auth::{localpart_to_jid, NativeUserStore, Session};
use crate::server::AppState;
use waddle_xmpp::auth::ScramServer;
use waddle_xmpp::pubsub::PubSubStorage;

pub mod handlers;

#[derive(Debug, Clone)]
pub struct XmppServiceDomains {
    pub muc: String,
    pub spaces: String,
    pub upload: String,
    pub extensions: String,
}

impl XmppServiceDomains {
    pub fn new(xmpp_domain: &str, component_parent_domain: &str) -> Self {
        Self {
            muc: format!("muc.{component_parent_domain}"),
            spaces: format!("spaces.{component_parent_domain}"),
            upload: format!("upload.{xmpp_domain}"),
            extensions: format!("extensions.{component_parent_domain}"),
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
    /// XEP-0397 Instant Stream Resumption token store.
    pub isr_token_store: waddle_xmpp::isr::SharedIsrTokenStore,
    /// XEP-0198 detached-session registry — holds state for clients whose
    /// WebSocket has closed but may still resume within the session timeout.
    pub sm_session_registry: Arc<InMemorySmSessionRegistry>,
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
struct WsConnState {
    phase: ConnectionPhase,
    /// The authenticated backend Session for this connection, if any.
    /// Populated on SASL success and used for SM resume/detach.
    authenticated_session: Option<Session>,
    /// XEP-0198 state for this WebSocket. Counts stanzas in both directions
    /// once enabled and holds the unacked queue used for resumption.
    sm_state: StreamManagementState,
    /// Per-connection XEP-0280 opt-in state. Updated when this resource
    /// sends `<enable/>` / `<disable/>` and restored from detached SM state
    /// on resume so re-registration preserves carbons behavior.
    carbons_enabled: bool,
    /// Per-stream RFC 6121 roster-interest state restored only on true SM resume.
    roster_interested: bool,
    /// Presence availability restored from XEP-0198 detached state.
    presence_available: bool,
    presence_show: Option<xmpp_parsers::presence::Show>,
    presence_status: Option<String>,
    presence_priority: i8,
    /// SM stream id restored by `<resume/>` but not yet removed from the
    /// detached registry. The main loop clears it after live routing is
    /// registered, closing the take-before-register fanout gap.
    pending_resume_stream_id: Option<String>,
    pending_resume_h: Option<u32>,
    /// Ownership handle for the current connection-registry entry.
    registry_owner: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// One-shot flag: when set, the main loop must NOT push the current
    /// frame's responses into `sm_state.record_outbound`. The flag is
    /// raised by `handle_sm_resume` because the responses it returns are
    /// replayed stanzas that were already pushed into the unacked queue
    /// before detach; re-recording them would double-count `outbound_count`
    /// and duplicate queue entries. The main loop resets the flag to
    /// `false` after skipping the record step.
    suppress_sm_record_next_batch: bool,
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
    state_machine: Option<XmppStateMachine>,
}

impl WsConnState {
    fn new() -> Self {
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
    fn ensure_state_machine(
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
    fn sync_state_machine_phase(&mut self) {
        if let Some(sm) = self.state_machine.as_mut() {
            if matches!(self.phase, ConnectionPhase::Closing { .. })
                && !matches!(sm.phase(), ConnectionPhase::Closing { .. })
            {
                sm.transition_to_closing();
            }
        }
    }
}

/// Create the WebSocket router
pub fn router(state: Arc<WebSocketState>) -> Router {
    Router::new()
        .route("/ws", get(xmpp_websocket_handler))
        .with_state(state)
}

/// GET /ws
///
/// WebSocket endpoint for XMPP over WebSocket (RFC 7395).
/// Upgrades HTTP connection to WebSocket and handles XMPP framing.
async fn xmpp_websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WebSocketState>>,
) -> Response {
    info!("XMPP WebSocket connection request");

    ws.protocols(["xmpp"])
        .on_upgrade(move |socket| handle_xmpp_websocket(socket, state))
}

/// Size of the outbound message channel buffer
const OUTBOUND_CHANNEL_SIZE: usize = 256;

/// Handle an XMPP WebSocket connection
async fn handle_xmpp_websocket(socket: WebSocket, state: Arc<WebSocketState>) {
    let domain = state.deps.auth_state.xmpp_domain.clone();
    info!(domain = %domain, "XMPP WebSocket connection established");

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Create outbound channel for receiving messages from other connections.
    // After the session is registered, `pending_tx` is handed to the
    // ConnectionRegistry and `None`'d out here — the registry becomes the sole
    // holder of the sender. If another session arrives for the same FullJid,
    // the registry replaces our entry, drops the sender, and our `recv()`
    // returns `None` — that's how we detect replacement and exit cleanly.
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<OutboundStanza>(OUTBOUND_CHANNEL_SIZE);
    let mut pending_tx: Option<mpsc::Sender<OutboundStanza>> = Some(outbound_tx);

    // Track connection state
    let mut conn = WsConnState::new();
    // Set when our own registry slot was replaced by a newer connection for
    // the same FullJid (detected via outbound_rx closing). In that case the
    // cleanup block below must NOT touch the registry or MUC state — those
    // belong to the newcomer now.
    let mut superseded = false;

    loop {
        tokio::select! {
            // Handle inbound WebSocket messages from the client
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        debug!(len = text.len(), "Received XMPP WebSocket message");

                        // Handle XMPP framing (RFC 7395)
                        let mut responses = handle_xmpp_frame(
                            &text,
                            &domain,
                            &state,
                            &mut conn,
                        ).await;

                        // Mirror any phase transition `handle_xmpp_frame`
                        // performed (most importantly Ready → Closing on
                        // SASL failure / stream error) into the per-
                        // connection state machine. Without this, late
                        // `PeerStanza` dispatches from the outbound
                        // channel would still go through the recipient
                        // pipeline even though the legacy phase tracker
                        // has marked the connection Closing.
                        conn.sync_state_machine_phase();

                        // Register connection after successful authentication AND resource binding
                        // This ensures the JID in ConnectionRegistry matches the JID stored in MUC room occupants
                        if let Some(jid) = conn.phase.bound_jid().cloned() {
                            if let Some(tx) = pending_tx.take() {
                                // Mirror the bind transition into the
                                // per-connection state machine (#229
                                // PR11). The SM stays `None` until
                                // here so unauthenticated traffic
                                // can't reach `on_peer_stanza`. We
                                // detect SM-resume vs fresh bind from
                                // whether `pending_resume_stream_id`
                                // was just consumed below.
                                let resumed = conn.pending_resume_stream_id.is_some();
                                // Seed the SM's XEP-0191 session-state
                                // snapshot (#229 PR13).
                                //
                                // Fresh bind: load from
                                // `DatabaseBlockingStorage`. On Err we
                                // FAIL the bind via a stream-error
                                // close rather than silently
                                // initializing an empty list — a
                                // session-long fail-open would bypass
                                // XEP-0191 for the entire connection
                                // (Codex P1 / Qodo P1 review).
                                //
                                // XEP-0198 resume: the previous
                                // session was detached/dropped, but we
                                // deliberately do NOT re-read from DB.
                                // Re-reading would let blocklist
                                // mutations from other resources
                                // during the detach window leak into
                                // the resumed stream, contradicting
                                // the snapshot semantic. The resumed
                                // session starts with an empty
                                // snapshot; subsequent XEP-0191
                                // IQ-set traffic on the resumed stream
                                // re-populates it via the SM's
                                // internal blocklist mutators.
                                let blocklist = if resumed {
                                    Blocklist::empty()
                                } else {
                                    match load_blocklist_for_bind(
                                        &state.deps.app_state.db_pool,
                                        &jid,
                                    )
                                    .await
                                    {
                                        Ok(bl) => bl,
                                        Err(error) => {
                                            error!(
                                                jid = %jid,
                                                %error,
                                                "Failed to load XEP-0191 blocklist at \
                                                 bind; failing the bind to avoid a \
                                                 session-long fail-open. Client should \
                                                 reconnect."
                                            );
                                            let stream_error =
                                                build_internal_server_error_stream_error(
                                                    "Session initialization failed; \
                                                     please reconnect.",
                                                );
                                            let _ = send_ws_message(
                                                &mut ws_sender,
                                                Message::Text(stream_error),
                                                "Failed to send blocklist-load \
                                                 stream error",
                                            )
                                            .await;
                                            break;
                                        }
                                    }
                                };
                                conn.ensure_state_machine(
                                    &domain,
                                    &state.deps.protocol.dispatcher,
                                    jid.clone(),
                                    resumed,
                                    blocklist,
                                );
                                let owner = state.deps.protocol.connection_registry.register_with_stream_state(
                                    jid.clone(),
                                    tx,
                                    conn.carbons_enabled,
                                    conn.roster_interested,
                                );
                                conn.registry_owner = Some(owner.clone());
                                if conn.presence_available {
                                    state
                                        .deps
                                        .protocol
                                        .connection_registry
                                        .update_presence(&jid, true, 0);
                                    state
                                        .deps
                                        .protocol
                                        .connection_registry
                                        .update_presence_state(
                                            &jid,
                                            conn.presence_show.as_ref().map(sm_show_name).map(str::to_string),
                                            conn.presence_status.clone(),
                                            conn.presence_priority,
                                        );
                                }
                                if let Some(stream_id) = conn.pending_resume_stream_id.take() {
                                    match state
                                        .deps
                                        .protocol
                                        .sm_session_registry
                                        .complete_claim(&stream_id)
                                        .await
                                    {
                                        Ok(Some(SmClaimCompletion::Resumed(detached))) => {
                                            if let Some(h) = conn.pending_resume_h.take() {
                                                conn.sm_state.restore_from_session(&detached);
                                                conn.sm_state.acknowledge(h);
                                                responses = vec![
                                                    waddle_xmpp::stream_management::SmResumed::new(
                                                        stream_id.clone(),
                                                        conn.sm_state.get_inbound_count(),
                                                    )
                                                    .to_xml(),
                                                ];
                                                responses
                                                    .extend(conn.sm_state.get_stanzas_to_resend(h));
                                            }
                                        }
                                        Ok(Some(SmClaimCompletion::Expired(detached))) => {
                                            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim expired before completion");
                                            cleanup_invalidated_detached_session(
                                                state.as_ref(),
                                                detached,
                                                Some(&owner),
                                            )
                                            .await;
                                            let _ = state
                                                .deps
                                                .protocol
                                                .connection_registry
                                                .unregister_if_owner(&jid, &owner);
                                            conn.registry_owner = None;
                                            conn.phase = ConnectionPhase::closing(Some(jid.clone()));
                                            responses = vec![waddle_xmpp::stream_management::SmFailed::with_condition("item-not-found").to_xml()];
                                        }
                                        Ok(None) => {
                                            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim disappeared before completion");
                                            let _ = state
                                                .deps
                                                .protocol
                                                .connection_registry
                                                .unregister_if_owner(&jid, &owner);
                                            conn.registry_owner = None;
                                            conn.phase = ConnectionPhase::closing(Some(jid.clone()));
                                            responses = vec![waddle_xmpp::stream_management::SmFailed::with_condition("item-not-found").to_xml()];
                                        }
                                        Err(error) => {
                                            warn!(stream_id = %stream_id, jid = %jid, error = %error, "Failed to complete SM resume claim");
                                            let _ = state
                                                .deps
                                                .protocol
                                                .connection_registry
                                                .unregister_if_owner(&jid, &owner);
                                            conn.registry_owner = None;
                                            conn.phase = ConnectionPhase::closing(Some(jid.clone()));
                                            responses = vec![waddle_xmpp::stream_management::SmFailed::with_condition("internal-server-error").to_xml()];
                                        }
                                    }
                                    state.deps.protocol.resumable_sessions.remove(&stream_id);
                                } else if !conn.phase.is_resumed() {
                                    match state
                                        .deps
                                        .protocol
                                        .sm_session_registry
                                        .invalidate_sessions_for_jid(&jid)
                                        .await
                                    {
                                        Ok(removed) => {
                                            for detached in removed {
                                                cleanup_invalidated_detached_session(
                                                    state.as_ref(),
                                                    detached,
                                                    Some(&owner),
                                                )
                                                .await;
                                            }
                                        }
                                        Err(error) => {
                                            warn!(jid = %jid, error = %error, "Failed to invalidate older detached SM sessions for fresh bind");
                                        }
                                    }
                                }
                                info!(
                                    jid = %jid,
                                    resumed = conn.phase.is_resumed(),
                                    carbons_enabled = conn.carbons_enabled,
                                    "WebSocket connection registered"
                                );
                            }
                        }

                        // Record outbound stanzas for XEP-0198 replay BEFORE
                        // writing them to the socket. If SM is enabled and
                        // the stanza is countable, push it into the unacked
                        // queue; a future resume will replay this exact XML.
                        //
                        // Exception: when `handle_sm_resume` just ran, the
                        // responses ARE the replay of the restored unacked
                        // queue — those stanzas already have their original
                        // sequence numbers and are still in the queue.
                        // Re-recording them would bump `outbound_count` past
                        // reality and push duplicate queue entries, breaking
                        // subsequent acks and a second resume.
                        if conn.suppress_sm_record_next_batch {
                            conn.suppress_sm_record_next_batch = false;
                        } else if conn.sm_state.enabled {
                            for frame in &responses {
                                if is_countable_stanza(frame) {
                                    conn.sm_state.record_outbound(frame.clone());
                                }
                            }
                        }

                        if !send_ws_text_frames(
                            &mut ws_sender,
                            responses,
                            "Failed to send WebSocket message",
                        )
                        .await
                        {
                            break;
                        }

                        if matches!(conn.phase, ConnectionPhase::Closing { .. }) {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        warn!("Received binary WebSocket message (not supported for XMPP)");
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if !send_ws_message(&mut ws_sender, Message::Pong(data), "Failed to send pong")
                            .await
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // Ignore pongs
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket close requested");
                        break;
                    }
                    Some(Err(e)) => {
                        error!(error = %e, "WebSocket error");
                        break;
                    }
                    None => {
                        // Stream ended
                        debug!("WebSocket stream ended");
                        break;
                    }
                }
            }

            // Handle outbound messages routed from other connections
            outbound = outbound_rx.recv() => {
                match outbound {
                    Some(outbound_stanza) => {
                        debug!(kind = ?outbound_stanza.kind, "Received outbound stanza from registry");
                        match outbound_stanza.kind {
                            DeliveryKind::DirectFrame => {
                                // Server-generated frame (carbon, IQ
                                // reply, SM ack, …). Bypass the
                                // recipient-pass pipeline and write
                                // directly to the wire — current
                                // PR1–PR10 semantic, byte-for-byte
                                // unchanged.
                                let xml = stanza_to_xml(&outbound_stanza.stanza);
                                if conn.sm_state.enabled && is_countable_stanza(&xml) {
                                    conn.sm_state.record_outbound(xml.clone());
                                }
                                if !send_ws_message(
                                    &mut ws_sender,
                                    Message::Text(xml),
                                    "Failed to send outbound stanza",
                                )
                                .await
                                {
                                    break;
                                }
                            }
                            DeliveryKind::PeerStanza => {
                                // #229 PR11: peer-routed stanza.
                                // Feed `InboundEvent::StanzaFromPeer`
                                // into the per-connection state
                                // machine so the recipient pass runs
                                // (XEP-0191 incoming block, XEP-0359
                                // recipient stamp, XEP-0313 archive,
                                // XEP-0280 received-carbons, inbox
                                // projection) and the resulting
                                // `SendStanza` events become wire
                                // bytes.
                                let Some(sm) = conn.state_machine.as_mut() else {
                                    warn!(
                                        "PeerStanza arrived before per-connection state \
                                         machine was initialized; dropping. This indicates \
                                         a routing peer targeted us before bind completed."
                                    );
                                    continue;
                                };
                                let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(
                                    outbound_stanza.stanza,
                                )));
                                let interpret_deps = build_interpret_deps(
                                    state.as_ref(),
                                    conn.authenticated_session.as_ref(),
                                );
                                let (frames, close) =
                                    drive_interpret_loop(events, sm, &interpret_deps).await;
                                // Always best-effort flush the
                                // accumulated frames first, even if
                                // `close=true`. If `CloseTransport`
                                // is ever emitted alongside a final
                                // error stanza or stream-close
                                // frame, the recipient must see
                                // those bytes before we tear down.
                                let mut send_failed = false;
                                for xml in frames {
                                    if conn.sm_state.enabled && is_countable_stanza(&xml) {
                                        conn.sm_state.record_outbound(xml.clone());
                                    }
                                    if !send_ws_message(
                                        &mut ws_sender,
                                        Message::Text(xml),
                                        "Failed to send recipient-pass frame",
                                    )
                                    .await
                                    {
                                        send_failed = true;
                                        break;
                                    }
                                }
                                if send_failed {
                                    break;
                                }
                                if close {
                                    info!("PeerStanza dispatch requested transport close");
                                    break;
                                }
                            }
                        }
                    }
                    None => {
                        // Outbound channel closed. All clones of the sender (our
                        // own outbound_tx + any copy held by the registry) have
                        // been dropped. The only path to this state after
                        // registration is a replacement register for the same
                        // FullJid: the registry drops our entry (and with it
                        // the sender) to install the new session's sender.
                        // Mark as superseded so the cleanup block skips
                        // unregister/MUC-cleanup/detach — all of those would
                        // target the newcomer's registry slot and occupant.
                        info!("Outbound channel closed; session superseded by replacement");
                        superseded = true;
                        break;
                    }
                }
            }
        }
    }

    // Connection is ending. Decide between two paths:
    //   A. Fully clean up (unregister + remove MUC occupants) — the default
    //      for non-SM sessions and for SM sessions that didn't negotiate
    //      resume.
    //   B. Detach for resumption — for SM sessions with `resume='true'`,
    //      stash state into the SmSessionRegistry so a reconnecting client
    //      can `<resume/>` without re-joining MUC or re-authenticating.
    //      MUC occupants stay in place during the detach window so other
    //      users continue to see this user as present.
    //
    // Short-circuit when this task was superseded: the registry and MUC
    // occupant slots now belong to the newer connection for this FullJid,
    // and any cleanup we do here would clobber the newcomer.
    cleanup_connection_shutdown(state.as_ref(), &mut outbound_rx, &mut conn, superseded).await;

    info!("XMPP WebSocket connection closed");
}

/// Load the bound user's persisted XEP-0191 blocklist from the
/// global database adapter and convert it into the typed
/// [`Blocklist`] shape the per-connection state machine consumes.
///
/// Used at bind time by the main loop to seed
/// [`WsConnState::ensure_state_machine`] (#229 PR13). The caller
/// **fails the bind** on `Err` rather than silently degrading to an
/// empty blocklist — that fail-closed semantic mirrors the legacy
/// per-message `is_blocked` check (`handlers/message.rs` /
/// `handlers/iq.rs` both `return vec![]` / refuse to route on
/// storage error). Treating storage failure as a session-long
/// fail-open (the agent's first cut) was a privacy/security
/// regression: any transient DB hiccup at bind would silently bypass
/// XEP-0191 enforcement for the entire connection until the client
/// reconnected. Failing the bind kicks the client to reconnect
/// instead, which is a self-healing recovery path.
///
/// Mid-session XEP-0191 IQ-set mutations remain authoritative (they
/// update the SM's internal blocklist directly); the snapshot loaded
/// here is the bind-time baseline only.
async fn load_blocklist_for_bind(
    db_pool: &Arc<crate::db::DatabasePool>,
    full_jid: &FullJid,
) -> Result<Blocklist, crate::db::blocking::BlockingStorageError> {
    let bare = full_jid.to_bare();
    let storage = crate::db::blocking::DatabaseBlockingStorage::new(db_pool.global().clone());
    storage.list_blocked_jids(&bare).await.map(Blocklist::new)
}

/// Build a `<stream:error>` close frame for a fatal session-init
/// failure (XEP-0191 blocklist load failure at bind time, etc.).
/// Pairs with breaking the WebSocket main loop so the client sees
/// the close + reconnects.
fn build_internal_server_error_stream_error(text: &str) -> String {
    let mut writer = Writer::new(Vec::new());
    let mut stream_error = BytesStart::new("stream:error");
    stream_error.push_attribute(("xmlns:stream", waddle_xmpp::ns::STREAM));
    writer
        .write_event(Event::Start(stream_error))
        .expect("serializing stream error should not fail");

    let mut internal = BytesStart::new("internal-server-error");
    internal.push_attribute(("xmlns", "urn:ietf:params:xml:ns:xmpp-streams"));
    writer
        .write_event(Event::Empty(internal))
        .expect("serializing internal-server-error should not fail");

    let mut text_elem = BytesStart::new("text");
    text_elem.push_attribute(("xmlns", "urn:ietf:params:xml:ns:xmpp-streams"));
    text_elem.push_attribute(("xml:lang", "en"));
    writer
        .write_event(Event::Start(text_elem))
        .expect("serializing stream error text should not fail");
    writer
        .write_event(Event::Text(quick_xml::events::BytesText::new(text)))
        .expect("serializing stream error text body should not fail");
    writer
        .write_event(Event::End(quick_xml::events::BytesEnd::new("text")))
        .expect("serializing stream error text close should not fail");

    writer
        .write_event(Event::End(quick_xml::events::BytesEnd::new("stream:error")))
        .expect("serializing stream error close should not fail");
    String::from_utf8(writer.into_inner()).expect("xml writer produces valid utf-8")
}

/// Build the [`crate::server::routes::interpret::Deps`] view a
/// per-connection main loop needs to resolve recipient-pass outbound
/// events. Centralized so the deps shape stays in sync with the IQ
/// flow's callsite — both go through the same `interpret()`.
///
/// `authenticated_session` is threaded through so the
/// [`OutboundEvent::DispatchToRoom`] bridge arm can preserve the
/// legacy managed-room owner check (announcements room admits server
/// owners only). Without it the dispatcher path's owner override
/// would always fail. The recipient-pass path the main loop drives
/// passes the connection's own session here.
pub(super) fn build_interpret_deps<'a>(
    state: &'a WebSocketState,
    authenticated_session: Option<&'a crate::auth::Session>,
) -> crate::server::routes::interpret::Deps<'a> {
    crate::server::routes::interpret::Deps {
        connection_registry: &state.deps.protocol.connection_registry,
        sm_session_registry: Some(&state.deps.protocol.sm_session_registry),
        mam_storage: Some(&state.deps.protocol.mam_storage),
        inbox_storage: Some(&state.deps.protocol.inbox_storage),
        extension_manager: Some(&state.deps.protocol.extension_manager),
        room_registry: Some(&state.deps.protocol.room_registry),
        web_socket_state: Some(state),
        authenticated_session,
        local_domain: state.deps.auth_state.xmpp_domain.as_str(),
        blocking_storage: Some(&state.deps.protocol.blocking_storage),
        message_dispatcher: Some(&state.deps.protocol.dispatcher),
    }
}

/// Drive the interpret-loop that resolves an initial batch of
/// [`OutboundEvent`]s and any callback-feedback rounds the dispatcher
/// chain produces (e.g. `LookupArchivedMessage` → `ArchivedMessageLoaded`
/// → resumed pipeline events).
///
/// Returns the accumulated wire frames (already serialized via
/// [`crate::server::routes::interpret::interpret`]) and a `close`
/// flag set when any round emitted [`OutboundEvent::CloseTransport`].
/// The state machine `sm` is borrowed mutably so feedback events can
/// be re-fed via `sm.handle(...)` and produce the next-round
/// `OutboundEvent` batch.
/// Drain `outbound_rx` of all immediately-available
/// [`OutboundStanza`] values and record them into the per-connection
/// XEP-0198 unacked queue (and, when a `detached_stream_id` is
/// supplied, into the detached SM session's stored replay buffer).
///
/// Dispatches on [`DeliveryKind`] so the recipient-pass contract
/// PR11 introduced is preserved through the detach path. Without
/// this dispatch, queued `PeerStanza` values would be serialized
/// raw and replayed bytes would be missing the recipient-side
/// `<stanza-id>` stamp / archive write that the recipient pipeline
/// produces — exactly the bug Qodo flagged on PR269.
///
/// `state_machine` borrows the per-connection SM mutably so it can
/// feed `InboundEvent::StanzaFromPeer` for queued PeerStanza values.
/// When `state_machine` is `None` (pre-bind queue, never reached in
/// practice for a detach drain) PeerStanza values are dropped with
/// a WARN log.
async fn drain_outbound_into_replay(
    state: &WebSocketState,
    state_machine: Option<&mut XmppStateMachine>,
    sm_state: &mut StreamManagementState,
    authenticated_session: Option<&crate::auth::Session>,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    detached_stream_id: Option<&str>,
) {
    let deps = build_interpret_deps(state, authenticated_session);
    let mut sm_borrow: Option<&mut XmppStateMachine> = state_machine;
    while let Ok(outbound_stanza) = outbound_rx.try_recv() {
        match outbound_stanza.kind {
            DeliveryKind::DirectFrame => {
                let xml = stanza_to_xml(&outbound_stanza.stanza);
                record_drained_xml(state, sm_state, detached_stream_id, xml).await;
            }
            DeliveryKind::PeerStanza => {
                let Some(sm) = sm_borrow.as_deref_mut() else {
                    warn!(
                        "PeerStanza encountered in detach drain without an SM; \
                         dropping. Resumed connection will not see this stanza."
                    );
                    continue;
                };
                let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(
                    outbound_stanza.stanza,
                )));
                let (frames, _close) = drive_interpret_loop(events, sm, &deps).await;
                for xml in frames {
                    record_drained_xml(state, sm_state, detached_stream_id, xml).await;
                }
            }
        }
    }
}

/// Helper: record a single drained XML frame into the per-connection
/// SM unacked queue and, when applicable, into the detached SM
/// session's stored replay buffer. Pulled out so both the
/// `DirectFrame` and per-frame `PeerStanza` arms in
/// [`drain_outbound_into_replay`] can share the same recording
/// contract.
async fn record_drained_xml(
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    detached_stream_id: Option<&str>,
    xml: String,
) {
    if !sm_state.enabled || !is_countable_stanza(&xml) {
        return;
    }
    sm_state.record_outbound(xml.clone());
    if let Some(stream_id) = detached_stream_id {
        let sequence = sm_state.outbound_count;
        if let Err(error) = state
            .deps
            .protocol
            .sm_session_registry
            .record_outbound_for_detached_stream_at(stream_id, sequence, xml)
            .await
        {
            warn!(stream_id = %stream_id, %error, "Failed to record drained outbound for detached SM session");
        }
    }
}

pub(super) async fn drive_interpret_loop(
    initial_events: Vec<OutboundEvent>,
    sm: &mut XmppStateMachine,
    deps: &crate::server::routes::interpret::Deps<'_>,
) -> (Vec<String>, bool) {
    let mut all_frames = Vec::new();
    let mut close = false;
    let mut events_to_run = initial_events;
    // Each iteration: resolve the current batch, append its frames,
    // honour `close`, and if the batch produced callback-feedback,
    // feed it back through the SM to get the next batch.
    while !events_to_run.is_empty() {
        let outcome = crate::server::routes::interpret::interpret(events_to_run, deps).await;
        all_frames.extend(outcome.frames);
        if outcome.close {
            close = true;
        }
        if outcome.feedback.is_empty() {
            break;
        }
        let mut next_events = Vec::new();
        for fb in outcome.feedback {
            next_events.extend(sm.handle(fb));
        }
        events_to_run = next_events;
    }
    (all_frames, close)
}

async fn send_ws_text_frames<S, E, I>(
    sender: &mut S,
    frames: I,
    failure_message: &'static str,
) -> bool
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
    I: IntoIterator<Item = String>,
{
    for frame in frames {
        debug!(len = frame.len(), "Sending XMPP WebSocket response");
        if !send_ws_message(sender, Message::Text(frame), failure_message).await {
            return false;
        }
    }

    true
}

async fn send_ws_message<S, E>(
    sender: &mut S,
    message: Message,
    failure_message: &'static str,
) -> bool
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    match sender.send(message).await {
        Ok(()) => true,
        Err(error) => {
            error!(error = %error, "{failure_message}");
            false
        }
    }
}

/// Clean up MUC room presence when a connection disconnects
/// Public alias for the MUC-presence cleanup used by the SM expired-session
/// janitor in `server::mod`. Thin passthrough so the janitor doesn't need
/// to reimplement the room traversal.
pub async fn cleanup_muc_presence_for_jid(state: &WebSocketState, jid: &FullJid) {
    cleanup_muc_presence(state, jid).await
}

async fn cleanup_connection_shutdown(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    conn: &mut WsConnState,
    superseded: bool,
) {
    // Short-circuit when this task was superseded: the registry and MUC
    // occupant slots now belong to the newer connection for this FullJid,
    // and any cleanup we do here would clobber the newcomer.
    if superseded {
        return;
    }
    // Note: we deliberately do NOT mirror `conn.phase` Closing into
    // the SM here — the drain loops below need the SM in `Ready`
    // phase so they can run the recipient pass on queued
    // `DeliveryKind::PeerStanza` values. Any explicit Closing
    // transition that needs to lock out late peer dispatches has
    // already happened via the post-`handle_xmpp_frame` mirror in
    // the main loop.

    let Some(jid) = conn.phase.cleanup_jid().cloned() else {
        return;
    };

    let should_detach_for_resume =
        conn.sm_state.is_resumable() && !matches!(conn.phase, ConnectionPhase::Closing { .. });

    if should_detach_for_resume {
        if conn.registry_owner.is_none() {
            if let Some(stream_id) = conn.pending_resume_stream_id.take() {
                if let Err(error) = state
                    .deps
                    .protocol
                    .sm_session_registry
                    .release_claim(&stream_id)
                    .await
                {
                    warn!(stream_id = %stream_id, error = %error, "Failed to release pending SM resume claim during cleanup");
                }
                state.deps.protocol.resumable_sessions.remove(&stream_id);
            }
        }
        let Some(owner) = conn.registry_owner.as_ref() else {
            debug!(jid = %jid, "Skipped SM detach for connection without registry ownership");
            return;
        };
        let presence_state = state
            .deps
            .protocol
            .connection_registry
            .get_presence_state(&jid);
        let Some(entry) = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&jid, owner)
        else {
            debug!(jid = %jid, "Skipped SM detach for non-owned registry entry");
            return;
        };

        let carbons_enabled = conn.carbons_enabled;
        let presence_available = entry.is_presence_available();
        // First detach drain: snapshot whatever's already in the
        // channel into the unacked queue. No detached_stream_id yet
        // because we haven't decided to store the detached session.
        drain_outbound_into_replay(
            state,
            conn.state_machine.as_mut(),
            &mut conn.sm_state,
            conn.authenticated_session.as_ref(),
            outbound_rx,
            None,
        )
        .await;
        let user_id = conn
            .authenticated_session
            .as_ref()
            .map(|session| session.user_id.to_string())
            .unwrap_or_else(|| jid.to_bare().to_string());
        if let Some(detached) = conn.sm_state.to_detached_session(
            waddle_xmpp::stream_management::DetachedSessionSnapshot {
                user_id,
                jid: jid.clone(),
                carbons_enabled,
                roster_interested: conn.roster_interested,
                presence_available,
                presence_show: presence_state
                    .as_ref()
                    .and_then(|state| state.show.as_deref())
                    .and_then(sm_show_from_name),
                presence_status: presence_state
                    .as_ref()
                    .and_then(|state| state.status.clone()),
                presence_priority: presence_state
                    .as_ref()
                    .map(|state| state.priority)
                    .unwrap_or_else(|| entry.presence_priority()),
            },
        ) {
            let stream_id = detached.stream_id.clone();
            match state
                .deps
                .protocol
                .sm_session_registry
                .store_session(detached)
                .await
            {
                Ok(()) => {
                    if state
                        .deps
                        .protocol
                        .connection_registry
                        .unregister_if_owner(&jid, owner)
                        .is_none()
                    {
                        debug!(jid = %jid, "Removed detached SM session after ownership moved");
                        let _ = state
                            .deps
                            .protocol
                            .sm_session_registry
                            .remove_stored_session_if_unclaimed(&stream_id)
                            .await;
                        return;
                    }
                    outbound_rx.close();
                    // Second detach drain: anything that arrived
                    // between the first drain and the registry
                    // unregister. With the detached session stored,
                    // we record both into the per-connection unacked
                    // queue AND the detached stream's replay buffer.
                    drain_outbound_into_replay(
                        state,
                        conn.state_machine.as_mut(),
                        &mut conn.sm_state,
                        conn.authenticated_session.as_ref(),
                        outbound_rx,
                        Some(&stream_id),
                    )
                    .await;
                    if let Some(session) = conn.authenticated_session.clone() {
                        state
                            .deps
                            .protocol
                            .resumable_sessions
                            .insert(stream_id.clone(), session);
                    }
                    // Remove the routing entry only — the MUC occupant
                    // slot stays. On a successful resume we'll re-register
                    // the same FullJid and presence is preserved.
                    info!(
                        jid = %jid,
                        stream_id = %stream_id,
                        "SM session detached; awaiting resume"
                    );
                }
                Err(err) => {
                    warn!(jid = %jid, error = %err, "Failed to detach SM session; falling back to full cleanup");
                    state.deps.protocol.resumable_sessions.remove(&stream_id);
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .unregister_if_owner(&jid, owner);
                    cleanup_muc_presence(state, &jid).await;
                }
            }
            return;
        }
    }

    let removed = conn.registry_owner.as_ref().is_some_and(|owner| {
        state
            .deps
            .protocol
            .connection_registry
            .unregister_if_owner(&jid, owner)
            .is_some()
    });
    if removed {
        info!(jid = %jid, "WebSocket connection unregistered");
        cleanup_muc_presence(state, &jid).await;
    } else {
        debug!(jid = %jid, "Skipped websocket cleanup for non-owned registry entry");
    }
}

async fn cleanup_muc_presence(state: &WebSocketState, jid: &FullJid) {
    for room_jid in list_room_jids(state).await {
        let Some(room_actor) = get_room_actor(state, &room_jid).await else {
            continue;
        };
        match room_actor
            .ask(LeaveByRealJid {
                sender_jid: jid.clone(),
            })
            .await
        {
            Ok(Some(outcome)) => {
                debug!(
                    room = %room_jid,
                    nick = %outcome.nick,
                    removed_last_session = outcome.removed_last_session,
                    "Removed user from MUC room on disconnect"
                );
            }
            Ok(None) => {}
            Err(error) => {
                warn!(
                    room = %room_jid,
                    jid = %jid,
                    error = ?error,
                    "Failed to remove disconnected user from MUC room"
                );
            }
        }
    }
}

async fn cleanup_invalidated_detached_session(
    state: &WebSocketState,
    detached: waddle_xmpp::stream_management::DetachedSession,
    replacement_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) {
    state
        .deps
        .protocol
        .resumable_sessions
        .remove(&detached.stream_id);
    let replacement_is_current_owner = replacement_owner.is_some_and(|owner| {
        state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&detached.jid, owner)
            .is_some()
    });
    if !replacement_is_current_owner {
        state
            .deps
            .protocol
            .connection_registry
            .unregister(&detached.jid);
    }
    if detached.presence_available && !replacement_is_current_owner {
        handlers::presence::broadcast_unavailable_for_expired_detached_session(
            state,
            &detached.jid,
        )
        .await;
    }
    cleanup_muc_presence(state, &detached.jid).await;
}

pub(crate) async fn get_room_actor(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Option<ActorRef<RoomActor>> {
    match state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(actor) => actor,
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to get room actor");
            None
        }
    }
}

pub(crate) async fn get_or_create_room_actor(
    state: &WebSocketState,
    room_jid: &BareJid,
    config: RoomConfig,
    waddle_id: String,
    channel_id: String,
) -> Option<ActorRef<RoomActor>> {
    match state
        .deps
        .protocol
        .room_registry
        .ask(GetOrCreateRoom {
            room_jid: room_jid.clone(),
            waddle_id,
            channel_id,
            config,
        })
        .await
    {
        Ok(actor) => Some(actor),
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to get or create room actor");
            None
        }
    }
}

pub(crate) async fn list_room_jids(state: &WebSocketState) -> Vec<BareJid> {
    match state.deps.protocol.room_registry.ask(ListRooms).await {
        Ok(rooms) => rooms,
        Err(error) => {
            warn!(error = ?error, "Failed to list room actors");
            Vec::new()
        }
    }
}

pub(crate) async fn is_muc_room_jid(state: &WebSocketState, room_jid: &BareJid) -> bool {
    match state
        .deps
        .protocol
        .room_registry
        .ask(IsMucJid {
            jid: room_jid.clone(),
        })
        .await
    {
        Ok(is_muc_jid) => is_muc_jid,
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to validate MUC JID");
            false
        }
    }
}

pub(crate) async fn destroy_room_actor(state: &WebSocketState, room_jid: &BareJid) -> bool {
    match state
        .deps
        .protocol
        .room_registry
        .ask(DestroyRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(destroyed) => destroyed,
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to destroy room actor");
            false
        }
    }
}

/// Convert a Stanza to XML string for WebSocket transmission
/// Serialize a stanza to XML by converting it to a `minidom::Element` via
/// `xmpp_parsers`' own `From<T> for Element` impls.  This ensures every field
/// — bodies, payloads (embeds), thread, etc. — is faithfully serialized
/// without hand-rolled format strings.
pub(crate) fn stanza_to_xml(stanza: &Stanza) -> String {
    let mut element = stanza.to_element();
    if let Stanza::Message(message) = stanza {
        // xmpp_parsers drops RFC 6121 `<thread/>` during Message -> Element.
        // Re-attach via the canonical XEP-0201 builder so the wire shape
        // — including any optional `parent` attribute — round-trips.
        // Note: post-`reattach_thread_parent` messages have
        // `message.thread = None` (the typed field is cleared and the
        // thread element lives in `msg.payloads` instead, which
        // `to_element()` already serializes faithfully). This branch
        // only re-attaches when the typed field is the only source —
        // no parent context exists in that case.
        if let Some(thread) = message.thread.as_ref() {
            // RFC 6121 §5.2.5 scopes `<thread/>` to the enclosing
            // message's namespace (`jabber:client` / `jabber:server`,
            // sometimes serialized with empty ns). Match that — any
            // `<thread>` element from a *different* namespace is an
            // unrelated extension payload that must not suppress
            // reattachment of the actual conversation thread
            // (Copilot review on PR #305).
            let stanza_ns = element.ns();
            let has_thread = element.children().any(|child| {
                child.name() == "thread" && (child.ns() == stanza_ns || child.ns().is_empty())
            });
            if !has_thread {
                // Skip the reattach if the typed `Thread` body is
                // empty: the wire shape requires a non-empty body
                // (`<thread></thread>` is malformed per RFC 6121
                // §5.2.5 / XEP-0201). `ThreadId::new` is the gate.
                if let Some(id) = waddle_xmpp::mam::ThreadId::new(thread.0.clone()) {
                    let info = waddle_xmpp::xep0201::ThreadInfo::root(id);
                    element.append_child(waddle_xmpp::xep0201::build_thread_element(
                        &info, &stanza_ns,
                    ));
                }
            }
        }
    }
    element_to_xml(element)
}

pub(crate) fn element_to_xml(element: xmpp_parsers::minidom::Element) -> String {
    let mut buf = Vec::new();
    // write_to cannot fail on a Vec<u8> in practice.
    element
        .write_to(&mut buf)
        .expect("serializing stanza to Vec<u8> should not fail");
    String::from_utf8(buf).expect("xmpp_parsers serializes valid UTF-8")
}

pub(crate) fn iq_to_xml(iq: xmpp_parsers::iq::Iq) -> String {
    stanza_to_xml(&Stanza::Iq(iq))
}

pub(crate) fn build_iq_result_xml(
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    payload: Option<xmpp_parsers::minidom::Element>,
) -> String {
    let mut iq = Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
        .attr("id", id)
        .attr("type", "result");
    if let Some(from) = from {
        iq = iq.attr("from", from);
    }
    if let Some(to) = to {
        iq = iq.attr("to", to);
    }

    let iq = if let Some(payload) = payload {
        iq.append(payload).build()
    } else {
        iq.build()
    };

    element_to_xml(iq)
}

pub(crate) fn build_iq_error_xml_with_addresses(
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    error_type: &str,
    condition: &str,
) -> String {
    let mut iq = xmpp_parsers::minidom::Element::builder("iq", "jabber:client")
        .attr("id", id)
        .attr("type", "error");
    if let Some(from) = from {
        iq = iq.attr("from", from);
    }
    if let Some(to) = to {
        iq = iq.attr("to", to);
    }

    let iq = iq
        .append(
            xmpp_parsers::minidom::Element::builder("error", "jabber:client")
                .attr("type", error_type)
                .append(
                    xmpp_parsers::minidom::Element::builder(
                        condition,
                        "urn:ietf:params:xml:ns:xmpp-stanzas",
                    )
                    .build(),
                )
                .build(),
        )
        .build();

    element_to_xml(iq)
}

pub(crate) fn build_iq_error_xml(id: &str, error_type: &str, condition: &str) -> String {
    build_iq_error_xml_with_addresses(id, None, None, error_type, condition)
}

fn build_handled_count_too_high_stream_error(acknowledged: u32, send_count: u32) -> String {
    let mut writer = Writer::new(Vec::new());
    let mut stream_error = BytesStart::new("stream:error");
    stream_error.push_attribute(("xmlns:stream", waddle_xmpp::ns::STREAM));
    writer
        .write_event(Event::Start(stream_error))
        .expect("serializing stream error should not fail");

    let mut undefined = BytesStart::new("undefined-condition");
    undefined.push_attribute(("xmlns", "urn:ietf:params:xml:ns:xmpp-streams"));
    writer
        .write_event(Event::Empty(undefined))
        .expect("serializing undefined-condition should not fail");

    let mut handled_count = BytesStart::new("handled-count-too-high");
    let acknowledged = acknowledged.to_string();
    let send_count = send_count.to_string();
    handled_count.push_attribute(("xmlns", SM_NS));
    handled_count.push_attribute(("h", acknowledged.as_str()));
    handled_count.push_attribute(("send-count", send_count.as_str()));
    writer
        .write_event(Event::Empty(handled_count))
        .expect("serializing handled-count-too-high should not fail");

    let mut text = BytesStart::new("text");
    text.push_attribute(("xmlns", "urn:ietf:params:xml:ns:xmpp-streams"));
    text.push_attribute(("xml:lang", "en"));
    writer
        .write_event(Event::Start(text))
        .expect("serializing stream error text should not fail");
    writer
        .write_event(Event::Text(BytesText::new(&format!(
            "You acknowledged {acknowledged} stanzas, but I only sent you {send_count} so far."
        ))))
        .expect("serializing stream error text should not fail");
    writer
        .write_event(Event::End(BytesEnd::new("text")))
        .expect("serializing stream error text end should not fail");
    writer
        .write_event(Event::End(BytesEnd::new("stream:error")))
        .expect("serializing stream error end should not fail");
    writer
        .write_event(Event::End(BytesEnd::new("stream:stream")))
        .expect("serializing stream close should not fail");
    String::from_utf8(writer.into_inner()).expect("quick-xml serializes valid UTF-8")
}

fn build_stream_features_xml(authenticated: bool) -> String {
    let mut features = Element::builder("features", waddle_xmpp::ns::STREAM);
    if authenticated {
        features = features.append(Element::builder("bind", waddle_xmpp::ns::BIND).build());
        features = features.append(Element::builder("ver", waddle_xmpp::ns::ROSTERVER).build());
        features =
            features.append(Element::builder("sub", "urn:xmpp:features:pre-approval").build());
        // XEP-0198: advertise stream management so clients can <enable/> it
        // after bind or <resume/> a detached session.
        features = features.append(Element::builder("sm", SM_NS).build());
        features = features.append(Element::builder("isr", waddle_xmpp::isr::ISR_NS).build());
    } else {
        features = features.append(
            Element::builder("mechanisms", waddle_xmpp::ns::SASL)
                .append(
                    Element::builder("mechanism", waddle_xmpp::ns::SASL)
                        .append("SCRAM-SHA-256")
                        .build(),
                )
                .append(
                    Element::builder("mechanism", waddle_xmpp::ns::SASL)
                        .append("OAUTHBEARER")
                        .build(),
                )
                .build(),
        );
    }

    element_to_xml(features.build())
}

fn build_stream_features_for_phase(phase: &ConnectionPhase) -> String {
    build_stream_features_xml(phase.is_authenticated())
}

fn sasl_success_xml() -> String {
    element_to_xml(Element::builder("success", waddle_xmpp::ns::SASL).build())
}

fn sasl_failure_xml(condition: &str) -> String {
    element_to_xml(
        Element::builder("failure", waddle_xmpp::ns::SASL)
            .append(Element::builder(condition, waddle_xmpp::ns::SASL).build())
            .build(),
    )
}

/// Returns true if the frame is an XMPP stanza that counts toward XEP-0198
/// handled/sent counters. Only `<iq>`, `<message>`, `<presence>` qualify —
/// stream headers, SASL frames, and SM control nonzas do not.
///
/// Matches on the element name rather than a string-prefix: a substring
/// match like `starts_with("<message")` would also accept future nonzas
/// such as `<messages>` or `<presences>`. PR #164 (xs:boolean parsing)
/// burned us on exactly this kind of substring assumption.
///
/// Hot-path: called for every outbound frame when SM is enabled, so we
/// do a byte-level scan of the element name instead of a full
/// `minidom::Element::from_str` — the parse would allocate a whole
/// DOM subtree every time only to read `.name()`.
fn is_countable_stanza(frame: &str) -> bool {
    let trimmed = frame.trim_start();
    let Some(after_lt) = trimmed.strip_prefix('<') else {
        return false;
    };
    let name_end = after_lt
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(after_lt.len());
    matches!(&after_lt[..name_end], "iq" | "message" | "presence")
}

fn sm_show_from_name(value: &str) -> Option<xmpp_parsers::presence::Show> {
    match value {
        "away" => Some(xmpp_parsers::presence::Show::Away),
        "chat" => Some(xmpp_parsers::presence::Show::Chat),
        "dnd" => Some(xmpp_parsers::presence::Show::Dnd),
        "xa" => Some(xmpp_parsers::presence::Show::Xa),
        _ => None,
    }
}

fn sm_show_name(show: &xmpp_parsers::presence::Show) -> &'static str {
    match show {
        xmpp_parsers::presence::Show::Away => "away",
        xmpp_parsers::presence::Show::Chat => "chat",
        xmpp_parsers::presence::Show::Dnd => "dnd",
        xmpp_parsers::presence::Show::Xa => "xa",
    }
}

/// Bundle the session-level borrows that XEP-0198 control handlers mutate.
/// Passed through `handle_sm_stanza` and its helpers so each signature stays
/// below the clippy too-many-arguments threshold.
struct SmCtx<'a> {
    phase: &'a mut ConnectionPhase,
    sm_state: &'a mut StreamManagementState,
    authenticated_session: &'a mut Option<Session>,
    carbons_enabled: &'a mut bool,
    presence_available: &'a mut bool,
    presence_show: &'a mut Option<xmpp_parsers::presence::Show>,
    presence_status: &'a mut Option<String>,
    presence_priority: &'a mut i8,
    pending_resume_stream_id: &'a mut Option<String>,
    pending_resume_h: &'a mut Option<u32>,
    /// Set by `handle_sm_resume` so the main loop skips SM recording for
    /// the responses it returns — those are replay stanzas already tracked
    /// in the unacked queue.
    suppress_sm_record_next_batch: &'a mut bool,
    roster_interested: &'a mut bool,
}

/// Dispatch an XEP-0198 control nonza. Isolated helper so the main frame
/// dispatcher stays flat.
async fn handle_sm_stanza(sm: SmStanza, state: &WebSocketState, ctx: SmCtx<'_>) -> Vec<String> {
    use waddle_xmpp::stream_management::SmAck;

    match sm {
        SmStanza::Enable(enable) => handle_sm_enable(enable, ctx.sm_state, ctx.phase),
        SmStanza::Request => vec![SmAck::new(ctx.sm_state.get_inbound_count()).to_xml()],
        SmStanza::Ack(ack) => {
            ctx.sm_state.acknowledge(ack.h);
            vec![]
        }
        SmStanza::Resume(resume) => handle_sm_resume(resume, state, ctx).await,
        // Server-origin nonzas should never arrive from a client. Ignore.
        SmStanza::Enabled(_) | SmStanza::Resumed(_) | SmStanza::Failed(_) => vec![],
    }
}

fn handle_sm_enable(
    enable: SmEnable,
    sm_state: &mut StreamManagementState,
    phase: &ConnectionPhase,
) -> Vec<String> {
    use waddle_xmpp::stream_management::{SmEnabled, SmFailed};

    if !phase.allows_stream_management_enable() {
        return vec![SmFailed::with_condition("unexpected-request").to_xml()];
    }
    if sm_state.enabled {
        return vec![SmFailed::with_condition("unexpected-request").to_xml()];
    }

    let stream_id = uuid::Uuid::new_v4().to_string();
    // Clamp resumption window to our server-side maximum (5 minutes) — this
    // is also the registry TTL. Clients that asked for less get what they
    // asked for; clients that asked for more or didn't specify get 300s.
    const MAX_RESUME_SECS: u32 = 300;
    let max = enable
        .max
        .map(|m| m.min(MAX_RESUME_SECS))
        .unwrap_or(MAX_RESUME_SECS);
    sm_state.enable(stream_id.clone(), enable.resume, Some(max));

    info!(stream_id = %stream_id, resume = enable.resume, max = max, "SM enabled");
    if enable.resume {
        vec![SmEnabled::with_resume(stream_id, max).to_xml()]
    } else {
        vec![SmEnabled::new(stream_id).to_xml()]
    }
}

async fn handle_sm_resume(resume: SmResume, state: &WebSocketState, ctx: SmCtx<'_>) -> Vec<String> {
    use waddle_xmpp::stream_management::{SmFailed, SmResumed};

    let SmCtx {
        phase,
        sm_state,
        authenticated_session,
        carbons_enabled,
        presence_available,
        presence_show,
        presence_status,
        presence_priority,
        pending_resume_stream_id,
        pending_resume_h,
        suppress_sm_record_next_batch,
        roster_interested,
    } = ctx;

    // Stream resumption is only legal before this transport has established a
    // fresh SASL/bind lifecycle of its own.
    if !phase.allows_stream_management_resume() {
        return vec![SmFailed::with_condition("unexpected-request").to_xml()];
    }

    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .claim_session(&resume.previd)
        .await
    {
        Ok(Some(session)) => session,
        Ok(None) => {
            info!(stream_id = %resume.previd, "SM resume rejected: session not found or expired");
            return vec![SmFailed::with_condition("item-not-found").to_xml()];
        }
        Err(e) => {
            warn!(stream_id = %resume.previd, error = %e, "SM resume failed: registry error");
            return vec![SmFailed::with_condition("internal-server-error").to_xml()];
        }
    };

    if let ConnectionPhase::Authenticated { bare_jid } = phase {
        if detached.jid.to_bare() != *bare_jid {
            warn!(
                current_jid = %bare_jid,
                resumed_jid = %detached.jid,
                "SM resume rejected due to authenticated identity mismatch"
            );
            if let Err(error) = state
                .deps
                .protocol
                .sm_session_registry
                .release_claim(&resume.previd)
                .await
            {
                warn!(stream_id = %resume.previd, error = %error, "Failed to release rejected SM resume claim");
            }
            return vec![SmFailed::with_condition("not-authorized").to_xml()];
        }
    }

    let preserve_authenticated_session = matches!(phase, ConnectionPhase::Authenticated { .. });

    if resume.h > detached.outbound_count {
        if let Err(error) = state
            .deps
            .protocol
            .sm_session_registry
            .release_claim(&resume.previd)
            .await
        {
            warn!(stream_id = %resume.previd, error = %error, "Failed to release invalid SM resume claim");
        }
        *phase = ConnectionPhase::closing(None);
        return vec![build_handled_count_too_high_stream_error(
            resume.h,
            detached.outbound_count,
        )];
    }

    // Restore SM counters + the unacked queue.
    sm_state.restore_from_session(&detached);
    // The client tells us how many of OUR outbound stanzas they've actually
    // handled. Acknowledge up to that point so the replay set is minimal.
    sm_state.acknowledge(resume.h);

    // Restore authentication identity. If the detached sidecar has no
    // matching Session (TTL expired / crash), the authenticated resume keeps
    // the fresh transport's current Session context.
    let restored_session = state
        .deps
        .protocol
        .resumable_sessions
        .get(&resume.previd)
        .map(|s| s.clone());
    if restored_session.is_none() && preserve_authenticated_session {
        warn!(
            stream_id = %resume.previd,
            jid = %detached.jid,
            "SM resumed without cached detached Session; preserving current authenticated Session"
        );
    }

    let resumed_session = restored_session.or_else(|| {
        if preserve_authenticated_session {
            authenticated_session.clone()
        } else {
            None
        }
    });

    *authenticated_session = resumed_session;
    *carbons_enabled = detached.carbons_enabled;
    *roster_interested = detached.roster_interested;
    *presence_available = detached.presence_available;
    *presence_show = detached.presence_show.clone();
    *presence_status = detached.presence_status.clone();
    *presence_priority = detached.presence_priority;
    *pending_resume_stream_id = Some(resume.previd.clone());
    *pending_resume_h = Some(resume.h);
    *phase = ConnectionPhase::ready(detached.jid.clone(), true);
    // Responses below include replayed stanzas straight from the restored
    // unacked queue. They already carry their original sequence numbers —
    // the main loop must NOT push them through `record_outbound` again.
    *suppress_sm_record_next_batch = true;

    let replay: Vec<String> = sm_state
        .get_stanzas_to_resend(resume.h)
        .into_iter()
        .collect();
    info!(
        stream_id = %resume.previd,
        jid = %detached.jid,
        replay = replay.len(),
        "SM resumed"
    );

    let mut responses = Vec::with_capacity(replay.len() + 1);
    responses.push(SmResumed::new(resume.previd, sm_state.get_inbound_count()).to_xml());
    responses.extend(replay);
    responses
}

/// Handle an XMPP frame per RFC 7395
async fn handle_xmpp_frame(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
) -> Vec<String> {
    if frame.len() > MAX_FRAME_SIZE {
        warn!(len = frame.len(), "Dropping oversized XMPP frame");
        return vec![];
    }

    let WsConnState {
        phase,
        authenticated_session,
        sm_state,
        carbons_enabled,
        presence_available,
        presence_show,
        presence_status,
        presence_priority,
        roster_interested,
        pending_resume_stream_id,
        pending_resume_h,
        suppress_sm_record_next_batch,
        state_machine,
        ..
    } = conn;
    let muc_domain = state.deps.service_domains.muc.clone();

    // SM nonzas (enable/resume/r/a) are not part of the parse_frame typed
    // vocabulary — keep the direct SmStanza check before parse_frame.
    if SmStanza::is_client_nonza_candidate(frame) {
        if let Some(sm) = SmStanza::parse(frame) {
            let ctx = SmCtx {
                phase,
                sm_state,
                authenticated_session,
                carbons_enabled,
                presence_available,
                presence_show,
                presence_status,
                presence_priority,
                pending_resume_stream_id,
                pending_resume_h,
                suppress_sm_record_next_batch,
                roster_interested,
            };
            return handle_sm_stanza(sm, state, ctx).await;
        }
    }

    let inbound = match parse_frame(frame) {
        Ok(f) => f,
        Err(ParseError::Empty) => return vec![],
        Err(err) => {
            if let Some(responses) = parse_error_responses(frame, &err) {
                if phase.scram_pending_username().is_some() && is_sasl_parse_failure(frame, &err) {
                    let _ = phase.take_scram_pending();
                }
                warn!(
                    error = %err,
                    len = frame.len(),
                    responses = responses.len(),
                    "Handled XMPP parse error with protocol response"
                );
                return responses;
            }
            warn!(error = %err, len = frame.len(), "Unhandled XMPP frame");
            return vec![];
        }
    };

    match inbound {
        InboundFrame::Open => {
            info!("XMPP stream open requested");
            let open_element = format!(
                r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" from="{}" id="{}" version="1.0" xml:lang="en"/>"#,
                domain,
                uuid::Uuid::new_v4()
            );
            let features_element = build_stream_features_for_phase(phase);
            vec![open_element, features_element]
        }

        InboundFrame::Close => {
            info!("XMPP stream close requested");
            *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
            vec![r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.to_string()]
        }

        InboundFrame::Auth { mechanism, data } => {
            if !phase.allows_sasl_auth() {
                let reset_scram_phase = phase.scram_pending_username().is_some();
                warn!(phase = ?phase, mechanism = %mechanism, "SASL auth received in invalid phase");
                if reset_scram_phase {
                    let _ = phase.take_scram_pending();
                }
                return vec![sasl_failure_xml("not-authorized")];
            }
            match mechanism.as_str() {
                "SCRAM-SHA-256" => {
                    handle_sasl_scram_client_first(&data, domain, state, phase).await
                }
                "OAUTHBEARER" => {
                    handle_sasl_oauthbearer(&data, state, authenticated_session, phase).await
                }
                other => {
                    warn!(mechanism = %other, "Unsupported SASL mechanism");
                    vec![sasl_failure_xml("invalid-mechanism")]
                }
            }
        }

        InboundFrame::SaslResponse(data) => {
            if !phase.allows_sasl_response() {
                warn!(phase = ?phase, "SASL response received in invalid phase");
                return vec![sasl_failure_xml("not-authorized")];
            }
            let scram = phase
                .take_scram_pending()
                .expect("SASL response must have pending SCRAM state");
            handle_sasl_scram_response(&data, domain, scram, authenticated_session, phase)
        }

        InboundFrame::Stanza(stanza) => {
            // Count inbound stanzas for XEP-0198: iq/message/presence always
            // count; SM control nonzas and framing elements are excluded above.
            if sm_state.enabled {
                sm_state.increment_inbound();
            }

            match *stanza {
                Stanza::Iq(iq) => {
                    let is_bind = match &iq.payload {
                        xmpp_parsers::iq::IqType::Set(e) | xmpp_parsers::iq::IqType::Get(e) => {
                            e.ns() == waddle_xmpp::ns::BIND
                        }
                        _ => false,
                    };
                    if is_bind {
                        return handle_resource_binding(&iq, domain, phase);
                    }
                    let mut iq_conn_state = handlers::iq::IqConnState {
                        carbons_enabled,
                        roster_interested,
                        state_machine: state_machine.as_mut(),
                    };
                    handlers::iq::handle_iq_with_conn_state(
                        iq,
                        domain,
                        &muc_domain,
                        state,
                        authenticated_session,
                        phase,
                        &mut iq_conn_state,
                    )
                    .await
                }

                Stanza::Presence(presence) => {
                    handlers::presence::handle_presence(
                        presence,
                        domain,
                        &muc_domain,
                        state,
                        phase,
                        authenticated_session,
                    )
                    .await
                }

                Stanza::Message(message) => {
                    handlers::message::handle_message(
                        message,
                        state,
                        phase,
                        state_machine.as_mut(),
                        authenticated_session.as_ref(),
                    )
                    .await
                }
            }
        }
    }
}

fn is_sasl_parse_failure(frame: &str, err: &ParseError) -> bool {
    match (parse_error_root_name(frame), err) {
        (Some("auth" | "response"), ParseError::MalformedSasl(_) | ParseError::InvalidXml(_)) => {
            raw_xml_attr_value(frame, "xmlns").unwrap_or(waddle_xmpp::ns::SASL)
                == waddle_xmpp::ns::SASL
        }
        _ => false,
    }
}

fn parse_error_responses(frame: &str, err: &ParseError) -> Option<Vec<String>> {
    match (parse_error_root_name(frame), err) {
        _ if is_sasl_parse_failure(frame, err) => Some(vec![sasl_failure_xml("malformed-request")]),
        (Some("iq"), ParseError::InvalidStanza { kind: "iq", .. } | ParseError::InvalidXml(_)) => {
            invalid_iq_parse_error_response(frame).map(|response| vec![response])
        }
        _ => None,
    }
}

fn parse_error_root_name(frame: &str) -> Option<&str> {
    let trimmed = frame.trim_start();
    let rest = trimmed.strip_prefix('<')?;
    if rest.starts_with('?') || rest.starts_with('!') {
        return None;
    }
    let name_end = rest
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    if name_end == 0 {
        return None;
    }
    Some(&rest[..name_end])
}

fn invalid_iq_parse_error_response(frame: &str) -> Option<String> {
    let patched = inject_client_ns_if_missing(frame);
    let parsed = Element::from_str(&patched).ok();
    let namespace = parsed
        .as_ref()
        .map(|element| element.ns().to_string())
        .or_else(|| decoded_raw_xml_attr_value(frame, "xmlns"))
        .unwrap_or(waddle_xmpp::ns::JABBER_CLIENT.to_string());
    if namespace.as_str() != waddle_xmpp::ns::JABBER_CLIENT {
        return None;
    }
    let iq_type = parsed
        .as_ref()
        .and_then(|element| element.attr("type"))
        .map(ToString::to_string)
        .or_else(|| decoded_raw_xml_attr_value(frame, "type"))?;
    if matches!(iq_type.as_str(), "result" | "error") {
        return None;
    }

    let id = parsed
        .as_ref()
        .and_then(|element| element.attr("id"))
        .map(ToString::to_string)
        .or_else(|| decoded_raw_xml_attr_value(frame, "id"))
        .unwrap_or_default();
    let response_from = parsed
        .as_ref()
        .and_then(|element| element.attr("to"))
        .map(ToString::to_string)
        .or_else(|| decoded_raw_xml_attr_value(frame, "to"));
    let response_to = parsed
        .as_ref()
        .and_then(|element| element.attr("from"))
        .map(ToString::to_string)
        .or_else(|| decoded_raw_xml_attr_value(frame, "from"));
    Some(build_iq_error_xml_with_addresses(
        &id,
        response_from.as_deref(),
        response_to.as_deref(),
        "cancel",
        "feature-not-implemented",
    ))
}

fn decoded_raw_xml_attr_value(xml: &str, attr: &str) -> Option<String> {
    raw_xml_attr_value(xml, attr).map(decode_xml_attr_value)
}

fn decode_xml_attr_value(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }

    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(pos) = rest.find('&') {
        decoded.push_str(&rest[..pos]);
        let entity_start = &rest[pos + 1..];
        let Some(entity_end) = entity_start.find(';') else {
            decoded.push_str(&rest[pos..]);
            return decoded;
        };
        let entity = &entity_start[..entity_end];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|dec| dec.parse::<u32>().ok())
                })
                .and_then(char::from_u32)
                .filter(|&ch| is_valid_xml_char(ch)),
        };

        if let Some(ch) = replacement {
            decoded.push(ch);
        } else {
            decoded.push('&');
            decoded.push_str(entity);
            decoded.push(';');
        }
        rest = &entity_start[entity_end + 1..];
    }
    decoded.push_str(rest);
    decoded
}

fn is_valid_xml_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{9}'
            | '\u{A}'
            | '\u{D}'
            | '\u{20}'..='\u{D7FF}'
            | '\u{E000}'..='\u{FFFD}'
            | '\u{10000}'..='\u{10FFFF}'
    )
}

fn looks_like_attr_token(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    looks_like_attr_name(name)
}

fn looks_like_attr_name(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.'))
}

fn raw_xml_attr_value<'a>(xml: &'a str, attr: &str) -> Option<&'a str> {
    let trimmed = xml.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.first().copied()? != b'<' {
        return None;
    }

    let mut idx = 1;
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    if idx >= bytes.len() || matches!(bytes[idx], b'/' | b'!' | b'?') {
        return None;
    }

    while idx < bytes.len()
        && !bytes[idx].is_ascii_whitespace()
        && !matches!(bytes[idx], b'/' | b'>')
    {
        idx += 1;
    }

    let mut fallback = None;
    while idx < bytes.len() {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() || matches!(bytes[idx], b'>' | b'/') {
            break;
        }

        let name_start = idx;
        while idx < bytes.len()
            && !bytes[idx].is_ascii_whitespace()
            && !matches!(bytes[idx], b'=' | b'>' | b'/')
        {
            idx += 1;
        }
        let name_end = idx;

        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() || bytes[idx] != b'=' {
            continue;
        }
        idx += 1;

        let value_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let had_value_whitespace = idx != value_start;
        let Some(&quote) = bytes.get(idx) else {
            return fallback;
        };
        if quote != b'"' && quote != b'\'' {
            let rest = &trimmed[idx..];
            let token_end = rest
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .unwrap_or(rest.len());
            let token = &rest[..token_end];
            let looks_like_next_attr = had_value_whitespace
                && (looks_like_attr_token(token)
                    || (looks_like_attr_name(token)
                        && rest[token_end..].trim_start().starts_with('=')));
            if !looks_like_next_attr {
                if &trimmed[name_start..name_end] == attr {
                    fallback = Some(token.trim_end_matches('/'));
                }
                idx += token_end;
            }
            continue;
        }
        idx += 1;
        let value_start = idx;
        while idx < bytes.len() && bytes[idx] != quote {
            idx += 1;
        }
        if idx >= bytes.len() {
            return fallback;
        }

        if &trimmed[name_start..name_end] == attr {
            return Some(&trimmed[value_start..idx]);
        }

        idx += 1;
    }

    fallback
}

/// Handle SASL OAUTHBEARER authentication.
async fn handle_sasl_oauthbearer(
    b64_data: &str,
    state: &WebSocketState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    debug!("SASL OAUTHBEARER auth attempt");

    let decoded = match BASE64_STANDARD.decode(b64_data) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER: failed to decode base64 data");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let token = match parse_oauthbearer(&decoded) {
        Ok(OAuthBearerResult::Credentials(credentials)) => credentials.token,
        Ok(OAuthBearerResult::DiscoveryRequest) => {
            warn!("SASL OAUTHBEARER: discovery request received on token-auth WebSocket path");
            return vec![sasl_failure_xml("not-authorized")];
        }
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER: failed to parse bearer data");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    match state
        .deps
        .auth_state
        .session_manager
        .validate_session(&token)
        .await
    {
        Ok(session) => {
            let bare_jid_str =
                match localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
                {
                    Ok(jid) => jid,
                    Err(e) => {
                        warn!(
                            localpart = %session.xmpp_localpart,
                            error = %e,
                            "SASL OAUTHBEARER: failed to build JID from session localpart",
                        );
                        return vec![sasl_failure_xml("not-authorized")];
                    }
                };

            let full_jid = match format!("{}/pending", bare_jid_str).parse::<FullJid>() {
                Ok(jid) => jid,
                Err(e) => {
                    warn!(jid = %bare_jid_str, error = %e, "SASL OAUTHBEARER: JID construction failed");
                    return vec![sasl_failure_xml("not-authorized")];
                }
            };

            info!(
                jid = %bare_jid_str,
                user_id = %session.user_id,
                "SASL OAUTHBEARER authentication successful",
            );

            *authenticated_session = Some(session);
            *phase = ConnectionPhase::authenticated(&full_jid);

            vec![sasl_success_xml()]
        }
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER authentication failed");
            vec![sasl_failure_xml("not-authorized")]
        }
    }
}

/// Handle SASL SCRAM-SHA-256 client-first-message.
///
/// Parses the client-first to extract the username, looks up stored SCRAM
/// credentials, creates a ScramServer with the user's salt/iterations, and
/// returns a `<challenge>` frame.
async fn handle_sasl_scram_client_first(
    b64_data: &str,
    domain: &str,
    state: &WebSocketState,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    debug!("SASL SCRAM-SHA-256 auth attempt");

    let decoded = match BASE64_STANDARD.decode(b64_data.trim()) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, "SCRAM: failed to decode base64 client-first");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let client_first = match String::from_utf8(decoded) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "SCRAM: invalid UTF-8 in client-first");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    // Parse username from client-first-message: "n,,n=<username>,r=<nonce>"
    // Use a temporary ScramServer to extract it.
    let username = {
        let mut tmp = ScramServer::new();
        match tmp.process_client_first(&client_first) {
            Ok(result) => result.username,
            Err(e) => {
                warn!(error = %e, "SCRAM: failed to parse client-first");
                return vec![sasl_failure_xml("not-authorized")];
            }
        }
    };

    // Look up SCRAM credentials for the user
    let native_user_store =
        NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone());

    let creds = match native_user_store
        .get_scram_credentials(&username, domain)
        .await
    {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            warn!(username = %username, "SCRAM: user not found");
            return vec![sasl_failure_xml("not-authorized")];
        }
        Err(e) => {
            warn!(error = %e, username = %username, "SCRAM: credential lookup failed");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    // Create ScramServer with the user's stored salt and iterations, then
    // process the client-first again to produce a challenge with the correct params.
    let mut scram_server = ScramServer::with_salt_b64(creds.salt_b64, creds.iterations);
    let server_first = match scram_server.process_client_first(&client_first) {
        Ok(result) => result,
        Err(e) => {
            warn!(error = %e, "SCRAM: failed to process client-first with stored params");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let challenge_b64 = BASE64_STANDARD.encode(server_first.message.as_bytes());
    debug!(username = %username, "SCRAM-SHA-256 challenge generated");

    *phase = ConnectionPhase::scram_pending(ScramPendingState::new(
        scram_server,
        creds.stored_key,
        creds.server_key,
        username,
    ));

    vec![element_to_xml(
        Element::builder("challenge", waddle_xmpp::ns::SASL)
            .append(challenge_b64)
            .build(),
    )]
}

/// Handle SASL SCRAM-SHA-256 response (client-final-message).
///
/// Verifies the client proof against stored keys and returns `<success>` or
/// `<failure>`.
fn handle_sasl_scram_response(
    b64_data: &str,
    domain: &str,
    mut scram: ScramPendingState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    let decoded = match BASE64_STANDARD.decode(b64_data.trim()) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, "SCRAM: failed to decode base64 client-final");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let client_final = match String::from_utf8(decoded) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "SCRAM: invalid UTF-8 in client-final");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let server_final = match scram.process_client_final(&client_final) {
        Ok(result) => result,
        Err(e) => {
            warn!(
                error = %e,
                username = %scram.username(),
                "SCRAM-SHA-256 authentication failed"
            );
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    // Authentication successful - create session
    let bare_jid_str = format!("{}@{}", scram.username(), domain);
    let full_jid = match format!("{}/pending", bare_jid_str).parse::<FullJid>() {
        Ok(jid) => jid,
        Err(e) => {
            warn!(error = %e, jid = %bare_jid_str, "SCRAM: JID construction failed");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let bare_jid: BareJid = match bare_jid_str.parse() {
        Ok(jid) => jid,
        Err(e) => {
            warn!(error = %e, "SCRAM: bare JID parse failed");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    info!(
        jid = %bare_jid_str,
        "SASL SCRAM-SHA-256 authentication successful",
    );

    let session = Session::new(&bare_jid.to_string(), scram.username(), scram.username());

    *authenticated_session = Some(session);
    *phase = ConnectionPhase::authenticated(&full_jid);

    let success_b64 = BASE64_STANDARD.encode(server_final.message.as_bytes());
    vec![element_to_xml(
        Element::builder("success", waddle_xmpp::ns::SASL)
            .append(success_b64)
            .build(),
    )]
}

fn build_bind_result_xml(id: &str, full_jid: &FullJid) -> String {
    element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("id", id)
            .attr("type", "result")
            .append(
                Element::builder("bind", waddle_xmpp::ns::BIND)
                    .append(
                        Element::builder("jid", waddle_xmpp::ns::BIND)
                            .append(full_jid.to_string())
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
}

/// Handle resource binding IQ.
fn handle_resource_binding(
    iq: &xmpp_parsers::iq::Iq,
    _domain: &str,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    let id = iq.id.clone();

    if !phase.allows_resource_binding() {
        warn!(phase = ?phase, id = %id, "Resource binding received in invalid phase");
        return vec![build_iq_error_xml(&id, "auth", "not-authorized")];
    }

    let Some(bare_jid) = phase.authenticated_bare_jid() else {
        warn!(id = %id, "Resource binding without authenticated session");
        return vec![build_iq_error_xml(&id, "auth", "not-authorized")];
    };
    let bare_jid = bare_jid.clone();
    let resource = match &iq.payload {
        xmpp_parsers::iq::IqType::Set(e) | xmpp_parsers::iq::IqType::Get(e) => e
            .get_child("resource", waddle_xmpp::ns::BIND)
            .map(|r| r.text().trim().to_string())
            .filter(|v| !v.is_empty()),
        _ => None,
    }
    .unwrap_or_else(|| format!("ws-{}", uuid::Uuid::new_v4()));

    let full_jid_str = format!("{}/{}", bare_jid, resource);

    if let Ok(full_jid) = full_jid_str.parse::<FullJid>() {
        info!(jid = %full_jid, id = %id, "Resource bound");
        *phase = ConnectionPhase::ready(full_jid.clone(), false);
        vec![build_bind_result_xml(&id, &full_jid)]
    } else {
        warn!(jid = %full_jid_str, "Invalid JID during resource binding");
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::permissions::{Object, ObjectType, Relation, Subject, Tuple, WriteTuple};
    use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
    use crate::server::AppState;
    use futures::Sink;
    use hmac::{Hmac, Mac};
    use pbkdf2::pbkdf2_hmac;
    use sha2::{Digest, Sha256};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use tokio::sync::mpsc;
    // Handler functions moved to sub-modules but called directly in tests
    use handlers::iq::{handle_iq, handle_iq_with_conn_state, IqConnState};
    use handlers::presence::{handle_muc_join, handle_muc_leave, parse_room_jid_context};
    // Types moved out of mod.rs scope but used in tests
    use waddle_extensions::ExtensionConfig;
    use waddle_xmpp::commands::{CommandContext, CommandResult};
    use waddle_xmpp::muc::room_actor::{
        ChangeAffiliation, GetSnapshot, JoinWithAffiliation, SetSubject,
    };
    use waddle_xmpp::registry::BroadcastOutcome;
    use waddle_xmpp::Affiliation;
    use xmpp_parsers::iq::{Iq, IqType};
    use xmpp_parsers::message::MessageType as XmppMessageType;

    #[derive(Default)]
    struct TestSink {
        fail_after: Option<usize>,
        sent: Vec<Message>,
    }

    impl TestSink {
        fn succeeds() -> Self {
            Self::default()
        }

        fn fails_after(sent_before_failure: usize) -> Self {
            Self {
                fail_after: Some(sent_before_failure),
                sent: Vec::new(),
            }
        }
    }

    impl Sink<Message> for TestSink {
        type Error = &'static str;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            if matches!(self.fail_after, Some(limit) if self.sent.len() >= limit) {
                return Err("synthetic websocket sink failure");
            }

            self.sent.push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    async fn create_test_websocket_state() -> Arc<WebSocketState> {
        create_test_websocket_state_with_extension_manager(empty_extension_manager().await).await
    }

    async fn empty_extension_manager() -> Arc<ExtensionManager> {
        Arc::new(
            ExtensionManager::from_config(ExtensionConfig {
                enabled: false,
                cache_dir: std::env::temp_dir()
                    .join("waddle-extension-test-cache")
                    .display()
                    .to_string(),
                modules: Vec::new(),
            })
            .await
            .expect("empty extension manager"),
        )
    }

    async fn create_test_websocket_state_with_extension_manager(
        extension_manager: Arc<ExtensionManager>,
    ) -> Arc<WebSocketState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig;
        let db_pool = DatabasePool::new(config, pool_config)
            .await
            .expect("db pool");

        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.expect("migrations");

        let server_config = ServerConfig::test_homeserver();
        let app_state = Arc::new(AppState::new(Arc::new(db_pool)));
        let mut auth_state_inner = AuthState::new(
            app_state.clone(),
            &server_config,
            Some(b"test-encryption-key-32-bytes!!!"),
        );
        // The dispatcher path's bare-JID branch
        // (`OutboundEvent::RouteToConnection`) drops cross-domain
        // bare JIDs without running the headless recipient pass —
        // the production env var defaults `xmpp_domain` to
        // `"localhost"`, but every fixture in this test module uses
        // `@example.com` JIDs. Pin the local domain to match so the
        // headless recipient pass actually fires for offline-bare
        // JID delivery (#229 PR15) under unit-test fixtures.
        auth_state_inner.xmpp_domain = "example.com".to_string();
        let auth_state = Arc::new(auth_state_inner);
        let mam_storage: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());

        let mut dispatcher = StanzaDispatcher::new();
        waddle_xmpp::protocol::handlers::register_default_handlers(&mut dispatcher);
        waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);

        Arc::new(WebSocketState {
            deps: WebSocketDeps {
                app_state,
                auth_state,
                service_domains: XmppServiceDomains {
                    muc: "muc.example.com".to_string(),
                    spaces: "spaces.example.com".to_string(),
                    upload: "upload.example.com".to_string(),
                    extensions: "extensions.example.com".to_string(),
                },
                protocol: ProtocolServices {
                    connection_registry: Arc::new(ConnectionRegistry::new()),
                    room_registry: kameo::spawn(RoomRegistryActor::new(
                        "muc.example.com".to_string(),
                        OccupantIdSecret::new(b"test-occupant-id-secret-32-bytes-long".to_vec())
                            .expect("test secret meets length floor"),
                    )),
                    mam_storage,
                    inbox_storage: Arc::new(
                        waddle_xmpp::inbox::storage::InMemoryInboxStorage::new(),
                    ),
                    blocking_storage: Arc::new(
                        waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new(),
                    ),
                    command_registry: Arc::new(CommandRegistry::new()),
                    extension_manager,
                    dispatcher: Arc::new(dispatcher),
                    pubsub_storage: Arc::new(waddle_xmpp::pubsub::InMemoryPubSubStorage::new()),
                    push_store: Arc::new(waddle_xmpp::push::InMemoryPushStore::new()),
                    isr_token_store: waddle_xmpp::isr::create_shared_store(),
                    sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
                    resumable_sessions: Arc::new(dashmap::DashMap::new()),
                },
                occupant_id_secret: OccupantIdSecret::new(
                    b"test-occupant-id-secret-32-bytes-long".to_vec(),
                )
                .expect("test secret meets length floor"),
            },
        })
    }

    async fn create_test_session(state: &WebSocketState, username: &str) -> Session {
        let session = Session::new(&uuid::Uuid::new_v4().to_string(), username, username);
        state
            .deps
            .auth_state
            .session_manager
            .create_session(&session)
            .await
            .expect("session");
        session
    }

    async fn create_test_server_owner_session(state: &WebSocketState, username: &str) -> Session {
        let session = create_test_session(state, username).await;
        state
            .deps
            .app_state
            .permission_actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                    Relation::new("owner"),
                    Subject::user(&session.user_id),
                ),
            })
            .await
            .expect("server owner tuple");
        session
    }

    async fn register_test_native_user(state: &WebSocketState, username: &str, password: &str) {
        let native_user_store =
            NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone());
        native_user_store
            .register(crate::auth::native::RegisterRequest {
                username: username.to_string(),
                domain: state.deps.auth_state.xmpp_domain.clone(),
                password: password.to_string(),
                email: None,
            })
            .await
            .expect("native user");
    }

    fn scram_client_final_from_challenge(
        username: &str,
        password: &str,
        client_nonce: &str,
        challenge_b64: &str,
    ) -> String {
        type HmacSha256 = Hmac<Sha256>;

        fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
            let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }

        fn sha256(data: &[u8]) -> Vec<u8> {
            let mut hasher = Sha256::new();
            hasher.update(data);
            hasher.finalize().to_vec()
        }

        let challenge = String::from_utf8(
            BASE64_STANDARD
                .decode(challenge_b64)
                .expect("challenge base64"),
        )
        .expect("challenge utf8");
        let mut combined_nonce = None;
        let mut salt_b64 = None;
        let mut iterations = None;
        for attr in challenge.split(',') {
            if let Some(value) = attr.strip_prefix("r=") {
                combined_nonce = Some(value.to_string());
            } else if let Some(value) = attr.strip_prefix("s=") {
                salt_b64 = Some(value.to_string());
            } else if let Some(value) = attr.strip_prefix("i=") {
                iterations = Some(value.parse::<u32>().expect("iterations"));
            }
        }

        let combined_nonce = combined_nonce.expect("combined nonce");
        let salt = BASE64_STANDARD
            .decode(salt_b64.expect("salt"))
            .expect("salt base64");
        let iterations = iterations.expect("iterations");

        let mut salted_password = vec![0u8; 32];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut salted_password);
        let client_key = hmac_sha256(&salted_password, b"Client Key");
        let stored_key = sha256(&client_key);
        let channel_binding = BASE64_STANDARD.encode("n,,");
        let client_final_without_proof = format!("c={channel_binding},r={combined_nonce}");
        let client_first_bare = format!(
            "n={},r={client_nonce}",
            waddle_xmpp::auth::encode_sasl_name(username)
        );
        let auth_message = format!("{client_first_bare},{challenge},{client_final_without_proof}");
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let client_proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(left, right)| left ^ right)
            .collect();

        format!(
            "{client_final_without_proof},p={}",
            BASE64_STANDARD.encode(client_proof)
        )
    }

    async fn snapshot_room(
        state: &WebSocketState,
        room_jid: &BareJid,
    ) -> waddle_xmpp::muc::room_actor::RoomSnapshot {
        let room_actor = get_room_actor(state, room_jid).await.expect("room actor");
        room_actor.ask(GetSnapshot).await.expect("room snapshot")
    }

    fn parse_message_for_test(xml: &str) -> xmpp_parsers::message::Message {
        match parse_frame(xml).expect("message parses") {
            InboundFrame::Stanza(stanza) => match *stanza {
                Stanza::Message(msg) => msg,
                _ => panic!("expected message stanza"),
            },
            _ => panic!("expected message stanza"),
        }
    }

    fn assert_sample_payload(xml: &str, element_name: &str, url: &str, owner: &str, name: &str) {
        let parsed = parse_message_for_test(xml);
        let payload = parsed
            .payloads
            .iter()
            .find(|payload| {
                payload.name() == element_name && payload.ns() == "urn:waddle:test-extension:1"
            })
            .unwrap_or_else(|| panic!("missing {element_name} sample payload"));
        assert_eq!(payload.attr("url"), Some(url));
        assert_eq!(payload.attr("owner"), Some(owner));
        assert_eq!(payload.attr("name"), Some(name));
    }

    fn parse_iq_for_test(xml: &str) -> xmpp_parsers::iq::Iq {
        match parse_frame(xml).expect("iq parses") {
            InboundFrame::Stanza(stanza) => match *stanza {
                Stanza::Iq(iq) => iq,
                _ => panic!("expected iq stanza"),
            },
            _ => panic!("expected iq stanza"),
        }
    }

    fn disco_items_iq_frame(id: &str, to: &str, node: Option<&str>) -> String {
        let mut query =
            xmpp_parsers::minidom::Element::builder("query", waddle_xmpp::disco::DISCO_ITEMS_NS);
        if let Some(node) = node {
            query = query.attr("node", node);
        }
        stanza_to_xml(&Stanza::Iq(Iq {
            from: None,
            to: Some(to.parse().expect("valid iq destination")),
            id: id.to_string(),
            payload: IqType::Get(query.build()),
        }))
    }

    fn disco_info_iq_frame(id: &str, to: &str, node: Option<&str>) -> String {
        let mut query =
            xmpp_parsers::minidom::Element::builder("query", waddle_xmpp::disco::DISCO_INFO_NS);
        if let Some(node) = node {
            query = query.attr("node", node);
        }
        stanza_to_xml(&Stanza::Iq(Iq {
            from: None,
            to: Some(to.parse().expect("valid iq destination")),
            id: id.to_string(),
            payload: IqType::Get(query.build()),
        }))
    }

    fn ready_phase(jid: &FullJid) -> ConnectionPhase {
        ConnectionPhase::ready(jid.clone(), false)
    }

    /// Construct a per-connection [`XmppStateMachine`] seeded with the
    /// shared test dispatcher (registered with the default message
    /// handler chain) and drive the given message through the new
    /// thin-adapter [`handle_message`]. Mirrors the production main
    /// loop's bind-time wiring (#229 PR11/PR13) closely enough for the
    /// unit-level tests in this module to assert end-to-end semantics
    /// against the dispatcher path.
    async fn handle_message_for_test(
        state: &WebSocketState,
        sender_jid: &FullJid,
        session: Option<&Session>,
        message: xmpp_parsers::message::Message,
    ) -> Vec<String> {
        let mut sm = XmppStateMachine::new(
            state.deps.auth_state.xmpp_domain.clone(),
            (*state.deps.protocol.dispatcher).clone(),
        );
        sm.transition_to_ready(sender_jid.clone(), false);
        sm.set_blocklist(Blocklist::empty());
        let phase = ConnectionPhase::ready(sender_jid.clone(), false);
        handlers::message::handle_message(message, state, &phase, Some(&mut sm), session).await
    }

    fn authenticated_phase_for_session(session: &Session, domain: &str) -> ConnectionPhase {
        let pending_jid: FullJid = format!("{}@{domain}/pending", session.xmpp_localpart)
            .parse()
            .expect("pending jid");
        ConnectionPhase::authenticated(&pending_jid)
    }

    #[tokio::test]
    async fn send_ws_text_frames_stops_after_first_send_failure() {
        let mut sink = TestSink::fails_after(1);

        let sent = send_ws_text_frames(
            &mut sink,
            vec!["<open/>".to_string(), "<features/>".to_string()],
            "synthetic failure",
        )
        .await;

        assert!(!sent);
        assert_eq!(sink.sent.len(), 1);
    }

    #[tokio::test]
    async fn send_ws_message_returns_true_on_success() {
        let mut sink = TestSink::succeeds();

        let sent = send_ws_message(
            &mut sink,
            Message::Text("<pong/>".into()),
            "unexpected failure",
        )
        .await;

        assert!(sent);
        assert_eq!(sink.sent.len(), 1);
    }

    #[tokio::test]
    async fn handle_xmpp_frame_open_dispatches_via_typed_ingress() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 2);
        assert!(responses[0].contains("urn:ietf:params:xml:ns:xmpp-framing"));
        assert!(responses[1].contains("<features"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_auth_dispatches_via_typed_ingress() {
        let state = create_test_websocket_state().await;
        let session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

        assert_eq!(responses, vec![sasl_success_xml()]);
        assert!(conn.phase.is_authenticated());
        assert!(conn.authenticated_session.is_some());
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_auth_returns_malformed_request() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl">payload</auth>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses, vec![sasl_failure_xml("malformed-request")]);
    }

    // ---------------------------------------------------------------
    // #229 PR11 — DeliveryKind dispatch in the per-connection main loop
    // ---------------------------------------------------------------
    //
    // The actual main-loop entry point is `xmpp_websocket_handler`, an
    // async function tied to a real WebSocket sink. To test the
    // dispatch logic in isolation we exercise its two helpers
    // (`build_interpret_deps`, `drive_interpret_loop`) and the
    // `WsConnState::ensure_state_machine` lifecycle directly. End-to-
    // end coverage of the routing flow lands once PR12 emits
    // `OutboundStanza::peer_stanza` from `RouteToConnection`.

    #[tokio::test]
    async fn ensure_state_machine_initializes_sm_in_ready_phase() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let jid: jid::FullJid = "alice@example.com/web".parse().expect("jid");

        assert!(
            conn.state_machine.is_none(),
            "fresh WsConnState has no state machine"
        );

        conn.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            jid.clone(),
            false,
            Blocklist::empty(),
        );

        let sm = conn.state_machine.as_ref().expect("SM initialized");
        assert!(matches!(sm.phase(), ConnectionPhase::Ready { .. }));
        assert_eq!(sm.phase().bound_jid(), Some(&jid));
    }

    #[tokio::test]
    async fn ensure_state_machine_seeds_blocklist_from_database_at_bind() {
        // #229 PR13: bind-time SM seeding from
        // `DatabaseBlockingStorage`. Persist a single blocked entry
        // for alice, run the bind-time loader against the same
        // global pool, hand the result to `ensure_state_machine`,
        // then drive a synchronous dispatch through a probe handler
        // and observe the seeded entry on the `MessageContext`
        // snapshot. Without the seed, the snapshot would be
        // `Blocklist::empty()` and `BlockingFilterHandler` (post
        // PR16 cutover) would silently regress XEP-0191 enforcement.
        use crate::db::blocking::DatabaseBlockingStorage;
        use std::sync::Mutex;
        use waddle_xmpp::protocol::{HandlerOutcome, MessageContext, MessageHandler};

        let state = create_test_websocket_state().await;
        let alice_full: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let alice_bare = alice_full.to_bare();
        let blocked_bare: BareJid = "blocked@example.com".parse().expect("bare");

        // Seed persistence with one entry.
        let storage = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
        storage
            .add_blocks(&alice_bare, &[blocked_bare.to_string()])
            .await
            .expect("add_blocks");

        // Mirror the bind-site loader.
        let blocklist = load_blocklist_for_bind(&state.deps.app_state.db_pool, &alice_full)
            .await
            .expect("blocklist load succeeds when storage is healthy");
        let loaded: Vec<_> = blocklist.iter().cloned().collect();
        assert_eq!(loaded, vec![blocked_bare.clone()]);

        // Build a probe-only dispatcher so the assertion isolates
        // the SM seeding behaviour from any side effects of the
        // production message-pipeline chain (those have their own
        // dedicated tests). The goal here is "the seeded blocklist
        // shows up on the `MessageContext` snapshot".
        let captured: Arc<Mutex<Vec<waddle_xmpp::protocol::Blocklist>>> =
            Arc::new(Mutex::new(Vec::new()));
        struct SnapshotProbe {
            captured: Arc<Mutex<Vec<waddle_xmpp::protocol::Blocklist>>>,
        }
        impl MessageHandler for SnapshotProbe {
            fn name(&self) -> &'static str {
                "ws-bind-blocklist-probe"
            }
            fn handle(
                &self,
                _message: &mut xmpp_parsers::message::Message,
                ctx: &MessageContext<'_>,
            ) -> HandlerOutcome {
                self.captured
                    .lock()
                    .expect("mutex")
                    .push(ctx.blocklist.clone());
                HandlerOutcome::Continue(Vec::new())
            }
        }
        let mut probe_dispatcher = StanzaDispatcher::new();
        probe_dispatcher.register_message(Arc::new(SnapshotProbe {
            captured: captured.clone(),
        }));
        let dispatcher = Arc::new(probe_dispatcher);

        let mut conn = WsConnState::new();
        conn.ensure_state_machine(
            "example.com",
            &dispatcher,
            alice_full.clone(),
            false,
            blocklist,
        );

        // Drive a chat message dispatch so the probe fires.
        let mut msg =
            xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
        msg.from = Some(jid::Jid::from(alice_full.clone()));
        msg.type_ = XmppMessageType::Chat;
        msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("hello".to_string()),
        );
        let sm = conn.state_machine.as_mut().expect("SM");
        sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Message(msg),
        ))));

        let snapshots = captured.lock().expect("mutex").clone();
        assert_eq!(snapshots.len(), 1, "probe runs exactly once");
        let entries: Vec<_> = snapshots[0].iter().cloned().collect();
        assert_eq!(
            entries,
            vec![blocked_bare],
            "MessageContext snapshot must reflect the persisted blocklist"
        );
    }

    #[tokio::test]
    async fn drive_interpret_loop_resolves_send_stanza_into_wire_frames() {
        // Recipient pass produces `OutboundEvent::SendStanza` for the
        // wire write. Drive the loop with a single SendStanza event
        // and assert it serializes cleanly into a frame (no extra
        // round-trips through the SM since no callback feedback is
        // produced).
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let jid: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        conn.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            jid,
            false,
            Blocklist::empty(),
        );
        let sm = conn.state_machine.as_mut().expect("SM");

        let mut msg =
            xmpp_parsers::message::Message::new(Some("alice@example.com".parse().expect("to jid")));
        msg.from = Some("bob@example.com/desk".parse().expect("from jid"));
        msg.type_ = xmpp_parsers::message::MessageType::Chat;
        msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("hello".to_string()),
        );

        let initial_events = vec![OutboundEvent::SendStanza(Box::new(Stanza::Message(msg)))];
        let deps = build_interpret_deps(state.as_ref(), None);
        let (frames, close) = drive_interpret_loop(initial_events, sm, &deps).await;

        assert!(!close, "SendStanza alone never requests transport close");
        assert_eq!(frames.len(), 1, "single SendStanza → single wire frame");
        assert!(
            frames[0].contains("hello"),
            "wire frame carries the message body; got {:?}",
            frames[0]
        );
    }

    #[tokio::test]
    async fn drive_interpret_loop_runs_recipient_pass_for_peer_message() {
        // Production-shape regression: feed `InboundEvent::StanzaFromPeer`
        // through a Ready state machine and drive the resulting events
        // via `drive_interpret_loop`. The recipient pass MUST produce
        // a wire frame containing bob's recipient-side `<stanza-id>`
        // stamp so XEP-0359 §5 conformance is preserved end-to-end
        // through the production helpers.
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let bob_full: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        conn.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            bob_full,
            false,
            Blocklist::empty(),
        );
        let sm = conn.state_machine.as_mut().expect("SM");

        let mut peer_msg =
            xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
        peer_msg.from = Some("alice@example.com/web".parse().expect("from jid"));
        peer_msg.type_ = xmpp_parsers::message::MessageType::Chat;
        peer_msg.id = Some("alice-wire-id".to_string());
        peer_msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("hi bob".to_string()),
        );
        // Pre-stamp alice's sender-side stanza-id so we can verify
        // the recipient pass *adds* bob's stamp rather than replacing
        // alice's (XEP-0359 §5 cross-archive preservation).
        peer_msg
            .payloads
            .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
                "alice-A1",
                &"alice@example.com".parse::<jid::Jid>().expect("jid"),
            ));

        let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Message(
            peer_msg,
        ))));
        let deps = build_interpret_deps(state.as_ref(), None);
        let (frames, _close) = drive_interpret_loop(events, sm, &deps).await;

        // Recipient pass terminates with at least one SendStanza
        // carrying bob's stamp.
        assert!(
            !frames.is_empty(),
            "recipient pass must produce at least one wire frame"
        );
        let combined = frames.join("\n");
        assert!(
            combined.contains("by=\"bob@example.com\""),
            "recipient-pass wire frame must carry bob's stanza-id stamp; got: {combined}"
        );
        assert!(
            combined.contains("alice-A1"),
            "recipient-pass wire frame must preserve alice's cross-archive stanza-id; \
             got: {combined}"
        );
        assert!(
            combined.contains("hi bob"),
            "recipient-pass wire frame must carry the message body; got: {combined}"
        );
    }

    #[tokio::test]
    async fn drain_outbound_dispatches_direct_frame_into_unacked_unchanged() {
        // Regression for the detach-drain DeliveryKind dispatch
        // Qodo flagged on PR269: DirectFrame values must be recorded
        // byte-for-byte (no recipient pipeline). This is the live
        // contract the SM-resume replay path depends on.
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let jid: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        conn.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            jid,
            false,
            Blocklist::empty(),
        );
        // Enable SM tracking so `record_outbound` actually retains the
        // drained XML.
        conn.sm_state.enabled = true;

        let mut msg =
            xmpp_parsers::message::Message::new(Some("alice@example.com".parse().expect("to jid")));
        msg.from = Some("bob@example.com/desk".parse().expect("from jid"));
        msg.type_ = xmpp_parsers::message::MessageType::Chat;
        msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("plain".to_string()),
        );
        let expected_xml = stanza_to_xml(&Stanza::Message(msg.clone()));

        let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
        tx.send(OutboundStanza::new(Stanza::Message(msg)))
            .await
            .expect("send");
        drop(tx); // close so try_recv eventually returns Empty

        drain_outbound_into_replay(
            state.as_ref(),
            conn.state_machine.as_mut(),
            &mut conn.sm_state,
            None,
            &mut rx,
            None,
        )
        .await;

        let queue = conn.sm_state.get_stanzas_to_resend(0);
        assert_eq!(queue.len(), 1, "DirectFrame recorded once");
        assert_eq!(
            queue[0], expected_xml,
            "DirectFrame is recorded byte-for-byte (no recipient pipeline rewrite)"
        );
    }

    #[tokio::test]
    async fn drain_outbound_dispatches_peer_stanza_through_recipient_pass() {
        // PeerStanza values queued during detach must run through
        // the recipient pass before being recorded in the SM unacked
        // queue, so a resumed connection's replay carries the
        // recipient-side `<stanza-id>` stamp. Without the dispatch
        // (Qodo's flagged bug), the queued bytes would be the raw
        // peer stanza missing bob's stamp.
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let bob_full: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        conn.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            bob_full,
            false,
            Blocklist::empty(),
        );
        conn.sm_state.enabled = true;

        let mut peer_msg =
            xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
        peer_msg.from = Some("alice@example.com/web".parse().expect("from jid"));
        peer_msg.type_ = xmpp_parsers::message::MessageType::Chat;
        peer_msg.id = Some("alice-wire-id".to_string());
        peer_msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("hi from drain".to_string()),
        );
        peer_msg
            .payloads
            .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
                "alice-A1",
                &"alice@example.com".parse::<jid::Jid>().expect("jid"),
            ));

        let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
        tx.send(OutboundStanza::peer_stanza(Stanza::Message(peer_msg)))
            .await
            .expect("send");
        drop(tx);

        drain_outbound_into_replay(
            state.as_ref(),
            conn.state_machine.as_mut(),
            &mut conn.sm_state,
            None,
            &mut rx,
            None,
        )
        .await;

        let queue = conn.sm_state.get_stanzas_to_resend(0);
        assert!(
            !queue.is_empty(),
            "PeerStanza drain MUST record at least the recipient-pass wire frame"
        );
        let combined: String = queue.join("\n");
        assert!(
            combined.contains("by=\"bob@example.com\""),
            "drained PeerStanza replay must carry bob's recipient-side stanza-id; got: {combined}"
        );
        assert!(
            combined.contains("alice-A1"),
            "drained PeerStanza replay must preserve alice's cross-archive stamp; got: {combined}"
        );
    }

    #[tokio::test]
    async fn sync_state_machine_phase_mirrors_closing_into_sm() {
        // PR269 review fix #2/#6: when WsConnState.phase transitions
        // to Closing (via SASL failure / stream-error / explicit
        // shutdown inside `handle_xmpp_frame`), the per-connection SM
        // must mirror so it stops accepting late `PeerStanza`
        // dispatches from the outbound channel.
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let jid: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        conn.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            jid.clone(),
            false,
            Blocklist::empty(),
        );

        // Sanity: SM starts in Ready.
        assert!(matches!(
            conn.state_machine.as_ref().expect("sm").phase(),
            ConnectionPhase::Ready { .. }
        ));

        // Simulate the legacy phase tracker transitioning to Closing.
        conn.phase = ConnectionPhase::closing(Some(jid.clone()));
        conn.sync_state_machine_phase();

        assert!(matches!(
            conn.state_machine.as_ref().expect("sm").phase(),
            ConnectionPhase::Closing { .. }
        ));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_sasl_response_returns_malformed_request() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">not-closed"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses, vec![sasl_failure_xml("malformed-request")]);
    }

    #[tokio::test]
    async fn handle_xmpp_frame_wrong_namespace_auth_stays_silent() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<auth xmlns="jabber:client" mechanism="SCRAM-SHA-256">x</auth>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert!(responses.is_empty(), "expected no response: {responses:?}");
    }

    #[tokio::test]
    async fn websocket_features_advertise_oauthbearer() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 2);
        let features = &responses[1];
        assert!(
            features.contains("<mechanism>OAUTHBEARER</mechanism>"),
            "expected OAUTHBEARER in WebSocket SASL mechanisms"
        );
        assert!(
            features.contains("<mechanism>SCRAM-SHA-256</mechanism>"),
            "expected SCRAM-SHA-256 in WebSocket SASL mechanisms"
        );
        assert!(
            !features.contains("<mechanism>PLAIN</mechanism>"),
            "expected WebSocket SASL mechanisms to exclude PLAIN"
        );
    }

    #[tokio::test]
    async fn websocket_close_moves_connection_into_closing_phase() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(
            responses,
            vec![r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.to_string()]
        );
        assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));
    }

    #[tokio::test]
    async fn websocket_close_keeps_bound_connection_in_closing_phase() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        conn.phase = ConnectionPhase::ready(jid, false);

        let responses = handle_xmpp_frame(
            r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(
            responses,
            vec![r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.to_string()]
        );
        assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));

        let _ = handle_xmpp_frame(
            r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;
        assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));
    }

    #[tokio::test]
    async fn websocket_rejects_plain_auth() {
        let state = create_test_websocket_state().await;
        let frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "PLAIN")
                .append(BASE64_STANDARD.encode("\0alice\0session-token"))
                .build(),
        );
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

        assert_eq!(responses, vec![sasl_failure_xml("invalid-mechanism")]);
        assert!(!conn.phase.is_authenticated());
        assert!(!conn.phase.is_ready());
        assert!(conn.authenticated_session.is_none());
    }

    #[tokio::test]
    async fn websocket_oauthbearer_authenticates_session_token() {
        let state = create_test_websocket_state().await;
        let session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

        assert_eq!(responses, vec![sasl_success_xml()]);
        assert!(conn.phase.is_authenticated());
        assert!(!conn.phase.is_ready());
        assert_eq!(
            conn.authenticated_session
                .as_ref()
                .map(|s| s.user_id.as_str()),
            Some(session.user_id.as_str())
        );
        let expected_bare =
            localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
                .expect("session localpart should produce JID");
        assert_eq!(
            conn.phase.authenticated_bare_jid().map(ToString::to_string),
            Some(expected_bare)
        );
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    }

    #[tokio::test]
    async fn websocket_rejects_reauthentication_after_successful_sasl() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let mut conn = WsConnState::new();

        let first = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(first, vec![sasl_success_xml()]);
        let first_bare_jid = conn.phase.authenticated_bare_jid().cloned();
        let first_user_id = conn
            .authenticated_session
            .as_ref()
            .map(|saved| saved.user_id.clone());

        let second = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

        assert_eq!(second, vec![sasl_failure_xml("not-authorized")]);
        assert!(conn.phase.is_authenticated());
        assert!(!conn.phase.is_ready());
        assert_eq!(conn.phase.authenticated_bare_jid(), first_bare_jid.as_ref());
        assert_eq!(
            conn.authenticated_session
                .as_ref()
                .map(|saved| saved.user_id.clone()),
            first_user_id
        );
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    }

    #[tokio::test]
    async fn websocket_failed_scram_response_resets_phase_to_unauthenticated() {
        let state = create_test_websocket_state().await;
        let domain = state.deps.auth_state.xmpp_domain.clone();
        register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
        let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "SCRAM-SHA-256")
                .append(client_first)
                .build(),
        );
        let response_frame = element_to_xml(
            Element::builder("response", waddle_xmpp::ns::SASL)
                .append(BASE64_STANDARD.encode("not-valid"))
                .build(),
        );
        let mut conn = WsConnState::new();

        let auth_responses =
            handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(auth_responses.len(), 1);
        let challenge = Element::from_str(&auth_responses[0]).expect("challenge xml");
        assert_eq!(challenge.name(), "challenge");
        assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

        let response_responses =
            handle_xmpp_frame(&response_frame, &domain, state.as_ref(), &mut conn).await;

        assert_eq!(response_responses, vec![sasl_failure_xml("not-authorized")]);
        assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
        assert!(!conn.phase.is_authenticated());
        assert!(conn.authenticated_session.is_none());
    }

    #[tokio::test]
    async fn websocket_malformed_scram_response_resets_phase_and_allows_retry() {
        let state = create_test_websocket_state().await;
        let domain = state.deps.auth_state.xmpp_domain.clone();
        register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
        let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "SCRAM-SHA-256")
                .append(client_first)
                .build(),
        );
        let malformed_response = r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">not-closed"#;
        let mut conn = WsConnState::new();

        let first = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(first.len(), 1);
        assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

        let malformed =
            handle_xmpp_frame(malformed_response, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(malformed, vec![sasl_failure_xml("malformed-request")]);
        assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
        assert!(!conn.phase.is_authenticated());
        assert!(conn.authenticated_session.is_none());

        let retry = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(retry.len(), 1);
        let challenge = Element::from_str(&retry[0]).expect("challenge xml");
        assert_eq!(challenge.name(), "challenge");
        assert_eq!(conn.phase.scram_pending_username(), Some("alice"));
    }

    #[tokio::test]
    async fn websocket_failed_reauth_during_scram_resets_phase_and_allows_retry() {
        let state = create_test_websocket_state().await;
        let domain = state.deps.auth_state.xmpp_domain.clone();
        register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
        let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "SCRAM-SHA-256")
                .append(client_first)
                .build(),
        );
        let mut conn = WsConnState::new();

        let first = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(first.len(), 1);
        assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

        let second = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(second, vec![sasl_failure_xml("not-authorized")]);
        assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));

        let third = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(third.len(), 1);
        let challenge = Element::from_str(&third[0]).expect("challenge xml");
        assert_eq!(challenge.name(), "challenge");
        assert_eq!(conn.phase.scram_pending_username(), Some("alice"));
    }

    #[tokio::test]
    async fn sm_resume_is_rejected_during_scram_and_scram_can_still_complete() {
        let state = create_test_websocket_state().await;
        let domain = state.deps.auth_state.xmpp_domain.clone();
        let password = "correct horse battery staple";
        let client_nonce = "fyko+d2lbbFgONRv9qkxdawL";
        register_test_native_user(state.as_ref(), "alice", password).await;

        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "SCRAM-SHA-256")
                .append(BASE64_STANDARD.encode(format!("n,,n=alice,r={client_nonce}")))
                .build(),
        );
        let mut conn = WsConnState::new();

        let auth_responses =
            handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        let challenge = Element::from_str(&auth_responses[0]).expect("challenge xml");
        let challenge_b64 = challenge.text();
        assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

        let resume_responses = handle_xmpp_frame(
            "<resume xmlns='urn:xmpp:sm:3' previd='stream-xyz' h='0'/>",
            &domain,
            state.as_ref(),
            &mut conn,
        )
        .await;
        assert_eq!(resume_responses.len(), 1);
        let failed = Element::from_str(&resume_responses[0]).expect("failed xml");
        assert_eq!(failed.name(), "failed");
        assert!(failed
            .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some());
        assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

        let response_frame = element_to_xml(
            Element::builder("response", waddle_xmpp::ns::SASL)
                .append(BASE64_STANDARD.encode(scram_client_final_from_challenge(
                    "alice",
                    password,
                    client_nonce,
                    &challenge_b64,
                )))
                .build(),
        );
        let response_responses =
            handle_xmpp_frame(&response_frame, &domain, state.as_ref(), &mut conn).await;

        assert_eq!(response_responses.len(), 1);
        let success = Element::from_str(&response_responses[0]).expect("success xml");
        assert_eq!(success.name(), "success");
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
        assert!(conn.phase.is_authenticated());
    }

    #[tokio::test]
    async fn websocket_resource_bind_returns_client_iq() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let bind_frame = element_to_xml(
            Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
                .attr("id", "bind-1")
                .attr("type", "set")
                .append(
                    Element::builder("bind", waddle_xmpp::ns::BIND)
                        .append(
                            Element::builder("resource", waddle_xmpp::ns::BIND)
                                .append("web")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        );
        let mut conn = WsConnState::new();

        let auth_responses =
            handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(auth_responses, vec![sasl_success_xml()]);

        let bind_responses =
            handle_xmpp_frame(&bind_frame, "example.com", state.as_ref(), &mut conn).await;

        assert!(conn.phase.is_ready());
        assert_eq!(bind_responses.len(), 1);

        let response = Element::from_str(&bind_responses[0]).expect("bind response XML");
        assert_eq!(response.name(), "iq");
        assert_eq!(response.ns(), waddle_xmpp::ns::JABBER_CLIENT);
        assert_eq!(response.attr("id"), Some("bind-1"));
        assert_eq!(response.attr("type"), Some("result"));

        let bind = response
            .get_child("bind", waddle_xmpp::ns::BIND)
            .expect("bind child");
        let jid = bind
            .get_child("jid", waddle_xmpp::ns::BIND)
            .expect("jid child");
        let expected_bare =
            localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
                .expect("session localpart should produce JID");
        let expected_full = format!("{expected_bare}/web");
        assert!(
            jid.text() == expected_full,
            "bound jid should match expected resource"
        );
        let bound_jid = conn.phase.bound_jid().map(ToString::to_string);
        assert!(
            bound_jid.as_deref() == Some(expected_full.as_str()),
            "connection state should store the bound jid"
        );
        assert!(matches!(
            &conn.phase,
            ConnectionPhase::Ready {
                full_jid,
                resumed: false,
                ..
            } if full_jid.to_string() == expected_full
        ));
    }

    #[tokio::test]
    async fn websocket_resource_bind_without_resource_uses_unique_server_resource() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let bind_frame = element_to_xml(
            Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
                .attr("id", "bind-2")
                .attr("type", "set")
                .append(Element::builder("bind", waddle_xmpp::ns::BIND).build())
                .build(),
        );
        let mut conn = WsConnState::new();

        let auth_responses =
            handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(auth_responses, vec![sasl_success_xml()]);

        let bind_responses =
            handle_xmpp_frame(&bind_frame, "example.com", state.as_ref(), &mut conn).await;

        assert!(conn.phase.is_ready());
        assert_eq!(bind_responses.len(), 1);

        let response = Element::from_str(&bind_responses[0]).expect("bind response XML");
        let bind = response
            .get_child("bind", waddle_xmpp::ns::BIND)
            .expect("bind child");
        let jid = bind
            .get_child("jid", waddle_xmpp::ns::BIND)
            .expect("jid child")
            .text();

        let expected_bare =
            localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
                .expect("session localpart should produce JID");
        let prefix = format!("{expected_bare}/ws-");
        assert!(
            jid.starts_with(&prefix),
            "server-assigned resource should be unique ws-* value: {jid}"
        );
        assert!(matches!(
            &conn.phase,
            ConnectionPhase::Ready {
                full_jid,
                resumed: false,
                ..
            } if full_jid.to_string() == jid
        ));
    }

    #[tokio::test]
    async fn websocket_rejects_second_resource_bind_after_ready() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let bind_one = element_to_xml(
            Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
                .attr("id", "bind-1")
                .attr("type", "set")
                .append(
                    Element::builder("bind", waddle_xmpp::ns::BIND)
                        .append(
                            Element::builder("resource", waddle_xmpp::ns::BIND)
                                .append("web")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        );
        let bind_two = element_to_xml(
            Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
                .attr("id", "bind-2")
                .attr("type", "set")
                .append(
                    Element::builder("bind", waddle_xmpp::ns::BIND)
                        .append(
                            Element::builder("resource", waddle_xmpp::ns::BIND)
                                .append("mobile")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        );
        let mut conn = WsConnState::new();

        let auth_responses =
            handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(auth_responses, vec![sasl_success_xml()]);
        let bind_one_responses =
            handle_xmpp_frame(&bind_one, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(bind_one_responses.len(), 1);
        let first_bound_jid = conn.phase.bound_jid().cloned();

        let bind_two_responses =
            handle_xmpp_frame(&bind_two, "example.com", state.as_ref(), &mut conn).await;

        assert_eq!(
            bind_two_responses,
            vec![build_iq_error_xml("bind-2", "auth", "not-authorized")]
        );
        assert_eq!(conn.phase.bound_jid(), first_bound_jid.as_ref());
        assert!(matches!(conn.phase, ConnectionPhase::Ready { .. }));
    }

    #[tokio::test]
    async fn muc_stale_leave_does_not_remove_current_resource() {
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
        let current_jid: FullJid = "alice@example.com/current".parse().expect("current jid");
        let stale_jid: FullJid = "alice@example.com/stale".parse().expect("stale jid");

        handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &current_jid,
            "alice",
            &Some(owner_session),
        )
        .await;

        let responses = handle_muc_leave(state.as_ref(), &room_jid, &stale_jid, "alice").await;

        assert_eq!(responses.len(), 1);
        let response = Element::from_str(&responses[0]).expect("leave response XML");
        assert_eq!(response.name(), "presence");
        assert_eq!(response.attr("type"), Some("unavailable"));

        let room = snapshot_room(state.as_ref(), &room_jid).await.room;
        assert_eq!(room.find_nick_by_real_jid(&current_jid), Some("alice"));
        assert!(room.find_nick_by_real_jid(&stale_jid).is_none());
        assert_eq!(room.occupant_count(), 1);
    }

    #[tokio::test]
    async fn muc_join_responses_use_client_namespace() {
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

        let responses = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &sender_jid,
            "alice",
            &Some(owner_session),
        )
        .await;

        assert_eq!(responses.len(), 2);

        let self_presence = Element::from_str(&responses[0]).expect("self presence xml");
        assert_eq!(self_presence.name(), "presence");
        assert_eq!(self_presence.ns(), waddle_xmpp::ns::JABBER_CLIENT);
        let user_x = self_presence
            .get_child("x", "http://jabber.org/protocol/muc#user")
            .expect("muc user payload");
        let item = user_x
            .get_child("item", "http://jabber.org/protocol/muc#user")
            .expect("muc user item");
        assert_eq!(item.attr("jid"), Some("alice@example.com/web"));
        assert_eq!(item.attr("affiliation"), Some("owner"));
        assert_eq!(item.attr("role"), Some("moderator"));
        assert!(user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("100")));
        assert!(user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("110")));
        assert!(
            self_presence
                .get_child("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
                .is_some(),
            "self-presence must carry XEP-0421 occupant-id"
        );

        let subject_message = Element::from_str(&responses[1]).expect("subject xml");
        assert_eq!(subject_message.name(), "message");
        assert_eq!(subject_message.ns(), waddle_xmpp::ns::JABBER_CLIENT);
        assert_eq!(subject_message.attr("type"), Some("groupchat"));
    }

    #[tokio::test]
    async fn xep_0045_join_replay_exposes_existing_occupant_real_jids() {
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "icepuma").await;
        let room_jid: BareJid = "mentions@muc.example.com".parse().expect("room jid");

        let occupants = [
            ("icepuma", "icepuma@example.com/web"),
            ("randax", "randax@example.com/desktop"),
            ("rawkode", "rawkode@example.com/mobile"),
        ];

        for (index, (nick, full_jid)) in occupants.iter().enumerate() {
            let sender_jid: FullJid = full_jid.parse().expect("occupant jid");
            let session = if index == 0 {
                Some(owner_session.clone())
            } else {
                None
            };
            let _ = handle_muc_join(
                state.as_ref(),
                "example.com",
                &room_jid,
                &sender_jid,
                nick,
                &session,
            )
            .await;
        }

        let joiner: FullJid = "witness@example.com/browser".parse().expect("joiner jid");
        let responses = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &joiner,
            "witness",
            &None,
        )
        .await;

        for (nick, full_jid) in occupants {
            let from = format!("{room_jid}/{nick}");
            let replay = responses
                .iter()
                .filter_map(|xml| Element::from_str(xml).ok())
                .find(|element| {
                    element.name() == "presence"
                        && element.attr("from") == Some(from.as_str())
                        && element.attr("to") == Some(joiner.as_str())
                })
                .unwrap_or_else(|| panic!("missing replay presence for {nick}: {responses:?}"));
            let user_x = replay
                .get_child("x", "http://jabber.org/protocol/muc#user")
                .expect("muc user payload");
            let item = user_x
                .get_child("item", "http://jabber.org/protocol/muc#user")
                .expect("muc user item");

            assert_eq!(item.attr("jid"), Some(full_jid));
            assert!(
                user_x
                    .children()
                    .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
                "non-anonymous replay must disclose status 100 for {nick}"
            );
            assert!(
                !user_x
                    .children()
                    .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
                "replay presence for another occupant must not be self-presence"
            );
            assert!(
                replay
                    .get_child("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
                    .is_some(),
                "replay presence for {nick} must carry XEP-0421 occupant-id"
            );
        }
    }

    #[tokio::test]
    async fn xep_0045_section_7_2_15_join_replay_serializes_full_subject_envelope() {
        // Boundary test for the WebSocket join wiring (Copilot review,
        // PR #319). Pre-populates `MucRoom.subject` with a multi-language
        // SubjectState via the production `SetSubject` actor message,
        // then drives a fresh join through `handle_muc_join` and asserts
        // the serialized subject message carries every conformance
        // element: `from='room/setter_nick'`, every persisted
        // `<subject xml:lang='...'>`, the XEP-0203 `<delay/>` from the
        // room JID, and the XEP-0421 `<occupant-id/>`.
        use chrono::TimeZone;
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let setter_session = create_test_server_owner_session(state.as_ref(), "setter").await;
        let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
        let setter_jid: FullJid = "setter@example.com/web".parse().expect("setter jid");
        let joiner_jid: FullJid = "alice@example.com/web".parse().expect("joiner jid");

        // Bootstrap the room actor by joining the setter (first joiner
        // becomes Owner → Moderator), then seed the subject state with
        // a multi-language `texts` map matching what a real §8.1
        // dispatch would produce.
        handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &setter_jid,
            "setter-nick",
            &Some(setter_session),
        )
        .await;
        let room_actor = get_room_actor(state.as_ref(), &room_jid)
            .await
            .expect("room actor");
        let texts = waddle_xmpp::muc::RoomSubjectTexts::from_iter([
            (String::new(), "Default subject".to_string()),
            ("en".to_string(), "English subject".to_string()),
        ]);
        let set_at = chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();
        room_actor
            .ask(SetSubject {
                texts,
                setter: setter_jid.to_bare(),
                setter_nick: "setter-nick".to_string(),
                set_at,
            })
            .await
            .expect("SetSubject succeeds");

        let responses = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &joiner_jid,
            "alice",
            &Some(owner_session),
        )
        .await;

        // 1 existing-occupant presence (setter) + self-presence + subject = 3.
        assert_eq!(
            responses.len(),
            3,
            "join responses: existing-occupants + self-presence + subject"
        );

        let subject_msg =
            Element::from_str(responses.last().expect("subject is last")).expect("subject xml");
        assert_eq!(subject_msg.name(), "message");
        assert_eq!(subject_msg.attr("type"), Some("groupchat"));
        assert_eq!(
            subject_msg.attr("from"),
            Some("channel@muc.example.com/setter-nick"),
            "§7.2.15 nick-form `from` for set room"
        );

        let subject_children: Vec<&Element> = subject_msg
            .children()
            .filter(|c| c.name() == "subject")
            .collect();
        assert_eq!(
            subject_children.len(),
            2,
            "every persisted xml:lang variant round-trips into the join replay"
        );
        let default_subject = subject_children
            .iter()
            .find(|c| c.attr("xml:lang").is_none() || c.attr("xml:lang") == Some(""))
            .expect("default-language subject present");
        assert_eq!(default_subject.text(), "Default subject");
        let en_subject = subject_children
            .iter()
            .find(|c| c.attr("xml:lang") == Some("en"))
            .expect("xml:lang=en subject present");
        assert_eq!(en_subject.text(), "English subject");

        let delay = subject_msg
            .get_child("delay", "urn:xmpp:delay")
            .expect("XEP-0203 <delay/> stamped per §7.2.15 SHOULD");
        assert_eq!(
            delay.attr("from"),
            Some("channel@muc.example.com"),
            "§7.2.15 conditional MUST: delay's `from` is the room JID"
        );
        assert!(
            delay.attr("stamp").is_some_and(|s| !s.is_empty()),
            "delay stamp present and non-empty"
        );

        let occupant_id = subject_msg
            .get_child("occupant-id", "urn:xmpp:occupant-id:0")
            .expect("XEP-0421 <occupant-id/> stamped on set-subject replay");
        assert!(
            occupant_id.attr("id").is_some_and(|s| !s.is_empty()),
            "occupant-id `id` attribute present"
        );

        assert!(
            subject_msg.children().all(|c| c.name() != "body"),
            "subject message MUST have no <body/>"
        );
    }

    #[test]
    fn test_parse_room_jid_valid() {
        let jid: jid::BareJid = "channel456@muc.example.com".parse().unwrap();
        let (waddle, channel) = parse_room_jid_context(&jid);
        assert_eq!(waddle, "space");
        assert_eq!(channel, "channel456");
    }

    #[test]
    fn test_parse_room_jid_fallback() {
        let jid: jid::BareJid = "singlename@muc.example.com".parse().unwrap();
        let (waddle, channel) = parse_room_jid_context(&jid);
        assert_eq!(waddle, "space");
        assert_eq!(channel, "singlename");
    }

    #[tokio::test]
    async fn handle_iq_roster_query_returns_parseable_result() {
        let state = create_test_websocket_state().await;
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        let frame = r#"<iq xmlns="jabber:client" id="roster-1" type="get"><query xmlns="jabber:iq:roster"/></iq>"#;
        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&jid),
        )
        .await;
        assert_eq!(responses.len(), 1);

        let iq_xml = responses.first().expect("roster response");
        let element = Element::from_str(iq_xml).expect("valid IQ XML");
        let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");

        assert_eq!(iq.id, "roster-1");
        match iq.payload {
            xmpp_parsers::iq::IqType::Result(Some(payload)) => {
                assert_eq!(payload.name(), "query");
                assert_eq!(payload.ns(), "jabber:iq:roster");
            }
            other => panic!("expected roster IQ result payload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_xmpp_frame_roster_get_marks_connection_interested_for_detach() {
        let state = create_test_websocket_state().await;
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::ready(jid, false);
        let frame = r#"<iq xmlns="jabber:client" id="roster-interest" type="get"><query xmlns="jabber:iq:roster"/></iq>"#;

        let responses = handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut conn).await;

        assert_eq!(responses.len(), 1);
        assert!(
            conn.roster_interested,
            "roster get must persist interest on WsConnState for SM detach"
        );
    }

    #[tokio::test]
    async fn handle_iq_roster_query_without_xmlns_survives_xmlns_like_attribute_value() {
        let state = create_test_websocket_state().await;
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        let frame = r#"<iq id="roster-attr" type="get" data="xmlns=bogus"><query xmlns="jabber:iq:roster"/></iq>"#;
        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&jid),
        )
        .await;
        assert_eq!(responses.len(), 1);

        let iq_xml = responses.first().expect("roster response");
        let element = Element::from_str(iq_xml).expect("valid IQ XML");
        let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
        assert_eq!(iq.id, "roster-attr");
        assert!(matches!(
            iq.payload,
            xmpp_parsers::iq::IqType::Result(Some(_))
        ));
    }

    #[tokio::test]
    async fn handle_iq_roster_query_requires_ready_phase() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let frame = r#"<iq xmlns="jabber:client" id="roster-prebind" type="get"><query xmlns="jabber:iq:roster"/></iq>"#;

        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session.clone()),
            &authenticated_phase_for_session(&session, "example.com"),
        )
        .await;

        let response = responses.first().expect("roster auth error");
        assert!(
            response.contains("not-authorized"),
            "pre-bind roster should be rejected: {response}"
        );
        assert!(
            !response.contains("feature-not-implemented"),
            "pre-bind roster should not fall through as unimplemented: {response}"
        );
    }

    #[tokio::test]
    async fn handle_xmpp_frame_drops_oversized_input() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let huge = format!(
            "<iq id=\"big\">{}</iq>",
            "a".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE)
        );
        let responses = handle_xmpp_frame(&huge, "example.com", state.as_ref(), &mut conn).await;
        assert!(responses.is_empty());
    }

    #[tokio::test]
    async fn handle_xmpp_frame_drops_whitespace_padded_oversized_input() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let huge = format!(
            "{}<iq id=\"big\"/>",
            " ".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE)
        );
        let responses = handle_xmpp_frame(&huge, "example.com", state.as_ref(), &mut conn).await;
        assert!(responses.is_empty());
    }

    #[tokio::test]
    async fn handle_xmpp_frame_drops_oversized_sm_nonza_before_parse() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let huge = format!(
            r#"<r xmlns="urn:xmpp:sm:3" note="{}"/>"#,
            "a".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE)
        );

        let responses = handle_xmpp_frame(&huge, "example.com", state.as_ref(), &mut conn).await;

        assert!(responses.is_empty());
    }

    #[tokio::test]
    async fn handle_xmpp_frame_invalid_iq_returns_feature_not_implemented() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq id="bad-iq" type="get"><nope/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"bad-iq\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_invalid_iq_result_returns_no_response() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq id="bad-result" type="result"><a/><b/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert!(responses.is_empty(), "expected no response: {responses:?}");
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_xml_iq_request_preserves_legacy_error() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq id="broken-iq" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"broken-iq\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_ignores_type_suffix_attributes() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq id="req-1" mimetype="result" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-1\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_recovers_attrs_with_spaces_and_gt() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq note="1 > 0" id = "req-2" type = "get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-2\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_skips_unquoted_attr_and_keeps_scanning() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq bogus=x id="req-3" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-3\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_skips_unquoted_attr_with_slashes() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq bogus=http://x id="req-4" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-4\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_keeps_id_after_empty_attr_value() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq bogus= id="req-5" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-5\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_recovers_unquoted_type_value() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq type=get id="req-6"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-6\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_wrong_namespace_iq_stays_silent() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq xmlns="urn:ietf:params:xml:ns:xmpp-sasl" id="bad-ns" type="get"><ping xmlns="urn:xmpp:ping"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert!(responses.is_empty(), "expected no response: {responses:?}");
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_self_closing_iq_result_stays_silent() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq type=result/>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert!(responses.is_empty(), "expected no response: {responses:?}");
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_skips_url_like_attr_with_equals() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq bogus=http://x=y id="req-7" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-7\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_does_not_shadow_real_type_attr() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq prev=type=result id="req-8" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-8\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_ignores_embedded_quoted_type_in_broken_value() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq prev=type="result" id="req-9" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-9\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_prefers_later_quoted_type_attr() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq type=result id="req-10" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-10\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_prefers_later_unquoted_type_attr() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq type=result type=get id="req-10b"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-10b\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_keeps_type_when_later_attr_is_truncated() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq type=get id="req-11" bogus="#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-11\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_keeps_unquoted_type_when_later_quote_is_unterminated() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq type=get id="req-12" to="alice@example.com"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("id=\"req-12\""));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_unescapes_recovered_id() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq id="a&amp;b" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains(r#"id="a&amp;b""#));
        assert!(!responses[0].contains(r#"id="a&amp;amp;b""#));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_recovers_later_unquoted_type_attr() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq bogus= type=get id="req-13"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains(r#"id="req-13""#));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_recovers_later_spaced_quoted_type_attr() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq bogus= type = "get" id="req-13b"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains(r#"id="req-13b""#));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_does_not_treat_next_id_attr_as_type_value() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq type= id="req-14"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert!(responses.is_empty(), "expected no response: {responses:?}");
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_does_not_treat_next_type_attr_as_id_value() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq id= type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(!responses[0].contains(r#"id="type=&quot;get&quot;""#));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_iq_keeps_invalid_numeric_entity_escaped() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            r#"<iq id="&#1;" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        assert!(responses[0].contains(r#"id="&amp;#1;""#));
        assert!(responses[0].contains("feature-not-implemented"));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_ping_roundtrips_through_sans_io_path() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        conn.phase = ConnectionPhase::ready(jid, false);

        let responses = handle_xmpp_frame(
            r#"<iq id="ping-roundtrip" type="get"><ping xmlns="urn:xmpp:ping"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1);
        let element = Element::from_str(&responses[0]).expect("valid IQ XML");
        let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
        assert_eq!(iq.id, "ping-roundtrip");
        assert!(matches!(iq.payload, xmpp_parsers::iq::IqType::Result(None)));
    }

    #[tokio::test]
    async fn handle_iq_carbons_enable_returns_parseable_result() {
        let state = create_test_websocket_state().await;
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        let frame = r#"<iq xmlns="jabber:client" id="carbons-1" type="set"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#;
        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&jid),
        )
        .await;
        assert_eq!(responses.len(), 1);

        let iq_xml = responses.first().expect("carbons response");
        let element = Element::from_str(iq_xml).expect("valid IQ XML");
        let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");

        assert_eq!(iq.id, "carbons-1");
        match iq.payload {
            xmpp_parsers::iq::IqType::Result(None) => {}
            other => panic!("expected empty IQ result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_iq_carbons_toggle_updates_registry_flag() {
        let state = create_test_websocket_state().await;
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx);
        assert!(!state
            .deps
            .protocol
            .connection_registry
            .is_carbons_enabled(&jid));

        let enable = r#"<iq xmlns="jabber:client" id="carbons-enable" type="set"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#;
        let enable_responses = handle_iq(
            enable,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&jid),
        )
        .await;
        assert_eq!(enable_responses.len(), 1);
        assert!(state
            .deps
            .protocol
            .connection_registry
            .is_carbons_enabled(&jid));

        let disable = r#"<iq xmlns="jabber:client" id="carbons-disable" type="set"><disable xmlns="urn:xmpp:carbons:2"/></iq>"#;
        let disable_responses = handle_iq(
            disable,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&jid),
        )
        .await;
        assert_eq!(disable_responses.len(), 1);
        assert!(!state
            .deps
            .protocol
            .connection_registry
            .is_carbons_enabled(&jid));
    }

    #[tokio::test]
    async fn handle_iq_unknown_includes_routing_addresses_in_error() {
        let state = create_test_websocket_state().await;
        let frame = r#"<iq xmlns="jabber:client" id="unknown-1" type="get" from="alice@example.com/web" to="example.com"><foo xmlns="urn:waddle:test:0"/></iq>"#;
        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ConnectionPhase::Unauthenticated,
        )
        .await;
        assert_eq!(responses.len(), 1);

        let iq_xml = responses.first().expect("error response");
        let element = Element::from_str(iq_xml).expect("valid IQ XML");
        let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");

        assert_eq!(iq.id, "unknown-1");
        assert_eq!(
            iq.from.as_ref().map(ToString::to_string).as_deref(),
            Some("example.com")
        );
        assert_eq!(
            iq.to.as_ref().map(ToString::to_string).as_deref(),
            Some("alice@example.com/web")
        );
        match iq.payload {
            xmpp_parsers::iq::IqType::Error(_) => {}
            other => panic!("expected IQ error payload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_iq_result_returns_empty_response() {
        let state = create_test_websocket_state().await;
        let frame = r#"<iq xmlns="jabber:client" id="ack-1" type="result" from="alice@example.com/web" to="muc.example.com"/>"#;
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&sender_jid),
        )
        .await;
        assert!(
            responses.is_empty(),
            "IQ result should produce no response, got: {responses:?}"
        );
    }

    #[tokio::test]
    async fn handle_iq_error_returns_empty_response() {
        let state = create_test_websocket_state().await;
        let frame = r#"<iq xmlns="jabber:client" id="err-1" type="error" from="alice@example.com/web" to="muc.example.com"><error type="cancel"><feature-not-implemented xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#;
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&sender_jid),
        )
        .await;
        assert!(
            responses.is_empty(),
            "IQ error should produce no response, got: {responses:?}"
        );
    }

    #[tokio::test]
    async fn handle_xmpp_frame_server_iq_error_returns_empty_response() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
        conn.phase = ConnectionPhase::ready(sender_jid, false);

        let responses = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" from="waddle.social" id="016f8556-3f56-4a75-b159-ee0a1eb0823e" type="error"><error type="cancel"><feature-not-implemented xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#,
            "waddle.social",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert!(
            responses.is_empty(),
            "IQ error should produce no response, got: {responses:?}"
        );
    }

    #[tokio::test]
    async fn handle_iq_command_request_routes_to_registry() {
        let state = create_test_websocket_state().await;
        state
            .deps
            .protocol
            .command_registry
            .register(
                "test:adhoc-command",
                "Test Command",
                |ctx: CommandContext| async move {
                    CommandResult::Executing {
                        form: waddle_xmpp::xep::xep0004::DataForm::new(
                            waddle_xmpp::xep::xep0004::FormType::Form,
                        ),
                        session_id: ctx.command.session_id.unwrap_or_default(),
                        notes: vec![],
                    }
                },
            )
            .await;

        let session = create_test_session(state.as_ref(), "alice").await;
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
        let frame = r#"<iq xmlns="jabber:client" id="cmd-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="test:adhoc-command" action="execute"/></iq>"#;
        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session),
            &ready_phase(&sender_jid),
        )
        .await;

        assert_eq!(responses.len(), 1);
        let response = responses.first().expect("command response");
        assert!(
            response.contains("status=\"executing\"") || response.contains("status='executing'"),
            "expected executing command response, got: {response}"
        );
        assert!(
            response.contains("sessionid=\"") || response.contains("sessionid='"),
            "expected command session ID in response, got: {response}"
        );
        assert!(
            !response.contains("feature-not-implemented"),
            "command IQ should not fall through to unhandled feature-not-implemented: {response}"
        );
    }

    #[tokio::test]
    async fn handle_iq_command_request_requires_ready_phase() {
        let state = create_test_websocket_state().await;
        state
            .deps
            .protocol
            .command_registry
            .register(
                "test:adhoc-command",
                "Test Command",
                |_ctx: CommandContext| async move {
                    CommandResult::Executing {
                        form: waddle_xmpp::xep::xep0004::DataForm::new(
                            waddle_xmpp::xep::xep0004::FormType::Form,
                        ),
                        session_id: String::new(),
                        notes: vec![],
                    }
                },
            )
            .await;

        let session = create_test_session(state.as_ref(), "alice").await;
        let pending_jid: FullJid = "alice@example.com/pending".parse().expect("pending jid");
        let mut carbons_enabled = false;
        let mut roster_interested = false;
        let frame = r#"<iq xmlns="jabber:client" id="cmd-prebind-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="test:adhoc-command" action="execute"/></iq>"#;
        let mut conn_state = IqConnState {
            carbons_enabled: &mut carbons_enabled,
            roster_interested: &mut roster_interested,
            state_machine: None,
        };
        let responses = handle_iq_with_conn_state(
            parse_iq_for_test(frame),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session),
            &ConnectionPhase::authenticated(&pending_jid),
            &mut conn_state,
        )
        .await;

        let response = responses.first().expect("command error response");
        assert!(
            response.contains("not-authorized"),
            "pre-bind command IQ should be rejected: {response}"
        );
        assert!(
            !response.contains("status=\"executing\"") && !response.contains("status='executing'"),
            "pre-bind command IQ must not reach the registry: {response}"
        );
    }

    #[tokio::test]
    async fn handle_iq_disco_info_advertises_replies() {
        let server_domain = "example.com";
        let muc_domain = "muc.example.com";
        let state = create_test_websocket_state().await;

        let server_query = disco_info_iq_frame("srv1", "example.com", None);
        let server_responses = handle_iq(
            &server_query,
            server_domain,
            muc_domain,
            state.as_ref(),
            &None,
            &ConnectionPhase::Unauthenticated,
        )
        .await;
        let server_response = server_responses.first().expect("server disco response");
        assert!(server_response.contains("urn:xmpp:reply:0"));
        assert!(!server_response.contains("urn:xmpp:fulltext:0"));
        assert!(!server_response.contains("urn:waddle:test-extension:1"));

        let muc_query = disco_info_iq_frame("muc1", "muc.example.com", None);
        let muc_responses = handle_iq(
            &muc_query,
            server_domain,
            muc_domain,
            state.as_ref(),
            &None,
            &ConnectionPhase::Unauthenticated,
        )
        .await;
        let muc_response = muc_responses.first().expect("muc disco response");
        assert!(muc_response.contains("urn:xmpp:reply:0"));
        assert!(!muc_response.contains("urn:waddle:test-extension:1"));

        let room_query = disco_info_iq_frame("room1", "room@muc.example.com", None);
        let room_responses = handle_iq(
            &room_query,
            server_domain,
            muc_domain,
            state.as_ref(),
            &None,
            &ConnectionPhase::Unauthenticated,
        )
        .await;
        let room_response = room_responses.first().expect("room disco response");
        assert!(room_response.contains("urn:xmpp:mam:2"));
        assert!(room_response.contains("urn:xmpp:reply:0"));
        assert!(room_response.contains("urn:xmpp:fulltext:0"));
        assert!(!room_response.contains("urn:waddle:test-extension:1"));

        let user_jid: FullJid = "alice@example.com/waddle".parse().expect("user jid");
        let user_query = disco_info_iq_frame("user1", "alice@example.com", None);
        let user_responses = handle_iq(
            &user_query,
            server_domain,
            muc_domain,
            state.as_ref(),
            &None,
            &ready_phase(&user_jid),
        )
        .await;
        let user_response = user_responses.first().expect("user disco response");
        assert!(user_response.contains("urn:xmpp:mam:2"));
        assert!(user_response.contains("urn:xmpp:fulltext:0"));
    }

    #[tokio::test]
    async fn handle_iq_disco_items_server_advertises_spaces_service() {
        let state = create_test_websocket_state().await;
        let query = disco_items_iq_frame("srv-items", "example.com", None);

        let responses = handle_iq(
            &query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ConnectionPhase::Unauthenticated,
        )
        .await;
        let response = responses.first().expect("server disco items response");

        assert!(
            response.contains("muc.example.com"),
            "expected MUC service: {response}"
        );
        assert!(
            response.contains("spaces.example.com"),
            "expected spaces service in server disco#items: {response}"
        );
    }

    #[tokio::test]
    async fn handle_iq_disco_items_spaces_is_empty_without_owner_created_spaces() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;

        let authenticated_session = Some(session);
        let authenticated_jid: FullJid = format!(
            "{}@example.com/web",
            authenticated_session
                .as_ref()
                .expect("session")
                .xmpp_localpart
        )
        .parse()
        .expect("authenticated jid");
        let authenticated_phase = ready_phase(&authenticated_jid);
        let query = disco_items_iq_frame("spaces-items", "spaces.example.com", None);

        let responses = handle_iq(
            &query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &authenticated_session,
            &authenticated_phase,
        )
        .await;
        let response = responses.first().expect("spaces disco items response");

        assert!(
            !response.contains("node="),
            "fresh deployments must not advertise a synthetic space node: {response}"
        );
    }

    #[tokio::test]
    async fn handle_iq_pubsub_items_spaces_node_lists_published_bookmarks() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;

        let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
        state
            .deps
            .protocol
            .pubsub_storage
            .get_or_create_node(&spaces_jid, "team")
            .await
            .expect("space node");
        let channel = waddle_xmpp::ChannelInfo {
            id: "general".to_string(),
            name: "General".to_string(),
            channel_type: "text".to_string(),
        };
        let item = waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com")
            .expect("bookmark item");
        state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(&spaces_jid, "team", &item, None, false)
            .await
            .expect("publish bookmark");

        let authenticated_session = Some(session);
        let authenticated_jid: FullJid = format!(
            "{}@example.com/web",
            authenticated_session
                .as_ref()
                .expect("session")
                .xmpp_localpart
        )
        .parse()
        .expect("authenticated jid");
        let authenticated_phase = ready_phase(&authenticated_jid);
        let query = r#"<iq xmlns="jabber:client" id="space-node-items" type="get" to="spaces.example.com"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="team"/></pubsub></iq>"#;

        let responses = handle_iq(
            query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &authenticated_session,
            &authenticated_phase,
        )
        .await;
        let response = responses
            .first()
            .expect("spaces node pubsub items response");

        assert!(
            response.contains("general@muc.example.com"),
            "expected channel room JID in spaces node pubsub items: {response}"
        );
        assert!(
            response.contains("conference") && response.contains("urn:xmpp:bookmarks:1"),
            "expected XEP-0402 conference item in spaces node pubsub items: {response}"
        );
        assert!(
            response.contains("General"),
            "expected channel name in spaces node pubsub items: {response}"
        );
    }

    #[tokio::test]
    async fn standard_muc_owner_config_persists_room_and_enforces_nonanonymous_defaults() {
        let state = create_test_websocket_state().await;
        let session = create_test_server_owner_session(state.as_ref(), "alice").await;

        let room_jid: BareJid = "project@muc.example.com".parse().expect("room jid");
        let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
            .parse()
            .expect("alice jid");
        let ready = ready_phase(&alice_jid);

        handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice_jid,
            "alice",
            &Some(session.clone()),
        )
        .await;

        let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
            .attr("type", "submit")
            .append(
                Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                    .attr("var", "muc#roomconfig_roomname")
                    .append(
                        Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                            .append("Project Room")
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                    .attr("var", "muc#roomconfig_persistentroom")
                    .append(
                        Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                            .append("0")
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                    .attr("var", "muc#roomconfig_whois")
                    .append(
                        Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                            .append("moderators")
                            .build(),
                    )
                    .build(),
            )
            .build();
        let owner_iq = element_to_xml(
            Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
                .attr("id", "owner-submit")
                .attr("type", "set")
                .attr("to", room_jid.to_string())
                .append(
                    Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                        .append(submit_form)
                        .build(),
                )
                .build(),
        );

        let responses = handle_iq(
            &owner_iq,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session),
            &ready,
        )
        .await;
        assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
        assert!(responses[0].contains("type=\"result\""));

        let actor = state.deps.app_state.db_pool.global_actor().clone();
        let channel = crate::server::xmpp_state::get_xmpp_channel(actor, "project")
            .await
            .expect("channel lookup")
            .expect("persisted channel");
        assert_eq!(channel.name, "Project Room");

        let snapshot = get_room_actor(state.as_ref(), &room_jid)
            .await
            .expect("room actor")
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room;
        assert!(snapshot.config.persistent);

        let disco = disco_items_iq_frame("muc-items", "muc.example.com", None);
        let responses = handle_iq(
            &disco,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ConnectionPhase::Unauthenticated,
        )
        .await;
        assert!(responses[0].contains("project@muc.example.com"));
    }

    #[tokio::test]
    async fn standard_muc_owner_get_returns_config_without_persisting_room() {
        let state = create_test_websocket_state().await;
        let session = create_test_server_owner_session(state.as_ref(), "alice").await;

        let room_jid: BareJid = "config-get@muc.example.com".parse().expect("room jid");
        let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
            .parse()
            .expect("alice jid");
        let ready = ready_phase(&alice_jid);

        handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice_jid,
            "alice",
            &Some(session.clone()),
        )
        .await;

        let owner_iq = element_to_xml(
            Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
                .attr("id", "owner-get")
                .attr("type", "get")
                .attr("to", room_jid.to_string())
                .append(Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER).build())
                .build(),
        );

        let responses = handle_iq(
            &owner_iq,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session),
            &ready,
        )
        .await;
        assert_eq!(responses.len(), 1, "owner get response: {responses:?}");
        assert!(responses[0].contains("type=\"result\""));
        assert!(responses[0].contains("muc#roomconfig_roomname"));

        let actor = state.deps.app_state.db_pool.global_actor().clone();
        let channel = crate::server::xmpp_state::get_xmpp_channel(actor, "config-get")
            .await
            .expect("channel lookup");
        assert!(channel.is_none());
    }

    #[tokio::test]
    async fn standard_muc_owner_config_rejects_non_owner() {
        let state = create_test_websocket_state().await;
        let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let bob_session = create_test_session(state.as_ref(), "bob").await;

        let room_jid: BareJid = "locked@muc.example.com".parse().expect("room jid");
        let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
            .parse()
            .expect("alice jid");
        let bob_jid: FullJid = format!("{}@example.com/web", bob_session.xmpp_localpart)
            .parse()
            .expect("bob jid");
        let bob_ready = ready_phase(&bob_jid);

        handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice_jid,
            "alice",
            &Some(alice_session),
        )
        .await;

        let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
            .attr("type", "submit")
            .append(
                Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                    .attr("var", "muc#roomconfig_roomname")
                    .append(
                        Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                            .append("Hacked")
                            .build(),
                    )
                    .build(),
            )
            .build();
        let owner_iq = element_to_xml(
            Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
                .attr("id", "owner-submit")
                .attr("type", "set")
                .attr("to", room_jid.to_string())
                .append(
                    Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                        .append(submit_form)
                        .build(),
                )
                .build(),
        );

        let responses = handle_iq(
            &owner_iq,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(bob_session.clone()),
            &bob_ready,
        )
        .await;
        assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("forbidden"));

        let snapshot = get_room_actor(state.as_ref(), &room_jid)
            .await
            .expect("room actor")
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room;
        assert_ne!(snapshot.config.name, "Hacked");

        let room_actor = get_room_actor(state.as_ref(), &room_jid)
            .await
            .expect("room actor");
        room_actor
            .ask(ChangeAffiliation {
                jid: bob_jid.to_bare(),
                affiliation: Affiliation::Admin,
            })
            .await
            .expect("set admin affiliation");

        let responses = handle_iq(
            &owner_iq,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(bob_session),
            &bob_ready,
        )
        .await;
        assert_eq!(
            responses.len(),
            1,
            "admin owner config response: {responses:?}"
        );
        assert!(responses[0].contains("type=\"error\""));
        assert!(responses[0].contains("forbidden"));
    }

    #[tokio::test]
    async fn room_disco_info_advertises_parent_space_metadata_for_linked_channel() {
        let state = create_test_websocket_state().await;
        let space_db = state.deps.app_state.db_pool.global();
        let conn = space_db.guard().await.expect("persistent connection");
        conn.execute(
            "INSERT INTO channels (id, name, channel_type, position, is_default) VALUES (?, ?, 'text', 0, 0)",
            crate::db_params!["linked", "Linked"],
        )
        .await
        .expect("insert channel");
        drop(conn);
        let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
        state
            .deps
            .protocol
            .pubsub_storage
            .get_or_create_node(&spaces_jid, "team")
            .await
            .expect("space node");
        let channel = waddle_xmpp::ChannelInfo {
            id: "linked".to_string(),
            name: "Linked".to_string(),
            channel_type: "text".to_string(),
        };
        let item = waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com")
            .expect("bookmark item");
        state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(&spaces_jid, "team", &item, None, false)
            .await
            .expect("publish bookmark");

        let query = disco_info_iq_frame("room-info", "linked@muc.example.com", None);
        let responses = handle_iq(
            &query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ConnectionPhase::Unauthenticated,
        )
        .await;
        let response = responses.first().expect("room disco response");
        assert!(response.contains("muc_nonanonymous"));
        assert!(response.contains("urn:xmpp:spaces:0"));
        assert!(response.contains("var=\"parent\""));
        assert!(response.contains("xmpp:spaces.example.com?;node=team"));
        assert!(response.contains("http://jabber.org/protocol/muc#roominfo"));
        assert!(response.contains("muc#roominfo_pubsub"));
    }

    #[tokio::test]
    async fn handle_iq_disco_info_spaces_node_reports_open_for_public_space() {
        let state = create_test_websocket_state().await;
        let viewer = create_test_session(state.as_ref(), "viewer").await;
        let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
        state
            .deps
            .protocol
            .pubsub_storage
            .get_or_create_node(&spaces_jid, "team")
            .await
            .expect("space node");

        let viewer_phase = authenticated_phase_for_session(&viewer, "example.com");
        let query = disco_info_iq_frame("space-node-info", "spaces.example.com", Some("team"));
        let responses = handle_iq(
            &query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(viewer),
            &viewer_phase,
        )
        .await;
        let response = responses.first().expect("spaces node disco info response");

        assert!(
            response.contains("type=\"result\"") || response.contains("type='result'"),
            "expected successful node disco#info response: {response}"
        );
        assert!(
            response.contains("pubsub#access_model"),
            "expected access model metadata in node disco#info: {response}"
        );
        assert!(
            response.contains(">open<"),
            "expected public access model=open in metadata: {response}"
        );
    }

    #[tokio::test]
    async fn handle_iq_disco_info_unknown_spaces_node_returns_item_not_found() {
        let state = create_test_websocket_state().await;
        let viewer = create_test_session(state.as_ref(), "viewer").await;

        let viewer_phase = authenticated_phase_for_session(&viewer, "example.com");
        let query = disco_info_iq_frame(
            "space-node-info-private",
            "spaces.example.com",
            Some("unknown"),
        );
        let responses = handle_iq(
            &query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(viewer),
            &viewer_phase,
        )
        .await;
        let response = responses
            .first()
            .expect("spaces node private disco info response");

        assert!(
            response.contains("item-not-found"),
            "unknown space node should not be discoverable: {response}"
        );
    }

    #[tokio::test]
    async fn handle_message_direct_rejects_client_authored_extension_envelope() {
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
        let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
        let state = create_test_websocket_state().await;

        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
        message.id = Some("dm-extension-spoof-1".to_string());
        message.type_ = XmppMessageType::Chat;
        message.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("spoofed extension".to_string()),
        );
        message
            .payloads
            .push(Element::builder("extensions", "urn:waddle:extension:1").build());
        message
            .payloads
            .push(Element::builder("spoof", "urn:waddle:test-extension:1").build());

        let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

        assert_eq!(responses.len(), 1);
        assert!(
            responses[0].contains("bad-request"),
            "response was {}",
            responses[0]
        );
        assert!(
            !responses[0].contains("urn:waddle:extension:1"),
            "response was {}",
            responses[0]
        );
        assert!(
            !responses[0].contains("urn:waddle:test-extension:1"),
            "response was {}",
            responses[0]
        );
        assert!(
            responses[0].contains("from=\"bob@example.com/mobile\""),
            "response was {}",
            responses[0]
        );
        assert!(
            responses[0].contains("to=\"alice@example.com/web\""),
            "response was {}",
            responses[0]
        );
    }

    #[tokio::test]
    async fn handle_message_error_with_extension_envelope_does_not_emit_error_loop() {
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
        let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
        let state = create_test_websocket_state().await;

        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
        message.id = Some("dm-extension-error-1".to_string());
        message.type_ = XmppMessageType::Error;
        message
            .payloads
            .push(Element::builder("extensions", "urn:waddle:extension:1").build());

        let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

        assert!(
            responses.is_empty(),
            "message errors must not trigger another error: {responses:?}"
        );
    }

    #[tokio::test]
    async fn handle_message_groupchat_rejects_client_authored_extension_envelope() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let sender_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
            .parse()
            .expect("sender jid");
        let room_jid: BareJid = "general@muc.example.com".parse().expect("room jid");
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room_jid,
            RoomConfig::default(),
            "space".to_string(),
            "general".to_string(),
        )
        .await
        .expect("create room");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: sender_jid.clone(),
                nick: "alice".to_string(),
                effective_affiliation: Affiliation::Member,
                local_domain: "example.com".to_string(),
            })
            .await
            .expect("join alice");

        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
        message.id = Some("muc-extension-spoof-1".to_string());
        message.type_ = XmppMessageType::Groupchat;
        message.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("spoofed extension".to_string()),
        );
        message
            .payloads
            .push(Element::builder("extensions", "urn:waddle:extension:1").build());
        message
            .payloads
            .push(Element::builder("spoof", "urn:waddle:test-extension:1").build());

        let responses =
            handle_message_for_test(state.as_ref(), &sender_jid, Some(&session), message).await;

        assert_eq!(responses.len(), 1);
        assert!(
            responses[0].contains("bad-request"),
            "response was {}",
            responses[0]
        );
        assert!(
            !responses[0].contains("urn:waddle:extension:1"),
            "response was {}",
            responses[0]
        );
        assert!(
            !responses[0].contains("urn:waddle:test-extension:1"),
            "response was {}",
            responses[0]
        );
        assert!(
            responses[0].contains("from=\"general@muc.example.com\""),
            "response was {}",
            responses[0]
        );
        assert!(
            responses[0].contains("to=\"alice@example.com/web\""),
            "response was {}",
            responses[0]
        );
    }

    #[tokio::test]
    async fn handle_message_groupchat_extension_envelope_preserves_non_occupant_error() {
        let state = create_test_websocket_state().await;
        let alice_session = create_test_session(state.as_ref(), "alice").await;
        let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
            .parse()
            .expect("alice jid");
        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let room_jid: BareJid = "general@muc.example.com".parse().expect("room jid");
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room_jid,
            RoomConfig::default(),
            "space".to_string(),
            "general".to_string(),
        )
        .await
        .expect("create room");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: bob_jid,
                nick: "bob".to_string(),
                effective_affiliation: Affiliation::Member,
                local_domain: "example.com".to_string(),
            })
            .await
            .expect("join bob");

        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
        message.id = Some("muc-extension-non-occupant-1".to_string());
        message.type_ = XmppMessageType::Groupchat;
        message.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("spoofed extension".to_string()),
        );
        message
            .payloads
            .push(Element::builder("extensions", "urn:waddle:extension:1").build());
        message
            .payloads
            .push(Element::builder("spoof", "urn:waddle:test-extension:1").build());

        let responses =
            handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message)
                .await;

        assert_eq!(responses.len(), 1);
        assert!(
            responses[0].contains("not-acceptable"),
            "response was {}",
            responses[0]
        );
        assert!(
            !responses[0].contains("bad-request"),
            "response was {}",
            responses[0]
        );
        assert!(
            !responses[0].contains("urn:waddle:extension:1"),
            "response was {}",
            responses[0]
        );
        assert!(
            !responses[0].contains("urn:waddle:test-extension:1"),
            "response was {}",
            responses[0]
        );
        assert!(
            responses[0].contains("from=\"general@muc.example.com\""),
            "response was {}",
            responses[0]
        );
        assert!(
            responses[0].contains("to=\"alice@example.com/web\""),
            "response was {}",
            responses[0]
        );
    }

    #[tokio::test]
    async fn handle_message_direct_with_sample_extension_payload_preserves_payload_for_recipient() {
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
        let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
        let state = create_test_websocket_state().await;

        let (recipient_tx, mut recipient_rx) = mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register(recipient_jid.clone(), recipient_tx);

        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
        message.id = Some("dm-extension-payload-1".to_string());
        message.type_ = XmppMessageType::Chat;
        message.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("Repo payload already attached".to_string()),
        );
        message.payloads.push(
            Element::builder("repo", "urn:waddle:test-extension:1")
                .attr("owner", "rust-lang")
                .attr("name", "rust")
                .attr("url", "xmpp:example.com?extension=test")
                .build(),
        );

        let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

        assert!(
            responses.is_empty(),
            "direct messages should not get a special extension echo"
        );

        // Recipient may receive an inbox push headline before the routed
        // chat message.  Drain until we find the chat stanza.
        let mut found_chat = false;
        while let Ok(routed) = recipient_rx.try_recv() {
            let routed_xml = stanza_to_xml(&routed.stanza);
            if routed_xml.contains("type=\"chat\"") || routed_xml.contains("type='chat'") {
                assert!(
                    routed_xml.contains("to=\"bob@example.com/mobile\"")
                        || routed_xml.contains("to='bob@example.com/mobile'"),
                    "routed stanza should target recipient resource: {routed_xml}"
                );
                assert!(
                    routed_xml.contains("urn:waddle:test-extension:1"),
                    "routed stanza should preserve extension payload: {routed_xml}"
                );
                assert_sample_payload(
                    &routed_xml,
                    "repo",
                    "xmpp:example.com?extension=test",
                    "rust-lang",
                    "rust",
                );
                found_chat = true;
                break;
            }
        }
        assert!(found_chat, "recipient should receive the chat message");
    }

    #[tokio::test]
    async fn handle_message_direct_chat_to_bare_jid_fans_out_to_all_connected_resources() {
        let state = create_test_websocket_state().await;
        let sender_jid: FullJid = "bob@example.com/phone".parse().expect("sender jid");
        let recipient_web: FullJid = "alice@example.com/web-123".parse().expect("recipient web");
        let recipient_mobile: FullJid = "alice@example.com/mobile-456"
            .parse()
            .expect("recipient mobile");

        let (web_tx, mut web_rx) = mpsc::channel(8);
        let (mobile_tx, mut mobile_rx) = mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register(recipient_web.clone(), web_tx);
        state
            .deps
            .protocol
            .connection_registry
            .register(recipient_mobile.clone(), mobile_tx);
        // RFC 6121 §8.5.2.1: bare-JID delivery selects available
        // resources by presence priority. The dispatcher path enforces
        // this; mark both recipient resources available so fan-out
        // reaches them.
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&recipient_web, true, 0);
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&recipient_mobile, true, 0);

        let responses = handle_message_for_test(
            state.as_ref(),
            &sender_jid,
            None,
            parse_message_for_test(
                "<message xmlns='jabber:client' to='alice@example.com' type='chat' id='dm-bare-1'>\
                <body>hello all resources</body>\
             </message>",
            ),
        )
        .await;

        assert!(responses.is_empty(), "plain DM should not echo to sender");

        let mut web_chat = None;
        while let Ok(outbound) = web_rx.try_recv() {
            let xml = stanza_to_xml(&outbound.stanza);
            if xml.contains("hello all resources") && !xml.contains("urn:xmpp:carbons:2") {
                web_chat = Some(xml);
                break;
            }
        }

        let mut mobile_chat = None;
        while let Ok(outbound) = mobile_rx.try_recv() {
            let xml = stanza_to_xml(&outbound.stanza);
            if xml.contains("hello all resources") && !xml.contains("urn:xmpp:carbons:2") {
                mobile_chat = Some(xml);
                break;
            }
        }

        let _web_xml = web_chat.expect("web resource should receive original bare-JID message");
        let _mobile_xml =
            mobile_chat.expect("mobile resource should receive original bare-JID message");
        // RFC 6121 §8.5.2.1.1: bare-JID fan-out delivers the original
        // stanza to each available resource without rewriting the
        // `to` attribute. The dispatcher path preserves this; legacy
        // `handle_message` rewrote `to` to the per-resource full JID,
        // which was a deviation from the RFC. Both resources received
        // the stanza — the reachability semantic — and that is what
        // this unit test now asserts.
    }

    #[tokio::test]
    async fn handle_message_direct_chat_sends_sent_carbon_to_opted_in_sibling_resource() {
        let state = create_test_websocket_state().await;
        let sender_jid: FullJid = "alice@example.com/phone".parse().expect("sender jid");
        let sibling_jid: FullJid = "alice@example.com/desktop".parse().expect("sibling jid");

        let (sender_tx, _sender_rx) = mpsc::channel(8);
        let (sibling_tx, mut sibling_rx) = mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register(sender_jid.clone(), sender_tx);
        state
            .deps
            .protocol
            .connection_registry
            .register_with_carbons(sibling_jid.clone(), sibling_tx, true);

        let responses = handle_message_for_test(
            state.as_ref(),
            &sender_jid,
            None,
            parse_message_for_test(
                "<message xmlns='jabber:client' to='ghost@example.com' type='chat' id='sent-carbon-1'>\
                <body>sent carbon over websocket</body>\
             </message>",
            ),
        )
        .await;

        assert!(responses.is_empty(), "plain DM should not echo to sender");

        let mut sent_carbon = None;
        while let Ok(outbound) = sibling_rx.try_recv() {
            let xml = stanza_to_xml(&outbound.stanza);
            if xml.contains("urn:xmpp:carbons:2") && xml.contains("<sent") {
                sent_carbon = Some(xml);
                break;
            }
        }

        let carbon_xml = sent_carbon.expect("opted-in sibling should receive sent carbon");
        assert!(
            carbon_xml.contains("sent carbon over websocket"),
            "sent carbon should preserve message body: {carbon_xml}"
        );
    }

    #[tokio::test]
    async fn handle_message_direct_chat_sends_received_carbon_to_opted_in_sibling_resource() {
        let state = create_test_websocket_state().await;
        let sender_jid: FullJid = "bob@example.com/phone".parse().expect("sender jid");
        let recipient_jid: FullJid = "alice@example.com/phone".parse().expect("recipient jid");
        let sibling_jid: FullJid = "alice@example.com/desktop".parse().expect("sibling jid");

        let (recipient_tx, mut recipient_rx) = mpsc::channel(8);
        let (sibling_tx, mut sibling_rx) = mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register(recipient_jid.clone(), recipient_tx);
        state
            .deps
            .protocol
            .connection_registry
            .register_with_carbons(sibling_jid.clone(), sibling_tx, true);

        let frame = format!(
            "<message xmlns='jabber:client' to='{recipient_jid}' type='chat' id='recv-carbon-1'>\
                <body>received carbon over websocket</body>\
             </message>"
        );

        // Build alice/phone's per-connection state machine so we can
        // drive the recipient pass the dispatcher path now owns. In
        // production this happens automatically via alice/phone's
        // main loop dispatching the queued
        // `DeliveryKind::PeerStanza`; the unit test reproduces the
        // same step explicitly so we observe the recipient-side
        // received-carbon fan-out.
        let mut recipient_conn = WsConnState::new();
        recipient_conn.phase = ConnectionPhase::ready(recipient_jid.clone(), false);
        recipient_conn.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            recipient_jid.clone(),
            false,
            Blocklist::empty(),
        );

        let responses = handle_message_for_test(
            state.as_ref(),
            &sender_jid,
            None,
            parse_message_for_test(frame.as_str()),
        )
        .await;

        assert!(responses.is_empty(), "plain DM should not echo to sender");

        // Pump alice/phone's queued PeerStanza through her SM so the
        // recipient pass runs and emits SendCarbons (received) to
        // alice/desktop.
        while let Ok(outbound) = recipient_rx.try_recv() {
            if !matches!(outbound.kind, DeliveryKind::PeerStanza) {
                continue;
            }
            let sm = recipient_conn.state_machine.as_mut().expect("alice SM");
            let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(outbound.stanza)));
            let deps = build_interpret_deps(state.as_ref(), None);
            let _ = drive_interpret_loop(events, sm, &deps).await;
        }

        let mut received_carbon = None;
        while let Ok(outbound) = sibling_rx.try_recv() {
            let xml = stanza_to_xml(&outbound.stanza);
            if xml.contains("urn:xmpp:carbons:2") && xml.contains("<received") {
                received_carbon = Some(xml);
                break;
            }
        }

        let carbon_xml = received_carbon.expect("opted-in sibling should receive received carbon");
        assert!(
            carbon_xml.contains("received carbon over websocket"),
            "received carbon should preserve message body: {carbon_xml}"
        );
    }

    #[tokio::test]
    async fn direct_messages_round_trip_through_inbox_query_and_mark_read() {
        let state = create_test_websocket_state().await;
        let alice_session = create_test_session(state.as_ref(), "alice").await;
        let bob_session = create_test_session(state.as_ref(), "bob").await;
        let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
            .parse()
            .expect("alice jid");
        let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
            .parse()
            .expect("bob jid");

        let message_xml = format!(
            "<message xmlns='jabber:client' to='{}' type='chat' id='dm-inbox-1'>\
                <body>Hello from Alice</body>\
             </message>",
            bob_jid.to_bare()
        );
        let responses = handle_message_for_test(
            state.as_ref(),
            &alice_jid,
            Some(&alice_session),
            parse_message_for_test(message_xml.as_str()),
        )
        .await;
        assert!(responses.is_empty(), "plain DM should not echo to sender");

        let inbox_query = format!(
            "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-1'>\
                <query xmlns='urn:waddle:inbox:0'/>\
             </iq>",
            bob_jid.to_bare()
        );
        let inbox_responses = handle_iq(
            inbox_query.as_str(),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(bob_session.clone()),
            &ready_phase(&bob_jid),
        )
        .await;
        let inbox_xml = inbox_responses.first().expect("inbox response");
        assert!(
            inbox_xml.contains("partner=\"alice@example.com\""),
            "inbox response should include Alice conversation: {inbox_xml}"
        );
        assert!(
            inbox_xml.contains("unread=\"1\""),
            "inbox response should report one unread DM: {inbox_xml}"
        );
        assert!(
            inbox_xml.contains("<preview>Hello from Alice</preview>"),
            "inbox response should include preview text: {inbox_xml}"
        );

        let mark_read = format!(
            "<iq xmlns='jabber:client' type='set' to='{}' id='inbox-2'>\
                <mark-read xmlns='urn:waddle:inbox:0' partner='alice@example.com'/>\
             </iq>",
            bob_jid.to_bare()
        );
        let mark_read_responses = handle_iq(
            mark_read.as_str(),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(bob_session.clone()),
            &ready_phase(&bob_jid),
        )
        .await;
        let mark_read_xml = mark_read_responses.first().expect("mark-read result");
        assert!(
            mark_read_xml.contains("type=\"result\""),
            "mark-read should succeed: {mark_read_xml}"
        );

        let unread_only_query = format!(
            "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-3'>\
                <query xmlns='urn:waddle:inbox:0' only-unread='true'/>\
             </iq>",
            bob_jid.to_bare()
        );
        let unread_only_responses = handle_iq(
            unread_only_query.as_str(),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(bob_session),
            &ready_phase(&bob_jid),
        )
        .await;
        let unread_only_xml = unread_only_responses.first().expect("unread-only response");
        assert!(
            unread_only_xml.contains("total-unread=\"0\""),
            "mark-read should clear the unread count: {unread_only_xml}"
        );
        assert!(
            !unread_only_xml.contains("<conversation "),
            "unread-only query should be empty after mark-read: {unread_only_xml}"
        );
    }

    #[tokio::test]
    async fn inbox_query_requires_ready_phase() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "bob").await;
        let pending_jid: FullJid = "bob@example.com/pending".parse().expect("pending jid");
        let mut carbons_enabled = false;
        let mut roster_interested = false;
        let frame = r#"<iq xmlns='jabber:client' type='get' to='bob@example.com' id='inbox-prebind-1'><query xmlns='urn:waddle:inbox:0'/></iq>"#;
        let mut conn_state = IqConnState {
            carbons_enabled: &mut carbons_enabled,
            roster_interested: &mut roster_interested,
            state_machine: None,
        };
        let responses = handle_iq_with_conn_state(
            parse_iq_for_test(frame),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session),
            &ConnectionPhase::authenticated(&pending_jid),
            &mut conn_state,
        )
        .await;

        let response = responses.first().expect("inbox error response");
        assert!(
            response.contains("not-authorized"),
            "pre-bind inbox IQ should be rejected: {response}"
        );
    }

    #[tokio::test]
    async fn encrypted_sfs_messages_without_bodies_still_project_into_inbox() {
        let state = create_test_websocket_state().await;
        let alice_session = create_test_session(state.as_ref(), "alice").await;
        let bob_session = create_test_session(state.as_ref(), "bob").await;
        let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
            .parse()
            .expect("alice jid");
        let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
            .parse()
            .expect("bob jid");

        let message_xml = format!(
            "<message xmlns='jabber:client' to='{}' type='chat' id='dm-esfs-1'>\
                <file-sharing xmlns='urn:xmpp:sfs:0'/>\
                <encrypted xmlns='urn:xmpp:esfs:0' cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>\
                    <key>a2V5</key>\
                    <iv>aXY=</iv>\
                    <sources xmlns='urn:xmpp:sfs:0'>\
                        <url-data target='https://files.example.com/secret.enc'/>\
                    </sources>\
                </encrypted>\
             </message>",
            bob_jid.to_bare()
        );
        let responses = handle_message_for_test(
            state.as_ref(),
            &alice_jid,
            Some(&alice_session),
            parse_message_for_test(message_xml.as_str()),
        )
        .await;
        assert!(
            responses.is_empty(),
            "encrypted file-sharing DM should not echo to sender"
        );

        let inbox_query = format!(
            "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-esfs-1'>\
                <query xmlns='urn:waddle:inbox:0'/>\
             </iq>",
            bob_jid.to_bare()
        );
        let inbox_responses = handle_iq(
            inbox_query.as_str(),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(bob_session),
            &ready_phase(&bob_jid),
        )
        .await;
        let inbox_xml = inbox_responses.first().expect("inbox response");
        assert!(
            inbox_xml.contains("partner=\"alice@example.com\""),
            "encrypted file-sharing inbox entry should target Alice: {inbox_xml}"
        );
        assert!(
            inbox_xml.contains("unread=\"1\""),
            "encrypted file-sharing inbox entry should increment unread: {inbox_xml}"
        );
        assert!(
            !inbox_xml.contains("<preview>"),
            "bodyless encrypted file-sharing message should not invent preview text: {inbox_xml}"
        );
    }

    #[tokio::test]
    async fn groupchat_messages_are_archived_and_returned_via_mam() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let waddle_id = "waddle-alpha";
        let channel_id = "channel-bravo";
        let room_jid: BareJid = format!("{channel_id}@muc.example.com")
            .parse()
            .expect("room jid");
        let sender_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
            .parse()
            .expect("sender jid");
        // #229 PR18 cutover: groupchat reflections flow through the
        // connection registry as PeerStanza, so register the sender's
        // outbound channel before driving `handle_message`.
        let (sender_tx, mut sender_rx) = mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register(sender_jid.clone(), sender_tx);
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room_jid,
            RoomConfig::default(),
            waddle_id.to_string(),
            channel_id.to_string(),
        )
        .await
        .expect("create room");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: sender_jid.clone(),
                nick: "alice".to_string(),
                effective_affiliation: Affiliation::Member,
                local_domain: "example.com".to_string(),
            })
            .await
            .expect("join room");

        let message_xml = format!(
            "<message xmlns='jabber:client' to='{room_jid}' type='groupchat' id='client-msg-1'>\
                <body>Hello from WebSocket</body>\
             </message>"
        );
        let _message_responses = handle_message_for_test(
            state.as_ref(),
            &sender_jid,
            Some(&session),
            parse_message_for_test(message_xml.as_str()),
        )
        .await;
        let echo_stanza = sender_rx
            .try_recv()
            .expect("sender echo queued on outbound channel");
        let _echo_xml = stanza_to_xml(&echo_stanza.stanza);

        let mam_query = format!(
            "<iq xmlns='jabber:client' type='set' to='{room_jid}' id='mam-1'>\
                <query xmlns='urn:xmpp:mam:2' queryid='q1'>\
                    <x xmlns='jabber:x:data' type='submit'>\
                        <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>\
                    </x>\
                    <set xmlns='http://jabber.org/protocol/rsm'><max>50</max></set>\
                </query>\
             </iq>"
        );
        let mam_responses = handle_iq(
            mam_query.as_str(),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session),
            &ready_phase(&sender_jid),
        )
        .await;

        assert!(
            mam_responses
                .iter()
                .any(|stanza| stanza.contains("urn:xmpp:mam:2") && stanza.contains("<result")),
            "expected at least one MAM result stanza, got: {:?}",
            mam_responses
        );
        assert!(
            mam_responses
                .iter()
                .any(|stanza| stanza.contains("Hello from WebSocket")),
            "expected archived body in MAM replay, got: {:?}",
            mam_responses
        );
        assert!(
            mam_responses
                .iter()
                .any(|stanza| stanza.contains("<fin") && stanza.contains("urn:xmpp:mam:2")),
            "expected MAM fin stanza, got: {:?}",
            mam_responses
        );
    }

    #[tokio::test]
    async fn personal_mam_query_uses_ready_phase_when_sidecar_session_is_missing() {
        let state = create_test_websocket_state().await;
        let alice_session = create_test_session(state.as_ref(), "alice").await;
        let bob_session = create_test_session(state.as_ref(), "bob").await;
        let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
            .parse()
            .expect("alice jid");
        let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
            .parse()
            .expect("bob jid");

        let message_xml = format!(
            "<message xmlns='jabber:client' to='{}' type='chat' id='dm-mam-1'>\
                <body>Hello from Alice</body>\
             </message>",
            bob_jid.to_bare()
        );
        let message_responses = handle_message_for_test(
            state.as_ref(),
            &alice_jid,
            Some(&alice_session),
            parse_message_for_test(message_xml.as_str()),
        )
        .await;
        assert!(
            message_responses.is_empty(),
            "plain DM should not echo to sender"
        );

        let mam_query = format!(
            "<iq xmlns='jabber:client' type='set' to='{}' id='mam-personal-1'>\
                <query xmlns='urn:xmpp:mam:2' queryid='q1'>\
                    <x xmlns='jabber:x:data' type='submit'>\
                        <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>\
                    </x>\
                    <set xmlns='http://jabber.org/protocol/rsm'><max>50</max></set>\
                </query>\
             </iq>",
            bob_jid.to_bare()
        );
        let mam_responses = handle_iq(
            mam_query.as_str(),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&bob_jid),
        )
        .await;

        assert!(
            mam_responses
                .iter()
                .any(|stanza| stanza.contains("urn:xmpp:mam:2") && stanza.contains("<result")),
            "expected personal MAM result stanza for resumed ready phase, got: {:?}",
            mam_responses
        );
        assert!(
            mam_responses
                .iter()
                .any(|stanza| stanza.contains("Hello from Alice")),
            "expected archived DM in personal MAM replay, got: {:?}",
            mam_responses
        );
        assert!(
            mam_responses
                .iter()
                .any(|stanza| stanza.contains(&format!("to=\"{}\"", bob_jid))
                    || stanza.contains(&format!("to='{}'", bob_jid))),
            "expected MAM results addressed to the resumed bound resource, got: {:?}",
            mam_responses
        );
        assert!(
            mam_responses
                .iter()
                .all(|stanza| !stanza.contains("unknown@localhost")),
            "resumed personal MAM must not fall back to unknown recipient: {:?}",
            mam_responses
        );
    }

    #[tokio::test]
    async fn upload_slot_request_requires_ready_phase() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let pending_phase = authenticated_phase_for_session(&session, "example.com");
        let frame = r#"<iq xmlns='jabber:client' type='get' to='upload.example.com' id='upload-prebind-1'><request xmlns='urn:xmpp:http:upload:0' filename='hello.txt' size='5' content-type='text/plain'/></iq>"#;

        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session),
            &pending_phase,
        )
        .await;

        let response = responses.first().expect("upload error response");
        assert!(
            response.contains("not-authorized"),
            "pre-bind upload request should be rejected: {response}"
        );
    }

    #[test]
    fn stanza_to_xml_includes_payloads() {
        let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
            "bob@example.com".parse::<jid::BareJid>().unwrap(),
        )));
        msg.from = Some(jid::Jid::from(
            "alice@example.com".parse::<jid::BareJid>().unwrap(),
        ));
        msg.id = Some("test-1".into());
        msg.type_ = xmpp_parsers::message::MessageType::Chat;
        msg.bodies
            .insert(String::new(), xmpp_parsers::message::Body("Hello".into()));

        let embed = xmpp_parsers::minidom::Element::builder("repo", "urn:waddle:test-extension:1")
            .attr("owner", "cuenv")
            .attr("name", "cuenv")
            .build();
        msg.payloads.push(embed);

        let xml = stanza_to_xml(&Stanza::Message(msg));

        assert!(xml.contains("<body>Hello</body>"), "body must be present");
        assert!(
            xml.contains("urn:waddle:test-extension:1"),
            "payload namespace must be serialized: {xml}"
        );
        assert!(
            xml.contains("cuenv"),
            "payload attributes must be serialized: {xml}"
        );
        // Must not contain XML declaration inside the message
        assert!(
            !xml.contains("<?xml"),
            "payload must not include XML declaration: {xml}"
        );
        // The whole thing must be a single <message>...</message>
        assert!(
            xml.starts_with("<message"),
            "must start with <message: {xml}"
        );
        assert!(
            xml.ends_with("</message>"),
            "must end with </message>: {xml}"
        );
    }

    #[test]
    fn stanza_to_xml_no_payloads_still_works() {
        let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
            "bob@example.com".parse::<jid::BareJid>().unwrap(),
        )));
        msg.from = Some(jid::Jid::from(
            "alice@example.com".parse::<jid::BareJid>().unwrap(),
        ));
        msg.id = Some("test-2".into());
        msg.type_ = xmpp_parsers::message::MessageType::Chat;
        msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("No embeds".into()),
        );

        let xml = stanza_to_xml(&Stanza::Message(msg));
        assert!(xml.contains("<body>No embeds</body>"));
        assert!(xml.ends_with("</message>"));
    }

    #[test]
    fn parse_message_stanza_preserves_thread_and_reply() {
        let xml = r#"<message to="room@muc.localhost" type="groupchat" id="msg-1">
            <body>Hello</body>
            <thread>thread-root</thread>
            <reply xmlns="urn:xmpp:reply:0" id="parent-msg" to="alice@localhost"/>
        </message>"#;

        let parsed = parse_message_for_test(xml);
        assert_eq!(parsed.id.as_deref(), Some("msg-1"));
        assert_eq!(
            parsed.thread.as_ref().map(|t| t.0.as_str()),
            Some("thread-root")
        );
        assert!(parsed
            .payloads
            .iter()
            .any(|p| p.name() == "reply" && p.ns() == "urn:xmpp:reply:0"));
    }

    #[test]
    fn stanza_to_xml_preserves_thread_and_reply() {
        let xml = r#"<message to="room@muc.localhost" type="groupchat" id="msg-2">
            <body>Follow-up</body>
            <thread>thread-root</thread>
            <reply xmlns="urn:xmpp:reply:0" id="msg-1" to="alice@localhost"/>
        </message>"#;

        let parsed = parse_message_for_test(xml);
        let rendered = stanza_to_xml(&Stanza::Message(parsed));
        let reparsed = parse_message_for_test(&rendered);
        assert_eq!(
            reparsed.thread.as_ref().map(|thread| thread.0.as_str()),
            Some("thread-root"),
            "rendered stanza: {rendered}"
        );
        assert!(reparsed.payloads.iter().any(|payload| {
            payload.name() == "reply"
                && payload.ns() == "urn:xmpp:reply:0"
                && payload.attr("id") == Some("msg-1")
                && payload.attr("to") == Some("alice@localhost")
        }));
    }

    #[test]
    fn xep_0201_thread_reattach_ignores_unrelated_namespaced_thread_payload() {
        // RFC 6121 §5.2.5 scopes `<thread/>` to the enclosing message's
        // namespace. If a stanza already contains a `<thread>` payload
        // in some other namespace (an unrelated extension), the
        // reattach branch must NOT see it as a conflict and skip
        // serializing the typed `message.thread` field — that would
        // drop the actual conversation thread on the wire (Copilot
        // review on PR #305).
        use xmpp_parsers::message::{Body, Message, MessageType, Thread};
        use xmpp_parsers::minidom::Element;

        let mut msg = Message::new(Some(jid::Jid::from(
            "bob@example.com".parse::<jid::BareJid>().expect("jid"),
        )));
        msg.from = Some(jid::Jid::from(
            "alice@example.com/web"
                .parse::<jid::FullJid>()
                .expect("jid"),
        ));
        msg.id = Some("msg-ns".to_string());
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(String::new(), Body("hi".to_string()));
        msg.thread = Some(Thread("conversation-thread".to_string()));
        // Unrelated extension element happening to be named "thread"
        // in a different namespace — must not suppress reattachment.
        msg.payloads.push(
            Element::builder("thread", "urn:example:other:0")
                .attr("kind", "unrelated")
                .build(),
        );

        let rendered = stanza_to_xml(&Stanza::Message(msg));
        let reparsed = parse_message_for_test(&rendered);
        assert_eq!(
            reparsed.thread.as_ref().map(|t| t.0.as_str()),
            Some("conversation-thread"),
            "RFC 6121 thread must survive serialization despite unrelated <thread> in another ns; rendered: {rendered}"
        );
    }

    #[tokio::test]
    async fn handle_iq_pubsub_publish_returns_result() {
        let state = create_test_websocket_state().await;
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        let frame = r#"<iq xmlns="jabber:client" id="pub-1" type="set"><pubsub xmlns="http://jabber.org/protocol/pubsub"><publish node="http://jabber.org/protocol/mood"><item id="current"><mood xmlns="http://jabber.org/protocol/mood"><happy/></mood></item></publish></pubsub></iq>"#;
        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&jid),
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        let element = Element::from_str(&responses[0]).expect("valid XML");
        let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
        assert_eq!(iq.id, "pub-1");
        match iq.payload {
            xmpp_parsers::iq::IqType::Result(Some(payload)) => {
                assert_eq!(payload.ns(), "http://jabber.org/protocol/pubsub");
            }
            other => panic!("expected pubsub result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_iq_pubsub_items_empty_node_returns_result() {
        let state = create_test_websocket_state().await;
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        let frame = r#"<iq xmlns="jabber:client" id="items-1" type="get"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="http://jabber.org/protocol/mood"/></pubsub></iq>"#;
        let responses = handle_iq(
            frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&jid),
        )
        .await;

        assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
        let element = Element::from_str(&responses[0]).expect("valid XML");
        let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
        assert_eq!(iq.id, "items-1");
    }

    // ---- B: Non-blocking broadcast ------------------------------------

    #[tokio::test]
    async fn try_send_to_returns_dropped_full_on_backpressured_channel() {
        // A size-1 channel lets us prove try_send does not block when the
        // receiver isn't draining: the second send must report DroppedFull
        // immediately instead of awaiting capacity. Callers rely on this
        // variant to count silent drops for observability.
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "user@example.com/res".parse().expect("jid");
        let (tx, _rx) = mpsc::channel::<OutboundStanza>(1);
        registry.register(jid.clone(), tx);

        let stanza_a = Stanza::Presence(xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::None,
        ));
        let stanza_b = Stanza::Presence(xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::None,
        ));

        assert_eq!(
            registry.try_send_to(&jid, stanza_a),
            BroadcastOutcome::Delivered
        );
        assert_eq!(
            registry.try_send_to(&jid, stanza_b),
            BroadcastOutcome::DroppedFull
        );
    }

    #[tokio::test]
    async fn try_send_to_returns_dropped_closed_and_unregisters() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "gone@example.com/res".parse().expect("jid");
        let (tx, rx) = mpsc::channel::<OutboundStanza>(4);
        registry.register(jid.clone(), tx);
        drop(rx); // close the channel so try_send sees Closed

        let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::None,
        ));
        assert_eq!(
            registry.try_send_to(&jid, stanza),
            BroadcastOutcome::DroppedClosed
        );
        assert!(!registry.is_connected(&jid));
    }

    #[tokio::test]
    async fn try_send_to_returns_not_connected_when_unregistered() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "nobody@example.com/res".parse().expect("jid");
        let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::None,
        ));
        assert_eq!(
            registry.try_send_to(&jid, stanza),
            BroadcastOutcome::NotConnected
        );
    }

    #[tokio::test]
    async fn try_send_to_does_not_unregister_replacement_entry() {
        // Simulate the replacement race: connection A is registered, its
        // receiver is dropped so the sender is closed, connection B takes
        // over the same JID with a live sender, then something (e.g. a MUC
        // broadcast task that still holds a clone of A's sender) tries to
        // send. The try_send would see Closed on A's cloned sender — but
        // the entry in the registry is now B's live one and must NOT be
        // evicted.
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");

        let (tx_a, rx_a) = mpsc::channel::<OutboundStanza>(4);
        registry.register(jid.clone(), tx_a);
        drop(rx_a); // A's sender is now closed

        // B takes over. The register() call replaces A's entry; only B's
        // (live) sender is now in the registry.
        let (tx_b, _rx_b) = mpsc::channel::<OutboundStanza>(4);
        registry.register(jid.clone(), tx_b);

        // A broadcast path now tries to send. From its perspective, it sees
        // whatever sender is currently in the registry — which is B's live
        // one — so try_send_to returns Delivered. Either way, the entry
        // must remain in the registry.
        let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::None,
        ));
        let _outcome = registry.try_send_to(&jid, stanza);
        assert!(
            registry.is_connected(&jid),
            "replacement entry must still be registered after a try_send_to that races with eviction"
        );
    }

    #[test]
    fn is_countable_stanza_matches_element_name_not_prefix() {
        // Real stanzas that must count toward SM handled/sent counters.
        assert!(is_countable_stanza(
            "<iq xmlns='jabber:client' type='get' id='1'/>"
        ));
        assert!(is_countable_stanza("<message xmlns='jabber:client'/>"));
        assert!(is_countable_stanza("<presence xmlns='jabber:client'/>"));
        // Leading whitespace is tolerated (matches the pre-existing
        // trim behaviour — frames are always serialized with a
        // namespace by minidom, so callers never produce bare `<iq/>`).
        assert!(is_countable_stanza("  <iq xmlns='jabber:client' id='1'/>"));

        // SM control nonzas and stream-level frames must NOT count.
        assert!(!is_countable_stanza("<r xmlns='urn:xmpp:sm:3'/>"));
        assert!(!is_countable_stanza("<a xmlns='urn:xmpp:sm:3' h='1'/>"));
        assert!(!is_countable_stanza(
            "<enable xmlns='urn:xmpp:sm:3' resume='1'/>"
        ));
        assert!(!is_countable_stanza(
            "<resumed xmlns='urn:xmpp:sm:3' previd='x' h='0'/>"
        ));

        // Substring prefix collisions that the old `starts_with`
        // implementation would have accepted. These are all non-standard
        // today but the element-name match is how we stay safe if any
        // future XEP introduces similarly-named nonzas.
        assert!(!is_countable_stanza("<messages xmlns='urn:example'/>"));
        assert!(!is_countable_stanza("<presences xmlns='urn:example'/>"));
        assert!(!is_countable_stanza("<iqsomething/>"));

        // Malformed XML just doesn't count — no panic, no false positive.
        assert!(!is_countable_stanza("not-xml-at-all"));
        assert!(!is_countable_stanza(""));
    }

    // ---- C: MUC nick handling -----------------------------------------

    #[tokio::test]
    async fn muc_self_rejoin_does_not_emit_ghost_presence() {
        // Same user joins the same nick twice from different resources —
        // the second join must NOT include a presence for the old resource
        // in the response (which used to be seen as a "ghost" occupant and
        // broke self-presence detection on the client).
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "rejoin-channel@muc.example.com".parse().expect("room");
        let first: FullJid = "alice@example.com/tab-1".parse().expect("first");
        let second: FullJid = "alice@example.com/tab-2".parse().expect("second");

        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &first,
            "alice",
            &Some(owner_session),
        )
        .await;
        let responses = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &second,
            "alice",
            &None,
        )
        .await;

        // Count presences emitted to the joiner that came from room/alice.
        // Only the self-presence (status 110) should be there — no "ghost".
        let alice_presences_for_joiner = responses
            .iter()
            .filter_map(|xml| Element::from_str(xml).ok())
            .filter(|el| el.name() == "presence")
            .filter(|el| el.attr("from") == Some(&format!("{room_jid}/alice")))
            .filter(|el| el.attr("to") == Some(&second.to_string()))
            .count();
        assert_eq!(
            alice_presences_for_joiner, 1,
            "self-rejoin must produce exactly one self-presence, not a ghost + self pair"
        );

        // And the one presence we got must carry status 110.
        let self_presence = responses
            .iter()
            .filter_map(|xml| Element::from_str(xml).ok())
            .find(|el| {
                el.name() == "presence"
                    && el.attr("from") == Some(&format!("{room_jid}/alice"))
                    && el.attr("to") == Some(&second.to_string())
            })
            .expect("self-presence must be present");
        let user_x = self_presence
            .get_child("x", "http://jabber.org/protocol/muc#user")
            .expect("muc user payload");
        assert!(
            user_x
                .children()
                .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
            "status 110 must be present on self-rejoin"
        );
    }

    #[tokio::test]
    async fn muc_join_broadcast_includes_real_occupant_jid() {
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "public-channel@muc.example.com".parse().expect("room");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice");
        let bob: FullJid = "bob@example.com/phone".parse().expect("bob");

        let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(4);
        state
            .deps
            .protocol
            .connection_registry
            .register(alice.clone(), alice_tx);

        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            &Some(owner_session),
        )
        .await;
        let _ = handle_muc_join(state.as_ref(), "example.com", &room_jid, &bob, "bob", &None).await;

        let broadcast = alice_rx.try_recv().expect("bob join broadcast to alice");
        let broadcast_xml = stanza_to_xml(&broadcast.stanza);
        let presence = Element::from_str(&broadcast_xml).expect("broadcast presence XML");
        let user_x = presence
            .get_child("x", "http://jabber.org/protocol/muc#user")
            .expect("muc user payload");
        let item = user_x
            .get_child("item", "http://jabber.org/protocol/muc#user")
            .expect("muc user item");

        let expected_from = format!("{room_jid}/bob");
        let expected_to = alice.to_string();
        assert_eq!(presence.attr("from"), Some(expected_from.as_str()));
        assert_eq!(presence.attr("to"), Some(expected_to.as_str()));
        assert_eq!(item.attr("jid"), Some("bob@example.com/phone"));
        assert_eq!(item.attr("affiliation"), Some("member"));
        assert_eq!(item.attr("role"), Some("participant"));
        assert!(
            user_x
                .children()
                .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
            "non-anonymous room presence must advertise status 100: {broadcast_xml}"
        );
    }

    #[tokio::test]
    async fn muc_nick_collision_returns_conflict_presence() {
        // Two different users try to hold the same nick — second gets a
        // <presence type='error'/> with <conflict/>, and room state for
        // the incumbent is untouched.
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "conflict-channel@muc.example.com".parse().expect("room");
        let alice: FullJid = "alice@example.com/desktop".parse().expect("alice");
        let bob: FullJid = "bob@example.com/phone".parse().expect("bob");

        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "dino",
            &Some(owner_session),
        )
        .await;
        let responses = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &bob,
            "dino",
            &None,
        )
        .await;

        assert_eq!(responses.len(), 1, "exactly one error presence");
        let el = Element::from_str(&responses[0]).expect("valid XML");
        assert_eq!(el.name(), "presence");
        assert_eq!(el.attr("type"), Some("error"));
        let bob_str = bob.to_string();
        assert_eq!(el.attr("to"), Some(bob_str.as_str()));
        let err = el
            .get_child("error", waddle_xmpp::ns::JABBER_CLIENT)
            .expect("error element");
        assert_eq!(err.attr("type"), Some("cancel"));
        assert!(err
            .get_child("conflict", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some());

        // Alice still owns the nick.
        let room = snapshot_room(state.as_ref(), &room_jid).await.room;
        assert_eq!(room.find_nick_by_real_jid(&alice), Some("dino"));
        assert!(room.find_nick_by_real_jid(&bob).is_none());
        assert_eq!(room.occupant_count(), 1);
    }

    // ---- D: XEP-0198 stream management --------------------------------

    #[tokio::test]
    async fn sm_features_advertise_sm_namespace() {
        // Stream features after successful auth must include <sm/>.
        let features = build_stream_features_xml(true);
        let el = Element::from_str(&features).expect("features xml");
        assert!(
            el.children()
                .any(|child| child.name() == "sm" && child.ns() == SM_NS),
            "post-auth features must advertise urn:xmpp:sm:3"
        );
    }

    #[tokio::test]
    async fn xep0237_features_advertise_roster_versioning() {
        let features = build_stream_features_xml(true);
        let el = Element::from_str(&features).expect("features xml");
        assert!(
            el.children()
                .any(|child| { child.name() == "ver" && child.ns() == waddle_xmpp::ns::ROSTERVER }),
            "post-auth features must advertise urn:xmpp:features:rosterver"
        );
    }

    #[tokio::test]
    async fn rfc6121_features_advertise_subscription_preapproval() {
        let features = build_stream_features_xml(true);
        let el = Element::from_str(&features).expect("features xml");
        assert!(
            el.children().any(|child| {
                child.name() == "sub" && child.ns() == "urn:xmpp:features:pre-approval"
            }),
            "post-auth features must advertise RFC 6121 subscription pre-approval"
        );
    }

    #[tokio::test]
    async fn sm_enable_requires_resource_binding() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        // Without resource_bound, enable must fail.
        let frame = "<enable xmlns='urn:xmpp:sm:3' resume='true'/>";
        let responses = handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(responses.len(), 1);
        let el = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(el.name(), "failed");
        assert!(!conn.sm_state.enabled);
    }

    #[tokio::test]
    async fn sm_resume_requires_authentication() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();

        let responses = handle_xmpp_frame(
            "<resume xmlns='urn:xmpp:sm:3' previd='stream-xyz' h='0'/>",
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1);
        let el = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(el.name(), "failed");
        assert!(el
            .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some());
        assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    }

    #[tokio::test]
    async fn sm_resume_is_allowed_after_auth_before_bind() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let mut conn = WsConnState::new();

        let auth_responses =
            handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(auth_responses, vec![sasl_success_xml()]);

        let responses = handle_xmpp_frame(
            "<resume xmlns='urn:xmpp:sm:3' previd='stream-xyz' h='0'/>",
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1);
        let el = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(el.name(), "failed");
        assert!(el
            .get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some());
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
        assert!(!conn.phase.is_resumed());
    }

    #[tokio::test]
    async fn sm_resume_rejects_authenticated_identity_mismatch_and_preserves_session() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

        let state = create_test_websocket_state().await;
        let domain = state.deps.auth_state.xmpp_domain.clone();
        let session = create_test_session(state.as_ref(), "bob").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let mut conn = WsConnState::new();
        let auth_responses =
            handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(auth_responses, vec![sasl_success_xml()]);
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));

        let detached = DetachedSession {
            stream_id: "stream-auth-mismatch".to_string(),
            user_id: format!("alice@{domain}"),
            jid: format!("alice@{domain}/web").parse().expect("jid"),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        };
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(detached.clone())
            .await
            .expect("store");

        let responses = handle_xmpp_frame(
            "<resume xmlns='urn:xmpp:sm:3' previd='stream-auth-mismatch' h='0'/>",
            &domain,
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1);
        let el = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(el.name(), "failed");
        assert!(el
            .get_child("not-authorized", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some());
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
        assert_eq!(
            conn.phase.authenticated_bare_jid().map(ToString::to_string),
            Some(format!("bob@{domain}"))
        );

        let stored = state
            .deps
            .protocol
            .sm_session_registry
            .take_session("stream-auth-mismatch")
            .await
            .expect("take")
            .expect("detached session should remain");
        assert_eq!(stored.jid, detached.jid);
    }

    #[tokio::test]
    async fn sm_resume_matching_authenticated_identity_preserves_current_session_without_sidecar() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

        let state = create_test_websocket_state().await;
        let domain = state.deps.auth_state.xmpp_domain.clone();
        let session = create_test_session(state.as_ref(), "bob").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let mut conn = WsConnState::new();
        let auth_responses =
            handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(auth_responses, vec![sasl_success_xml()]);
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));

        let detached_jid: FullJid = format!("bob@{domain}/web").parse().expect("jid");
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: "stream-auth-match".to_string(),
                user_id: format!("bob@{domain}"),
                jid: detached_jid.clone(),
                inbound_count: 2,
                outbound_count: 3,
                last_acked: 3,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: true,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store");

        let responses = handle_xmpp_frame(
            "<resume xmlns='urn:xmpp:sm:3' previd='stream-auth-match' h='3'/>",
            &domain,
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1);
        let resumed = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(resumed.name(), "resumed");
        assert_eq!(conn.phase.bound_jid(), Some(&detached_jid));
        assert!(conn.phase.is_ready());
        assert!(conn.phase.is_resumed());
        assert!(matches!(
            &conn.phase,
            ConnectionPhase::Ready {
                full_jid,
                resumed: true,
                ..
            } if full_jid == &detached_jid
        ));
        assert_eq!(
            conn.authenticated_session
                .as_ref()
                .map(|saved| saved.user_id.as_str()),
            Some(session.user_id.as_str())
        );
    }

    #[tokio::test]
    async fn sm_resume_matching_authenticated_identity_prefers_detached_sidecar_session() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

        let state = create_test_websocket_state().await;
        let domain = state.deps.auth_state.xmpp_domain.clone();
        let fresh_session = create_test_session(state.as_ref(), "bob").await;
        let payload =
            BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", fresh_session.id));
        let auth_frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let mut conn = WsConnState::new();
        let auth_responses =
            handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
        assert_eq!(auth_responses, vec![sasl_success_xml()]);

        let stream_id = "stream-auth-match-with-sidecar";
        let detached_jid: FullJid = format!("bob@{domain}/web").parse().expect("jid");
        let resumed_session = Session::new(&fresh_session.user_id, "bob", "bob");
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.to_string(),
                user_id: format!("bob@{domain}"),
                jid: detached_jid.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store");
        state
            .deps
            .protocol
            .resumable_sessions
            .insert(stream_id.to_string(), resumed_session.clone());

        let responses = handle_xmpp_frame(
            &format!("<resume xmlns='urn:xmpp:sm:3' previd='{stream_id}' h='0'/>"),
            &domain,
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1);
        let resumed = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(resumed.name(), "resumed");
        assert!(matches!(
            &conn.phase,
            ConnectionPhase::Ready {
                full_jid,
                resumed: true,
                ..
            } if full_jid == &detached_jid
        ));
        assert_eq!(
            conn.authenticated_session
                .as_ref()
                .map(|saved| saved.id.as_str()),
            Some(resumed_session.id.as_str())
        );
        assert_ne!(
            conn.authenticated_session
                .as_ref()
                .map(|saved| saved.id.as_str()),
            Some(fresh_session.id.as_str())
        );
    }

    #[tokio::test]
    async fn sm_resume_rejects_ready_phase() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        conn.phase = ConnectionPhase::ready(jid, false);

        let responses = handle_xmpp_frame(
            "<resume xmlns='urn:xmpp:sm:3' previd='stream-xyz' h='0'/>",
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1);
        let el = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(el.name(), "failed");
        assert!(el
            .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some());
        assert!(matches!(conn.phase, ConnectionPhase::Ready { .. }));
    }

    #[tokio::test]
    async fn sm_enable_after_bind_returns_enabled_and_tracks_counters() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        conn.phase = ConnectionPhase::ready(jid, false);

        let responses = handle_xmpp_frame(
            "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;
        assert_eq!(responses.len(), 1);
        let el = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(el.name(), "enabled");
        assert_eq!(el.attr("resume"), Some("true"));
        assert!(el.attr("id").filter(|s| !s.is_empty()).is_some());
        assert!(conn.sm_state.enabled);
        assert!(conn.sm_state.is_resumable());

        // An ack request bumps no counters but produces <a h=inbound_count/>.
        let ack_responses = handle_xmpp_frame(
            "<r xmlns='urn:xmpp:sm:3'/>",
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;
        assert_eq!(ack_responses.len(), 1);
        let ack_el = Element::from_str(&ack_responses[0]).expect("xml");
        assert_eq!(ack_el.name(), "a");
        assert_eq!(ack_el.attr("h"), Some("0"));

        // A countable inbound stanza bumps the inbound counter.
        let _ = handle_xmpp_frame(
            "<presence xmlns='jabber:client'/>",
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;
        assert_eq!(conn.sm_state.get_inbound_count(), 1);

        // Subsequent <r/> should now report h=1.
        let ack2 = handle_xmpp_frame(
            "<r xmlns='urn:xmpp:sm:3'/>",
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;
        let ack2_el = Element::from_str(&ack2[0]).expect("xml");
        assert_eq!(ack2_el.attr("h"), Some("1"));
    }

    #[tokio::test]
    async fn sm_resume_restores_session_and_replays_unacked() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        // Seed a detached session directly in the registry — this is the
        // shape left behind by a prior WebSocket task after detach-on-close.
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let stream_id = "stream-xyz".to_string();
        let detached = DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 7,
            outbound_count: 10,
            last_acked: 8,
            unacked_stanzas: vec![
                (9, "<message id='m9'/>".to_string()),
                (10, "<message id='m10'/>".to_string()),
            ],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        };
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(detached)
            .await
            .expect("store");

        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::authenticated(&jid);
        // Client reports it has acked through 9, so only m10 needs replay.
        let frame = format!(
            "<resume xmlns='urn:xmpp:sm:3' previd='{}' h='9'/>",
            stream_id
        );
        let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

        // Expect <resumed/> first, then exactly the one unacked stanza.
        assert!(!responses.is_empty());
        let resumed = Element::from_str(&responses[0]).expect("resumed xml");
        assert_eq!(resumed.name(), "resumed");
        assert_eq!(resumed.attr("previd"), Some(stream_id.as_str()));

        let replay_count = responses.len() - 1;
        assert_eq!(
            replay_count, 1,
            "only m10 should be replayed: {responses:?}"
        );
        assert!(responses[1].contains("m10"));

        // Session identity restored without SASL or bind frames.
        assert!(conn.phase.is_authenticated());
        assert!(conn.phase.is_ready());
        assert_eq!(conn.phase.bound_jid(), Some(&jid));
        assert!(conn.phase.is_resumed());
        assert!(conn.carbons_enabled);
        assert!(matches!(
            &conn.phase,
            ConnectionPhase::Ready {
                full_jid,
                resumed: true,
                ..
            } if full_jid == &jid
        ));
    }

    #[tokio::test]
    async fn sm_resume_rejects_impossible_client_handled_count() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: "stream-too-far".to_string(),
                user_id: "alice@example.com".to_string(),
                jid: jid.clone(),
                inbound_count: 4,
                outbound_count: 2,
                last_acked: 0,
                unacked_stanzas: vec![(1, "<message id='m1'/>".to_string())],
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store");

        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::authenticated(&jid);
        let responses = handle_xmpp_frame(
            "<resume xmlns='urn:xmpp:sm:3' previd='stream-too-far' h='3'/>",
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses.len(), 1);
        assert!(
            responses[0].contains("stream:error")
                && responses[0].contains("undefined-condition")
                && responses[0].contains("handled-count-too-high")
                && (responses[0].contains("h=\"3\"") || responses[0].contains("h='3'"))
                && (responses[0].contains("send-count=\"2\"")
                    || responses[0].contains("send-count='2'")),
            "invalid resume count should be a handled-count-too-high stream error: {responses:?}"
        );
        assert!(
            !conn.sm_state.enabled,
            "rejected resume must not pollute the fresh stream SM state"
        );
        assert!(
            !conn.sm_state.is_resumable(),
            "rejected resume must not make the fresh stream resumable"
        );
        assert!(
            state
                .deps
                .protocol
                .sm_session_registry
                .take_session("stream-too-far")
                .await
                .expect("lookup")
                .is_some(),
            "rejected resume must release the detached session for a valid retry"
        );
    }

    #[tokio::test]
    async fn sm_resume_replays_roster_push_recorded_while_detached() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let stream_id = "stream-roster-replay".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.clone(),
                user_id: "alice@example.com".to_string(),
                jid: jid.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: true,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store");

        let recorded = state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(
                &jid,
                &Stanza::Iq(
                    Element::from_str(
                        "<iq xmlns='jabber:client' type='set' id='detached-roster-push'><query xmlns='jabber:iq:roster'/></iq>",
                    )
                    .expect("iq element")
                    .try_into()
                    .expect("iq stanza"),
                ),
            )
            .await
            .expect("record detached roster push");
        assert!(recorded);

        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::authenticated(&jid);
        let frame = format!("<resume xmlns='urn:xmpp:sm:3' previd='{stream_id}' h='0'/>");
        let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

        assert_eq!(
            responses.len(),
            2,
            "expected resumed plus replay: {responses:?}"
        );
        assert!(responses[0].contains("<resumed"));
        assert!(
            responses[1].contains("detached-roster-push"),
            "detached roster push should replay after resume: {responses:?}"
        );
        assert!(conn.roster_interested);
    }

    #[tokio::test]
    async fn direct_full_jid_message_records_for_detached_resource_replay() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
        let stream_id = "stream-detached-direct-message".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.clone(),
                user_id: "alice@example.com".to_string(),
                jid: alice_jid,
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store detached alice");

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
        bob.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            bob_jid,
            false,
            Blocklist::empty(),
        );
        let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="alice@example.com/phone" id="detached-dm-1"><body>queued while detached</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        assert!(responses.is_empty());

        let detached = state
            .deps
            .protocol
            .sm_session_registry
            .take_session(&stream_id)
            .await
            .expect("take detached")
            .expect("detached session remains");
        assert!(
            detached
                .unacked_stanzas
                .iter()
                .any(|(_, stanza)| stanza.contains("detached-dm-1")),
            "full-JID direct message should be recorded for detached replay: {detached:?}"
        );
    }

    #[tokio::test]
    async fn bare_jid_message_records_for_detached_resource_replay() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
        let stream_id = "stream-detached-bare-message".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.clone(),
                user_id: "alice@example.com".to_string(),
                jid: alice_jid,
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store detached alice");

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
        bob.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            bob_jid,
            false,
            Blocklist::empty(),
        );
        let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="alice@example.com" id="detached-bare-dm-1"><body>queued while detached</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        assert!(responses.is_empty());

        let detached = state
            .deps
            .protocol
            .sm_session_registry
            .take_session(&stream_id)
            .await
            .expect("take detached")
            .expect("detached session remains");
        assert!(
            detached
                .unacked_stanzas
                .iter()
                .any(|(_, stanza)| stanza.contains("detached-bare-dm-1")),
            "bare-JID direct message should be recorded for detached replay: {detached:?}"
        );
        // RFC 6121 §8.5.2.1.1: bare-JID delivery routes the original
        // stanza to each available resource without rewriting `to`.
        // The dispatcher path preserves this — legacy `handle_message`
        // rewrote `to` to the per-resource full JID, which was a
        // server-side deviation from the RFC. Assert only the
        // reachability semantic here; integration tests verify the
        // wire shape end-to-end.
    }

    #[tokio::test]
    async fn message_carbons_record_for_detached_enabled_resources() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let alice_phone: FullJid = "alice@example.com/phone".parse().expect("alice phone");
        let alice_laptop: FullJid = "alice@example.com/laptop".parse().expect("alice laptop");
        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let sent_stream_id = "stream-detached-sent-carbon".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: sent_stream_id.clone(),
                user_id: "alice@example.com".to_string(),
                jid: alice_laptop.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: true,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store detached alice laptop");

        let mut alice = WsConnState::new();
        alice.phase = ConnectionPhase::ready(alice_phone.clone(), false);
        alice.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            alice_phone.clone(),
            false,
            Blocklist::empty(),
        );
        let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="bob@example.com/web" id="detached-sent-carbon-source"><body>copy me</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
        assert!(responses.is_empty());

        let sent_detached = state
            .deps
            .protocol
            .sm_session_registry
            .take_session(&sent_stream_id)
            .await
            .expect("take sent detached")
            .expect("sent detached session remains");
        assert!(
            sent_detached
                .unacked_stanzas
                .iter()
                .any(|(_, stanza)| stanza.contains("<sent")
                    && stanza.contains("urn:xmpp:carbons:2")
                    && stanza.contains("detached-sent-carbon-source")),
            "sent carbon should be recorded for detached opted-in resource: {sent_detached:?}"
        );

        let received_stream_id = "stream-detached-received-carbon".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: received_stream_id.clone(),
                user_id: "alice@example.com".to_string(),
                jid: alice_laptop,
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: true,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store detached alice laptop again");

        // Build alice/phone's per-connection state machine so we can
        // drive the recipient-pass carbon fan-out the dispatcher path
        // owns. In production this happens automatically via
        // alice/phone's main loop dispatching the queued
        // `DeliveryKind::PeerStanza`; the unit test reproduces the
        // same step explicitly.
        let mut alice_phone_conn = WsConnState::new();
        alice_phone_conn.phase = ConnectionPhase::ready(alice_phone.clone(), false);
        alice_phone_conn.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            alice_phone.clone(),
            false,
            Blocklist::empty(),
        );
        let (alice_phone_tx, mut alice_phone_rx) = mpsc::channel::<OutboundStanza>(16);
        state
            .deps
            .protocol
            .connection_registry
            .register(alice_phone.clone(), alice_phone_tx);

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
        bob.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            bob_jid,
            false,
            Blocklist::empty(),
        );
        let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="alice@example.com/phone" id="detached-received-carbon-source"><body>copy me too</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        assert!(responses.is_empty());

        // Pump the queued PeerStanza through alice/phone's SM so the
        // recipient pass runs and the dispatcher emits the
        // received-carbon fan-out. This is the same dispatch the
        // production main loop performs on `DeliveryKind::PeerStanza`.
        while let Ok(outbound) = alice_phone_rx.try_recv() {
            if !matches!(outbound.kind, DeliveryKind::PeerStanza) {
                continue;
            }
            let sm = alice_phone_conn.state_machine.as_mut().expect("alice SM");
            let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(outbound.stanza)));
            let deps = build_interpret_deps(state.as_ref(), None);
            let _ = drive_interpret_loop(events, sm, &deps).await;
        }

        let received_detached = state
            .deps
            .protocol
            .sm_session_registry
            .take_session(&received_stream_id)
            .await
            .expect("take received detached")
            .expect("received detached session remains");
        assert!(
            received_detached
                .unacked_stanzas
                .iter()
                .any(|(_, stanza)| stanza.contains("<received")
                    && stanza.contains("urn:xmpp:carbons:2")
                    && stanza.contains("detached-received-carbon-source")),
            "received carbon should be recorded for detached opted-in resource: {received_detached:?}"
        );
    }

    #[tokio::test]
    async fn duplicate_subscribe_ack_reaches_non_roster_interested_resource() {
        let state = create_test_websocket_state().await;

        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
        let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
        let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(16);
        state
            .deps
            .protocol
            .connection_registry
            .register(bob_jid.clone(), bob_tx);
        state
            .deps
            .protocol
            .connection_registry
            .register(alice_jid.clone(), alice_tx);

        let mut alice = WsConnState::new();
        alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="get" id="alice-roster"><query xmlns="jabber:iq:roster"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client"/>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
        while alice_rx.try_recv().is_ok() {}

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        let _ = tokio::time::timeout(std::time::Duration::from_millis(250), alice_rx.recv())
            .await
            .expect("alice receives initial subscribe")
            .expect("subscribe stanza");

        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
        while bob_rx.try_recv().is_ok() {}

        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        let ack = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
            .await
            .expect("duplicate subscribe ack")
            .expect("ack stanza");
        let frame = stanza_to_xml(&ack.stanza);
        assert!(
            frame.contains("from=\"alice@example.com\"")
                && frame.contains("to=\"bob@example.com\"")
                && frame.contains("type=\"subscribed\""),
            "duplicate subscribe ack should reach a live resource even before roster get: {frame}"
        );
    }

    #[tokio::test]
    async fn roster_set_records_push_for_detached_interested_resource() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let detached_jid: FullJid = "alice@example.com/web".parse().expect("detached jid");
        let source_jid: FullJid = "alice@example.com/phone".parse().expect("source jid");
        let stream_id = "stream-roster-fanout".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.clone(),
                user_id: "alice@example.com".to_string(),
                jid: detached_jid.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: true,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store detached session");

        let mut source = WsConnState::new();
        source.phase = ConnectionPhase::ready(source_jid, false);
        let responses = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="set" id="roster-detached-fanout"><query xmlns="jabber:iq:roster"><item jid="bob@example.com" name="Bob"/></query></iq>"#,
            "example.com",
            state.as_ref(),
            &mut source,
        )
        .await;
        assert!(
            responses
                .iter()
                .any(|frame| frame.contains("roster-detached-fanout")
                    && frame.contains("type=\"result\"")),
            "roster set should succeed: {responses:?}"
        );

        let mut resumed = WsConnState::new();
        resumed.phase = ConnectionPhase::authenticated(&detached_jid);
        let resume_frame = format!("<resume xmlns='urn:xmpp:sm:3' previd='{stream_id}' h='0'/>");
        let replay =
            handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
        assert!(
            replay.iter().any(
                |frame| frame.contains("jabber:iq:roster") && frame.contains("bob@example.com")
            ),
            "detached interested resource should replay roster fanout push: {replay:?}"
        );
        assert!(resumed.roster_interested);
    }

    #[tokio::test]
    async fn subscription_approval_replays_current_presence_from_detached_available_resource() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice_web_jid: FullJid = "alice@example.com/web".parse().expect("alice web jid");
        let alice_phone_jid: FullJid = "alice@example.com/phone".parse().expect("alice phone jid");
        let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
        state
            .deps
            .protocol
            .connection_registry
            .register(bob_jid.clone(), bob_tx);
        state
            .deps
            .protocol
            .connection_registry
            .mark_roster_interested(&bob_jid);
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&bob_jid, true, 0);

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        while bob_rx.try_recv().is_ok() {}

        let stream_id = "stream-detached-current-presence".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id,
                user_id: "alice@example.com".to_string(),
                jid: alice_web_jid.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: true,
                presence_show: Some(xmpp_parsers::presence::Show::Chat),
                presence_status: Some("ready from detach".to_string()),
                presence_priority: 7,
            })
            .await
            .expect("store detached alice web");

        let mut alice_phone = WsConnState::new();
        alice_phone.phase = ConnectionPhase::ready(alice_phone_jid, false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut alice_phone,
        )
        .await;

        let mut delivered = Vec::new();
        for _ in 0..4 {
            if let Ok(Some(outbound)) =
                tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv()).await
            {
                delivered.push(stanza_to_xml(&outbound.stanza));
            }
        }
        assert!(
            delivered.iter().any(|frame| {
                frame.contains("from=\"alice@example.com/web\"")
                    && frame.contains("<show>chat</show>")
                    && frame.contains("<status>ready from detach</status>")
                    && frame.contains("<priority>7</priority>")
            }),
            "approval should deliver current rich presence from detached available resource: {delivered:?}"
        );
    }

    #[tokio::test]
    async fn presence_probe_returns_detached_available_resource_presence() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
        let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
        state
            .deps
            .protocol
            .connection_registry
            .register(bob_jid.clone(), bob_tx);

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        let mut alice = WsConnState::new();
        alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
        while bob_rx.try_recv().is_ok() {}

        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: "stream-detached-probe".to_string(),
                user_id: "alice@example.com".to_string(),
                jid: alice_jid,
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: true,
                presence_show: Some(xmpp_parsers::presence::Show::Away),
                presence_status: Some("stepped away".to_string()),
                presence_priority: 5,
            })
            .await
            .expect("store detached alice");

        bob.phase = ConnectionPhase::ready(bob_jid, false);
        let responses = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="probe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        assert!(responses.is_empty());

        let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
            .await
            .expect("probe response")
            .expect("outbound stanza");
        let frame = stanza_to_xml(&outbound.stanza);
        assert!(
            frame.contains("from=\"alice@example.com/phone\"")
                && frame.contains("to=\"bob@example.com\"")
                && frame.contains("<show>away</show>")
                && frame.contains("<status>stepped away</status>")
                && frame.contains("<priority>5</priority>"),
            "probe should return rich presence from detached available resource: {frame}"
        );
    }

    #[tokio::test]
    async fn full_jid_presence_probe_returns_only_that_resources_availability() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice_phone: FullJid = "alice@example.com/phone".parse().expect("alice phone");
        let alice_tablet: FullJid = "alice@example.com/tablet".parse().expect("alice tablet");
        let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
        state
            .deps
            .protocol
            .connection_registry
            .register(bob_jid.clone(), bob_tx);

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        let mut alice = WsConnState::new();
        alice.phase = ConnectionPhase::ready(alice_phone.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
        while bob_rx.try_recv().is_ok() {}

        for (stream_id, jid, show, status) in [
            (
                "stream-probe-phone",
                alice_phone.clone(),
                xmpp_parsers::presence::Show::Away,
                "phone detail",
            ),
            (
                "stream-probe-tablet",
                alice_tablet,
                xmpp_parsers::presence::Show::Chat,
                "tablet detail",
            ),
        ] {
            state
                .deps
                .protocol
                .sm_session_registry
                .store_session(DetachedSession {
                    stream_id: stream_id.to_string(),
                    user_id: "alice@example.com".to_string(),
                    jid,
                    inbound_count: 0,
                    outbound_count: 0,
                    last_acked: 0,
                    unacked_stanzas: Vec::new(),
                    max_resume_time: Some(300),
                    detached_at: std::time::Instant::now(),
                    carbons_enabled: false,
                    roster_interested: false,
                    presence_available: true,
                    presence_show: Some(show),
                    presence_status: Some(status.to_string()),
                    presence_priority: 5,
                })
                .await
                .expect("store detached alice resource");
        }

        let responses = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="probe" to="alice@example.com/phone"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        assert!(responses.is_empty());

        let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
            .await
            .expect("full-jid probe response")
            .expect("outbound stanza");
        let frame = stanza_to_xml(&outbound.stanza);
        assert!(
            frame.contains("from=\"alice@example.com/phone\"")
                && frame.contains("to=\"bob@example.com\"")
                && frame.contains("<show>away</show>")
                && frame.contains("<status>phone detail</status>")
                && frame.contains("<priority>5</priority>")
                && !frame.contains("alice@example.com/tablet"),
            "full-JID probe should return rich presence only for the requested resource: {frame}"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), bob_rx.recv())
                .await
                .is_err(),
            "full-JID probe must not return sibling resources"
        );
    }

    #[tokio::test]
    async fn presence_probe_without_subscription_does_not_reveal_detached_presence() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let mallory_jid: FullJid = "mallory@example.com/web".parse().expect("mallory jid");
        let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
        let (mallory_tx, mut mallory_rx) = mpsc::channel::<OutboundStanza>(16);
        state
            .deps
            .protocol
            .connection_registry
            .register(mallory_jid.clone(), mallory_tx);
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: "stream-detached-probe-denied".to_string(),
                user_id: "alice@example.com".to_string(),
                jid: alice_jid,
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: true,
                presence_show: Some(xmpp_parsers::presence::Show::Away),
                presence_status: Some("private".to_string()),
                presence_priority: 5,
            })
            .await
            .expect("store detached alice");

        let mut mallory = WsConnState::new();
        mallory.phase = ConnectionPhase::ready(mallory_jid, false);
        let responses = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="probe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut mallory,
        )
        .await;
        assert!(responses.is_empty());
        let outbound =
            tokio::time::timeout(std::time::Duration::from_millis(250), mallory_rx.recv())
                .await
                .expect("unsubscribed probe response")
                .expect("outbound stanza");
        let frame = stanza_to_xml(&outbound.stanza);
        assert!(
            frame.contains("from=\"alice@example.com\"")
                && frame.contains("to=\"mallory@example.com\"")
                && frame.contains("type=\"unsubscribed\"")
                && !frame.contains("alice@example.com/phone")
                && !frame.contains("private"),
            "unauthorized probe must return only an unsubscribed signal: {frame}"
        );
    }

    #[tokio::test]
    async fn expired_detached_available_session_broadcasts_unavailable_to_subscribers() {
        let state = create_test_websocket_state().await;

        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
        let alice_sibling_jid: FullJid = "alice@example.com/laptop".parse().expect("alice sibling");
        let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
        state
            .deps
            .protocol
            .connection_registry
            .register(bob_jid.clone(), bob_tx);
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&bob_jid, true, 0);
        let (alice_sibling_tx, mut alice_sibling_rx) = mpsc::channel::<OutboundStanza>(16);
        state
            .deps
            .protocol
            .connection_registry
            .register(alice_sibling_jid.clone(), alice_sibling_tx);
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&alice_sibling_jid, true, 0);

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid, false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        let mut alice = WsConnState::new();
        alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
        while bob_rx.try_recv().is_ok() {}
        while alice_sibling_rx.try_recv().is_ok() {}

        handlers::presence::broadcast_unavailable_for_expired_detached_session(
            state.as_ref(),
            &alice_jid,
        )
        .await;

        let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
            .await
            .expect("unavailable broadcast")
            .expect("outbound stanza");
        let frame = stanza_to_xml(&outbound.stanza);
        assert!(
            frame.contains("from=\"alice@example.com/phone\"")
                && frame.contains("to=\"bob@example.com\"")
                && frame.contains("type=\"unavailable\""),
            "expired detached session should broadcast unavailable presence: {frame}"
        );
        let sibling_outbound = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            alice_sibling_rx.recv(),
        )
        .await
        .expect("sibling unavailable broadcast")
        .expect("outbound stanza");
        let sibling_frame = stanza_to_xml(&sibling_outbound.stanza);
        assert!(
            sibling_frame.contains("from=\"alice@example.com/phone\"")
                && sibling_frame.contains("to=\"alice@example.com\"")
                && sibling_frame.contains("type=\"unavailable\""),
            "expired detached session should notify sibling resources: {sibling_frame}"
        );
    }

    #[tokio::test]
    async fn subscription_approval_records_roster_push_for_detached_interested_resource() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="get" id="bob-roster"><query xmlns="jabber:iq:roster"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;

        let stream_id = "stream-detached-subscription-roster-push".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.clone(),
                user_id: "bob@example.com".to_string(),
                jid: bob_jid.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: true,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store detached bob");

        let mut alice = WsConnState::new();
        alice.phase = ConnectionPhase::ready(alice_jid, false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;

        let mut resumed = WsConnState::new();
        resumed.phase = ConnectionPhase::authenticated(&bob_jid);
        let resume_frame = format!("<resume xmlns='urn:xmpp:sm:3' previd='{stream_id}' h='0'/>");
        let replay =
            handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
        assert!(
            replay.iter().any(|frame| {
                frame.contains("jabber:iq:roster")
                    && frame.contains("alice@example.com")
                    && frame.contains("subscription=\"to\"")
            }),
            "detached interested resource should replay subscription roster push: {replay:?}"
        );
    }

    #[tokio::test]
    async fn subscribe_to_detached_available_resource_replays_on_resume() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let stream_id = "stream-detached-subscribe-recipient".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.clone(),
                user_id: "alice@example.com".to_string(),
                jid: alice_jid.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: true,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store detached alice");

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid, false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;

        let mut resumed = WsConnState::new();
        resumed.phase = ConnectionPhase::authenticated(&alice_jid);
        let resume_frame = format!("<resume xmlns='urn:xmpp:sm:3' previd='{stream_id}' h='0'/>");
        let replay =
            handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
        assert!(
            replay.iter().any(|frame| {
                frame.contains("type=\"subscribe\"") && frame.contains("from=\"bob@example.com\"")
            }),
            "detached available recipient should replay inbound subscribe: {replay:?}"
        );
    }

    #[tokio::test]
    async fn presence_broadcast_to_detached_available_subscriber_replays_on_resume() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");

        let mut bob = WsConnState::new();
        bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
        let mut alice = WsConnState::new();
        alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;

        let stream_id = "stream-detached-presence-broadcast".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.clone(),
                user_id: "bob@example.com".to_string(),
                jid: bob_jid.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: true,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store detached bob");

        let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client"><show>away</show><status>broadcast while detached</status><priority>5</priority></presence>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;

        let mut resumed = WsConnState::new();
        resumed.phase = ConnectionPhase::authenticated(&bob_jid);
        let resume_frame = format!("<resume xmlns='urn:xmpp:sm:3' previd='{stream_id}' h='0'/>");
        let replay =
            handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
        assert!(
            replay.iter().any(|frame| {
                frame.contains("from=\"alice@example.com/web\"")
                    && frame.contains("<show>away</show>")
                    && frame.contains("<status>broadcast while detached</status>")
                    && frame.contains("<priority>5</priority>")
            }),
            "detached available subscriber should replay presence broadcast: {replay:?}"
        );
    }

    #[tokio::test]
    async fn sm_resume_with_unknown_stream_id_fails() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        conn.phase = ConnectionPhase::authenticated(&jid);
        let frame = "<resume xmlns='urn:xmpp:sm:3' previd='does-not-exist' h='0'/>";
        let responses = handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(responses.len(), 1);
        let el = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(el.name(), "failed");
        // Must NOT mark the session as bound/resumed.
        assert!(conn.phase.is_authenticated());
        assert!(!conn.phase.is_ready());
        assert!(!conn.phase.is_resumed());
    }

    #[tokio::test]
    async fn sm_resume_signals_suppress_record_so_main_loop_skips_replay() {
        // Regression guard for the double-record bug reported in PR review:
        // `handle_sm_resume` must request suppression of outbound recording
        // for its own response batch. Replayed stanzas are already in the
        // unacked queue — re-recording them would bump `outbound_count` and
        // create duplicates.
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;

        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let stream_id = "stream-dup-check".to_string();
        let detached = DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            outbound_count: 2,
            last_acked: 0,
            unacked_stanzas: vec![
                (1, "<message id='m1'/>".to_string()),
                (2, "<message id='m2'/>".to_string()),
            ],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        };
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(detached)
            .await
            .expect("store");

        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::authenticated(&jid);
        let frame = format!(
            "<resume xmlns='urn:xmpp:sm:3' previd='{}' h='0'/>",
            stream_id
        );
        let _ = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

        // The resume handler must have raised the suppress flag so the main
        // loop skips re-recording its own response batch.
        assert!(
            conn.suppress_sm_record_next_batch,
            "handle_sm_resume must ask the main loop to skip SM recording for this batch"
        );
        // And the restored counters must still reflect what the client had
        // acknowledged, not the inflated post-re-record values (2, not 4).
        assert_eq!(conn.sm_state.outbound_count, 2);
        assert_eq!(conn.sm_state.queue_len(), 2);
    }

    #[tokio::test]
    async fn cleanup_shutdown_detaches_resumable_session_on_transport_drop() {
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "detached-channel@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");

        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &jid,
            "alice",
            &Some(owner_session),
        )
        .await;
        let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
        let owner = state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx);
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&jid, true, 0);
        state
            .deps
            .protocol
            .connection_registry
            .update_presence_state(
                &jid,
                Some("away".to_string()),
                Some("stepped out".to_string()),
                3,
            );

        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::ready(jid.clone(), false);
        conn.registry_owner = Some(owner);
        conn.roster_interested = true;
        conn.sm_state
            .enable("stream-detach".to_string(), true, Some(300));
        state
            .deps
            .protocol
            .connection_registry
            .send_to(
                &jid,
                Stanza::Presence(xmpp_parsers::presence::Presence::new(
                    xmpp_parsers::presence::Type::None,
                )),
            )
            .await;

        cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

        assert!(!state.deps.protocol.connection_registry.is_connected(&jid));
        let detached = state
            .deps
            .protocol
            .sm_session_registry
            .take_session("stream-detach")
            .await
            .expect("registry lookup");
        let detached = detached.expect("detached session");
        assert!(
            detached.roster_interested,
            "detached session must preserve roster-interest state"
        );
        assert!(
            detached.presence_available,
            "detached session must preserve available-presence state"
        );
        assert_eq!(
            detached.presence_show,
            Some(xmpp_parsers::presence::Show::Away)
        );
        assert_eq!(detached.presence_status.as_deref(), Some("stepped out"));
        assert_eq!(detached.presence_priority, 3);
        assert!(
            detached
                .unacked_stanzas
                .iter()
                .any(|(_, stanza)| stanza.contains("<presence")),
            "cleanup must record queued-but-unwritten outbound stanzas before detaching"
        );
        assert!(snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .find_nick_by_real_jid(&jid)
            .is_some());
    }

    #[tokio::test]
    async fn cleanup_shutdown_does_not_detach_explicit_close() {
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "closing-channel@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");

        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &jid,
            "alice",
            &Some(owner_session),
        )
        .await;
        let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
        let owner = state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx);

        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::ready(jid.clone(), false);
        conn.registry_owner = Some(owner);
        conn.sm_state
            .enable("stream-close".to_string(), false, Some(300));

        cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

        assert!(!state.deps.protocol.connection_registry.is_connected(&jid));
        let detached = state
            .deps
            .protocol
            .sm_session_registry
            .take_session("stream-close")
            .await
            .expect("registry lookup");
        assert!(
            detached.is_none(),
            "explicit <close/> must not leave a resumable detached session behind"
        );
        assert!(snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .find_nick_by_real_jid(&jid)
            .is_none());
    }

    #[tokio::test]
    async fn cleanup_shutdown_does_not_unregister_replacement_session() {
        let state = create_test_websocket_state().await;
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
        let (new_tx, _new_rx) = mpsc::channel::<OutboundStanza>(4);

        let old_owner = state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), old_tx);
        let new_owner = state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), new_tx);

        let mut old_conn = WsConnState::new();
        old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
        old_conn.registry_owner = Some(old_owner);

        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await;

        assert!(
            state.deps.protocol.connection_registry.is_connected(&jid),
            "cleanup for a replaced connection must leave the replacement registered"
        );
        assert!(
            state
                .deps
                .protocol
                .connection_registry
                .unregister_if_owner(&jid, &new_owner)
                .is_some(),
            "the remaining registry owner should be the replacement session"
        );
    }

    #[tokio::test]
    async fn sm_janitor_helper_drains_expired_and_cleans_muc() {
        // Exercise the pieces the janitor composes: drain_expired() returns
        // the removed sessions, and cleanup_muc_presence_for_jid removes the
        // occupant that was held while the session was detached.
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "expired-channel@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");

        // Put alice in the room, as if she'd detached with SM.
        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &jid,
            "alice",
            &Some(owner_session),
        )
        .await;
        assert!(snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .find_nick_by_real_jid(&jid)
            .is_some());

        // Seed an immediately-expired detached session for that JID.
        let stream_id = "already-expired".to_string();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.clone(),
                user_id: "alice@example.com".to_string(),
                jid: jid.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(0), // already expired
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .await
            .expect("store");
        state
            .deps
            .protocol
            .resumable_sessions
            .insert(stream_id.clone(), Session::new("uid", "alice", "alice"));

        // Wait a hair so the 0-second TTL is definitely in the past.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let drained = state
            .deps
            .protocol
            .sm_session_registry
            .drain_expired()
            .await
            .expect("drain");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].stream_id, stream_id);

        // The janitor body: remove sidecar + MUC occupant + any routing slot.
        state.deps.protocol.resumable_sessions.remove(&stream_id);
        state
            .deps
            .protocol
            .connection_registry
            .unregister(&drained[0].jid);
        cleanup_muc_presence_for_jid(state.as_ref(), &drained[0].jid).await;

        assert!(!state
            .deps
            .protocol
            .resumable_sessions
            .contains_key(&stream_id));
        assert!(
            snapshot_room(state.as_ref(), &room_jid)
                .await
                .room
                .find_nick_by_real_jid(&jid)
                .is_none(),
            "MUC occupant must be gone after janitor sweep"
        );
    }
}
