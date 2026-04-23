//! XMPP over WebSocket (RFC 7395)
//!
//! Provides WebSocket transport for XMPP, allowing all traffic over port 443.

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
use chrono::Utc;
use futures::{Sink, SinkExt, StreamExt};
use jid::{BareJid, FullJid, Jid};
use kameo::actor::ActorRef;
use std::{str::FromStr, sync::Arc};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use waddle_xmpp::{
    auth::{parse_oauthbearer, OAuthBearerResult},
    carbons::{build_received_carbon, build_sent_carbon, should_copy_message, CARBONS_NS},
    commands::{CommandContext, CommandRegistry, CommandResult},
    connection::Stanza,
    disco::{
        build_disco_info_response, build_disco_info_response_with_extensions,
        build_disco_items_response, muc_room_features, parse_disco_info_query,
        parse_disco_items_query, spaces_service_features, upload_service_features, DiscoItem,
        Feature, Identity,
    },
    inbox::{
        runtime::{
            direct_message_entry, filter_query, groupchat_entry, groupchat_thread_entry,
            preview_text, should_project_message,
        },
        storage::InboxStorage,
    },
    mam::{
        add_stanza_id as add_mam_stanza_id, build_fin_iq, build_result_messages, is_mam_query,
        parse_mam_query, ArchivedMessage, LibSqlMamStorage, MamStorage, STANZA_ID_NS,
    },
    muc::{
        room_actor::{
            BuildGroupchatBroadcast, GetSnapshot, JoinWithAffiliation, LeaveByRealJid, RoomActor,
        },
        room_registry_actor::{
            DestroyRoom, GetOrCreateRoom, GetRoom, IsMucJid, ListRooms, RoomRegistryActor,
        },
        RoomConfig,
    },
    protocol::{
        frame::{
            inject_client_ns_if_missing, parse_frame, InboundFrame, ParseError, MAX_FRAME_SIZE,
        },
        ConnectionPhase, ScramPendingState, StanzaContext as ProtocolStanzaContext,
        StanzaDispatcher,
    },
    registry::{BroadcastOutcome, ConnectionRegistry, OutboundStanza, SendResult},
    stream_management::{
        InMemorySmSessionRegistry, SmEnable, SmResume, SmSessionRegistry, SmStanza,
        StreamManagementState, SM_NS,
    },
    xep::{
        build_command_items, build_command_result, build_spaces_metadata_form, has_file_sharing,
        is_moderation_request_message, is_moderation_result_message, is_reaction_message,
        is_retraction_message, is_sticker_message, parse_command_from_iq, should_skip_storage,
        Command, CommandStatus, NODE_COMMANDS, NS_REPLY,
    },
    Affiliation, Role, StanzaErrorCondition, StanzaErrorType, WaddleDetails, XmppError,
};
use xmpp_parsers::message::MessageType as XmppMessageType;
use xmpp_parsers::minidom::Element;

use waddle_extensions::{message_has_embed_for_namespaces, ExtensionManager};

use super::auth::AuthState;
use crate::auth::{localpart_to_jid, NativeUserStore, Session};
use crate::server::routes::channels::{get_channel_from_db, list_channels_from_db};
use crate::server::routes::waddles::{
    get_waddle_by_id, list_all_waddles_from_db, list_user_waddles,
};
use crate::server::AppState;
use waddle_xmpp::auth::ScramServer;
use waddle_xmpp::pubsub::{
    build_pubsub_error, build_pubsub_items_result, build_pubsub_publish_result,
    build_pubsub_success, is_pubsub_iq, parse_pubsub_iq, PubSubError, PubSubRequest, PubSubStorage,
};
use waddle_xmpp::xep::xep0363::{
    build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
    parse_upload_request, sanitize_filename, UploadError, UploadSlot,
};
use waddle_xmpp::xep::xep0430::{
    build_inbox_push, build_inbox_query_result, build_mark_read_result, is_inbox_iq,
    parse_inbox_query, parse_mark_read,
};

/// WebSocket route dependencies kept narrower than the full server graph.
pub struct WebSocketState {
    pub deps: WebSocketDeps,
}

pub struct WebSocketDeps {
    /// Core app state for accessing the global and per-waddle databases.
    pub app_state: Arc<AppState>,
    /// Authentication state for session validation.
    pub auth_state: Arc<AuthState>,
    /// Protocol/runtime services used by the WebSocket C2S path.
    pub protocol: ProtocolServices,
}

pub struct ProtocolServices {
    /// Registry for tracking active connections by JID.
    pub connection_registry: Arc<ConnectionRegistry>,
    /// Actor-backed registry for MUC rooms.
    pub room_registry: ActorRef<RoomRegistryActor>,
    /// Shared XMPP MAM storage for archived message history.
    pub mam_storage: Arc<LibSqlMamStorage>,
    /// Shared XEP-0430 inbox projection storage.
    pub inbox_storage: Arc<dyn InboxStorage>,
    /// Registry for ad-hoc commands exposed over the WebSocket transport.
    pub command_registry: Arc<CommandRegistry>,
    /// Runtime extension manager for message embeds + feature advertisements.
    pub extension_manager: Arc<ExtensionManager>,
    /// Sans-I/O stanza dispatcher. Handlers migrated so far (ping, session,
    /// roster, carbons) are routed through this before falling back to the
    /// legacy string-matching code paths below.
    pub dispatcher: Arc<StanzaDispatcher>,
    /// Shared PubSub/PEP storage (XEP-0060/XEP-0163).
    pub pubsub_storage: Arc<dyn PubSubStorage>,
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

/// Per-connection mutable state threaded through the legacy dispatch path.
///
/// Holds the typed connection phase plus the remaining mutable transport/session
/// adjuncts that have not moved into the protocol state machine yet.
struct LegacyConnState {
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
    /// One-shot flag: when set, the main loop must NOT push the current
    /// frame's responses into `sm_state.record_outbound`. The flag is
    /// raised by `handle_sm_resume` because the responses it returns are
    /// replayed stanzas that were already pushed into the unacked queue
    /// before detach; re-recording them would double-count `outbound_count`
    /// and duplicate queue entries. The main loop resets the flag to
    /// `false` after skipping the record step.
    suppress_sm_record_next_batch: bool,
}

impl LegacyConnState {
    fn new() -> Self {
        Self {
            phase: ConnectionPhase::new(),
            authenticated_session: None,
            sm_state: StreamManagementState::new(),
            carbons_enabled: false,
            suppress_sm_record_next_batch: false,
        }
    }
}

/// Create the WebSocket router
pub fn router(state: Arc<WebSocketState>) -> Router {
    Router::new()
        .route("/xmpp-websocket", get(xmpp_websocket_handler))
        .with_state(state)
}

/// GET /xmpp-websocket
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
    let mut conn = LegacyConnState::new();
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
                        let responses = handle_xmpp_frame(
                            &text,
                            &domain,
                            &state,
                            &mut conn,
                        ).await;

                        // Register connection after successful authentication AND resource binding
                        // This ensures the JID in ConnectionRegistry matches the JID stored in MUC room occupants
                        if let Some(jid) = conn.phase.bound_jid() {
                            if let Some(tx) = pending_tx.take() {
                                state.deps.protocol.connection_registry.register_with_carbons(
                                    jid.clone(),
                                    tx,
                                    conn.carbons_enabled,
                                );
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
                        debug!("Received outbound stanza from registry");
                        let xml = stanza_to_xml(&outbound_stanza.stanza);
                        // Outbound stanzas routed from other connections are
                        // always iq/message/presence — count them into the
                        // SM outbound queue for replay if SM is enabled.
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
    cleanup_connection_shutdown(state.as_ref(), &conn, superseded).await;

    info!("XMPP WebSocket connection closed");
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
    conn: &LegacyConnState,
    superseded: bool,
) {
    // Short-circuit when this task was superseded: the registry and MUC
    // occupant slots now belong to the newer connection for this FullJid,
    // and any cleanup we do here would clobber the newcomer.
    if superseded {
        return;
    }

    let Some(jid) = conn.phase.cleanup_jid().cloned() else {
        return;
    };

    let should_detach_for_resume =
        conn.sm_state.is_resumable() && !matches!(conn.phase, ConnectionPhase::Closing { .. });

    if should_detach_for_resume {
        let carbons_enabled = conn.carbons_enabled;
        let user_id = conn
            .authenticated_session
            .as_ref()
            .map(|session| session.user_id.to_string())
            .unwrap_or_else(|| jid.to_bare().to_string());
        if let Some(detached) =
            conn.sm_state
                .to_detached_session(user_id, jid.clone(), carbons_enabled)
        {
            let stream_id = detached.stream_id.clone();
            if let Some(session) = conn.authenticated_session.clone() {
                state
                    .deps
                    .protocol
                    .resumable_sessions
                    .insert(stream_id.clone(), session);
            }
            match state
                .deps
                .protocol
                .sm_session_registry
                .store_session(detached)
                .await
            {
                Ok(()) => {
                    // Remove the routing entry only — the MUC occupant
                    // slot stays. On a successful resume we'll re-register
                    // the same FullJid and presence is preserved.
                    state.deps.protocol.connection_registry.unregister(&jid);
                    info!(
                        jid = %jid,
                        stream_id = %stream_id,
                        "SM session detached; awaiting resume"
                    );
                }
                Err(err) => {
                    warn!(jid = %jid, error = %err, "Failed to detach SM session; falling back to full cleanup");
                    state.deps.protocol.resumable_sessions.remove(&stream_id);
                    state.deps.protocol.connection_registry.unregister(&jid);
                    cleanup_muc_presence(state, &jid).await;
                }
            }
            return;
        }
    }

    state.deps.protocol.connection_registry.unregister(&jid);
    info!(jid = %jid, "WebSocket connection unregistered");
    cleanup_muc_presence(state, &jid).await;
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

async fn get_room_actor(state: &WebSocketState, room_jid: &BareJid) -> Option<ActorRef<RoomActor>> {
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

async fn get_or_create_room_actor(
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

async fn list_room_jids(state: &WebSocketState) -> Vec<BareJid> {
    match state.deps.protocol.room_registry.ask(ListRooms).await {
        Ok(rooms) => rooms,
        Err(error) => {
            warn!(error = ?error, "Failed to list room actors");
            Vec::new()
        }
    }
}

async fn is_muc_room_jid(state: &WebSocketState, room_jid: &BareJid) -> bool {
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

async fn destroy_room_actor(state: &WebSocketState, room_jid: &BareJid) -> bool {
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
fn stanza_to_xml(stanza: &Stanza) -> String {
    let mut element = stanza.to_element();
    if let Stanza::Message(message) = stanza {
        // xmpp_parsers currently drops RFC 6121 <thread/> during Message -> Element.
        // Re-attach it so forwarded/broadcast messages preserve thread metadata.
        if let Some(thread) = message.thread.as_ref() {
            let has_thread = element.children().any(|child| child.name() == "thread");
            if !has_thread {
                element.append_child(
                    Element::builder("thread", "jabber:client")
                        .append(thread.0.clone())
                        .build(),
                );
            }
        }
    }
    element_to_xml(element)
}

fn element_to_xml(element: xmpp_parsers::minidom::Element) -> String {
    let mut buf = Vec::new();
    // write_to cannot fail on a Vec<u8> in practice.
    element
        .write_to(&mut buf)
        .expect("serializing stanza to Vec<u8> should not fail");
    String::from_utf8(buf).expect("xmpp_parsers serializes valid UTF-8")
}

fn archived_stanza_xml(message: &xmpp_parsers::message::Message) -> String {
    stanza_to_xml(&Stanza::Message(message.clone()))
}

fn iq_to_xml(iq: xmpp_parsers::iq::Iq) -> String {
    stanza_to_xml(&Stanza::Iq(iq))
}

fn build_iq_result_xml(
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    payload: Option<xmpp_parsers::minidom::Element>,
) -> String {
    let mut iq = xmpp_parsers::minidom::Element::builder("iq", "jabber:client")
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

fn build_iq_error_xml_with_addresses(
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

fn build_iq_error_xml(id: &str, error_type: &str, condition: &str) -> String {
    build_iq_error_xml_with_addresses(id, None, None, error_type, condition)
}

fn build_stream_features_xml(authenticated: bool) -> String {
    let mut features = Element::builder("features", waddle_xmpp::ns::STREAM);
    if authenticated {
        features = features.append(Element::builder("bind", waddle_xmpp::ns::BIND).build());
        // XEP-0198: advertise stream management so clients can <enable/> it
        // after bind or <resume/> a detached session.
        features = features.append(Element::builder("sm", SM_NS).build());
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

/// Bundle the session-level borrows that XEP-0198 control handlers mutate.
/// Passed through `handle_sm_stanza` and its helpers so each signature stays
/// below the clippy too-many-arguments threshold.
struct SmCtx<'a> {
    phase: &'a mut ConnectionPhase,
    sm_state: &'a mut StreamManagementState,
    authenticated_session: &'a mut Option<Session>,
    carbons_enabled: &'a mut bool,
    /// Set by `handle_sm_resume` so the main loop skips SM recording for
    /// the responses it returns — those are replay stanzas already tracked
    /// in the unacked queue.
    suppress_sm_record_next_batch: &'a mut bool,
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
        suppress_sm_record_next_batch,
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
        .peek_session(&resume.previd)
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
            return vec![SmFailed::with_condition("not-authorized").to_xml()];
        }
    }

    let preserve_authenticated_session = matches!(phase, ConnectionPhase::Authenticated { .. });

    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .take_session(&resume.previd)
        .await
    {
        Ok(Some(session)) => session,
        Ok(None) => {
            info!(stream_id = %resume.previd, "SM resume rejected: session disappeared before take");
            return vec![SmFailed::with_condition("item-not-found").to_xml()];
        }
        Err(e) => {
            warn!(stream_id = %resume.previd, error = %e, "SM resume failed: registry error");
            return vec![SmFailed::with_condition("internal-server-error").to_xml()];
        }
    };

    // Restore SM counters + the unacked queue.
    sm_state.restore_from_session(&detached);
    // The client tells us how many of OUR outbound stanzas they've actually
    // handled. Acknowledge up to that point so the replay set is minimal.
    sm_state.acknowledge(resume.h);

    // Restore authentication identity. If the detached sidecar has no
    // matching Session (TTL expired / crash), a resume from Unauthenticated
    // proceeds JID-only; a resume from Authenticated keeps the fresh
    // transport's current Session context.
    let restored_session = state
        .deps
        .protocol
        .resumable_sessions
        .remove(&resume.previd)
        .map(|(_, s)| s);
    if restored_session.is_none() {
        if preserve_authenticated_session {
            warn!(
                stream_id = %resume.previd,
                jid = %detached.jid,
                "SM resumed without cached detached Session; preserving current authenticated Session"
            );
        } else {
            warn!(
                stream_id = %resume.previd,
                jid = %detached.jid,
                "SM resumed without cached Session; authorization context is thinner than expected"
            );
        }
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
    conn: &mut LegacyConnState,
) -> Vec<String> {
    if frame.len() > MAX_FRAME_SIZE {
        warn!(len = frame.len(), "Dropping oversized XMPP frame");
        return vec![];
    }

    let LegacyConnState {
        phase,
        authenticated_session,
        sm_state,
        carbons_enabled,
        suppress_sm_record_next_batch,
    } = conn;
    let muc_domain = format!("muc.{}", domain);

    // SM nonzas (enable/resume/r/a) are not part of the parse_frame typed
    // vocabulary — keep the direct SmStanza check before parse_frame.
    if SmStanza::is_client_nonza_candidate(frame) {
        if let Some(sm) = SmStanza::parse(frame) {
            let ctx = SmCtx {
                phase,
                sm_state,
                authenticated_session,
                carbons_enabled,
                suppress_sm_record_next_batch,
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
                    handle_iq_with_conn_state(
                        iq,
                        domain,
                        &muc_domain,
                        state,
                        authenticated_session,
                        phase,
                        carbons_enabled,
                    )
                    .await
                }

                Stanza::Presence(presence) => {
                    handle_presence(
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
                    handle_message(message, &muc_domain, state, phase, authenticated_session).await
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
        NativeUserStore::new(Arc::new(state.deps.app_state.db_pool.global().clone()));

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

/// Handle presence stanzas including MUC join/leave
async fn handle_presence(
    presence: xmpp_parsers::presence::Presence,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    let to = presence.to.as_ref().map(|jid| jid.to_string());
    let is_unavailable = presence.type_ == xmpp_parsers::presence::Type::Unavailable;

    // Check if this is a MUC presence (to room@muc.domain/nick)
    if let Some(ref to_jid) = to {
        if to_jid.contains(muc_domain) {
            let parts: Vec<&str> = to_jid.split('/').collect();
            let room_jid_str = parts.first().copied().unwrap_or(to_jid);
            let nick = parts.get(1).copied().unwrap_or("anonymous");

            let Ok(room_jid) = room_jid_str.parse::<BareJid>() else {
                warn!(room = %room_jid_str, "Invalid room JID");
                return vec![];
            };

            let Some(sender_jid) = phase.bound_jid() else {
                warn!("MUC presence without authenticated session");
                return vec![];
            };

            if is_unavailable {
                return handle_muc_leave(state, &room_jid, sender_jid, nick).await;
            }

            return handle_muc_join(
                state,
                domain,
                &room_jid,
                sender_jid,
                nick,
                _authenticated_session,
            )
            .await;
        }
    }

    debug!("Presence stanza received");
    vec![]
}

/// Handle MUC room join
async fn handle_muc_join(
    state: &WebSocketState,
    domain: &str,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC join request");

    let existing_room_actor = get_room_actor(state, room_jid).await;
    let (room_actor, created_instant_room) = match existing_room_actor {
        Some(actor) => (actor, false),
        None => {
            let managed_channel = get_managed_channel_for_room(state, room_jid).await;
            let config = managed_channel
                .as_ref()
                .map(|channel| RoomConfig {
                    name: channel.name.clone(),
                    description: channel.description.clone(),
                    members_only: false,
                    forum: channel.channel_type == "forum",
                    ..Default::default()
                })
                .unwrap_or_else(|| RoomConfig {
                    name: room_jid
                        .node()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "Room".to_string()),
                    members_only: false,
                    ..Default::default()
                });

            let (waddle_id, channel_id) = managed_channel
                .as_ref()
                .map(|channel| (channel.waddle_id.clone(), channel.id.clone()))
                .unwrap_or_else(|| parse_room_jid_context(room_jid));

            let Some(actor) =
                get_or_create_room_actor(state, room_jid, config, waddle_id, channel_id).await
            else {
                return vec![];
            };
            (actor, managed_channel.is_none())
        }
    };

    let effective_affiliation = if created_instant_room {
        Affiliation::Owner
    } else {
        Affiliation::Member
    };

    let join_outcome = match room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: nick.to_string(),
            effective_affiliation,
            local_domain: domain.to_string(),
        })
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let nick_collision = matches!(
                &error,
                kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::NickAlreadyInUse(_)
                )
            );
            if nick_collision {
                warn!(
                    room = %room_jid,
                    nick = %nick,
                    sender = %sender_jid,
                    "MUC nick collision; returning conflict"
                );
                return vec![build_muc_conflict_presence_xml(room_jid, nick, sender_jid)];
            }
            warn!(room = %room_jid, nick = %nick, error = ?error, "Failed to join MUC room");
            return vec![];
        }
    };

    let occupant_count = join_outcome.occupant_count;

    info!(room = %room_jid, nick = %nick, occupants = occupant_count, "User joined MUC room");

    let mut responses = Vec::new();

    // Replay one occupant presence per nick to the joiner. Same-bare multi-session
    // joins must not turn into duplicate room occupants on the wire.
    let mut replayed_nicks = std::collections::HashSet::new();
    for existing in join_outcome
        .existing_occupants
        .iter()
        .filter(|existing| existing.nick != nick)
        .filter(|existing| replayed_nicks.insert(existing.nick.clone()))
    {
        responses.push(build_muc_join_presence_xml(
            room_jid,
            &existing.nick,
            sender_jid,
            affiliation_str(existing.affiliation),
            role_str(existing.role),
            &existing.jid,
            false,
        ));
    }

    // Broadcast the new occupant's presence to all existing occupants.
    // Non-blocking: a zombied/slow consumer must never stall the join path,
    // which is how "Timed out waiting for self-presence" cascades start.
    // Drop accounting is handled inside `try_send_to` (logs + metrics);
    // per-occupant outcome is discarded here because a missed join
    // presence self-heals via the next MUC presence/probe round-trip.
    if !join_outcome.is_same_bare_multi_session_join {
        for existing in &join_outcome.existing_occupants {
            let presence_stanza =
                create_presence_stanza(room_jid, nick, sender_jid, &existing.jid, false);
            let stanza = Stanza::Presence(presence_stanza);
            let _outcome = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&existing.jid, stanza);
        }
    }

    // Send self-presence to the joining user (with status code 110)
    responses.push(build_muc_join_presence_xml(
        room_jid,
        nick,
        sender_jid,
        affiliation_str(join_outcome.new_occupant_affiliation),
        role_str(join_outcome.new_occupant_role),
        sender_jid,
        true,
    ));

    // Send room subject
    let room_name = room_jid
        .node()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "Waddle".to_string());
    responses.push(build_muc_subject_message_xml(
        room_jid, sender_jid, &room_name,
    ));

    responses
}

/// Handle MUC room leave
async fn handle_muc_leave(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave request");

    let Some(room_actor) = get_room_actor(state, room_jid).await else {
        debug!(room = %room_jid, "Room not found for leave");
        return vec![build_muc_self_unavailable_xml(room_jid, nick, sender_jid)];
    };

    let outcome = match room_actor
        .ask(LeaveByRealJid {
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            debug!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave for absent occupant");
            return vec![build_muc_self_unavailable_xml(room_jid, nick, sender_jid)];
        }
        Err(error) => {
            warn!(room = %room_jid, nick = %nick, sender = %sender_jid, error = ?error, "Failed to leave MUC room");
            return vec![build_muc_self_unavailable_xml(room_jid, nick, sender_jid)];
        }
    };

    // Broadcast unavailable presence to remaining occupants (non-blocking).
    // Drop accounting is handled inside `try_send_to`.
    if outcome.removed_last_session {
        for occupant_jid in &outcome.remaining_occupants {
            let from_jid = room_jid
                .clone()
                .with_resource_str(&outcome.nick)
                .unwrap_or_else(|_| sender_jid.clone());
            let mut presence =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
            presence.from = Some(jid::Jid::from(from_jid));
            presence.to = Some(jid::Jid::from(occupant_jid.clone()));
            let stanza = Stanza::Presence(presence);
            let _outcome = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(occupant_jid, stanza);
        }
    }

    vec![build_muc_self_unavailable_xml(
        room_jid,
        &outcome.nick,
        sender_jid,
    )]
}

/// Handle IQ stanzas
async fn handle_iq(
    frame: &str,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
) -> Vec<String> {
    let mut carbons_enabled = phase.bound_jid().is_some_and(|jid| {
        state
            .deps
            .protocol
            .connection_registry
            .is_carbons_enabled(jid)
    });

    let iq = match parse_frame(frame) {
        Ok(InboundFrame::Stanza(stanza)) => match *stanza {
            Stanza::Iq(iq) => iq,
            _ => return vec![],
        },
        _ => return vec![],
    };

    handle_iq_with_conn_state(
        iq,
        domain,
        muc_domain,
        state,
        authenticated_session,
        phase,
        &mut carbons_enabled,
    )
    .await
}

async fn handle_iq_with_conn_state(
    iq: xmpp_parsers::iq::Iq,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
    carbons_enabled: &mut bool,
) -> Vec<String> {
    let spaces_domain = format!("spaces.{domain}");
    let single_tenant = std::env::var("WADDLE_SINGLE_TENANT")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);

    let id = iq.id.clone();
    let to = iq.to.as_ref().map(|jid| jid.to_string());
    let from = iq.from.as_ref().map(|jid| jid.to_string());
    let response_from = to.as_deref();
    let response_to = from.as_deref();

    if matches!(
        &iq.payload,
        xmpp_parsers::iq::IqType::Result(_) | xmpp_parsers::iq::IqType::Error(_)
    ) {
        debug!(id = %id, "Ignoring IQ result/error stanza");
        return vec![];
    }

    let payload_ns = match &iq.payload {
        xmpp_parsers::iq::IqType::Get(e) | xmpp_parsers::iq::IqType::Set(e) => e.ns(),
        _ => String::new(),
    };
    let has_destroy = match &iq.payload {
        xmpp_parsers::iq::IqType::Set(e) => e
            .get_child("destroy", "http://jabber.org/protocol/muc#owner")
            .is_some(),
        _ => false,
    };

    // Sans-I/O dispatch: if the IQ namespace has a registered handler in
    // the protocol dispatcher, route through it and translate the emitted
    // OutboundEvents into outbound XML frames via `interpret()`.
    //
    // Handlers that still need async I/O (for example MAM, Jingle, disco,
    // and any other namespaces not yet registered with the dispatcher)
    // continue to fall through to the legacy string-matching branches
    // below until the two-phase async callback machinery lands.
    let carbons_toggle = match &iq.payload {
        xmpp_parsers::iq::IqType::Set(e)
            if e.ns() == CARBONS_NS && (e.name() == "enable" || e.name() == "disable") =>
        {
            Some(e.name() == "enable")
        }
        _ => None,
    };
    if state
        .deps
        .protocol
        .dispatcher
        .has_iq_handler(payload_ns.as_str())
    {
        let Some(full_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "auth",
                "not-authorized",
            )];
        };
        if let Some(enabled) = carbons_toggle {
            *carbons_enabled = enabled;
            let _ = state
                .deps
                .protocol
                .connection_registry
                .set_carbons_enabled(full_jid, enabled);
        }
        let ctx = ProtocolStanzaContext { domain, full_jid };
        let events = state.deps.protocol.dispatcher.dispatch_iq(&iq, &ctx);
        let outcome = super::interpret::interpret(events).await;
        if outcome.close {
            warn!(
                ns = %payload_ns,
                "Sans-I/O handler requested transport close; \
                 WebSocket adapter cannot honour CloseTransport yet"
            );
        }
        return outcome.frames;
    }

    // jabber:iq:roster is now served by protocol::handlers::roster::RosterHandler
    // through the sans-I/O dispatcher short-circuit above.

    // Disco info on MUC service
    if payload_ns == "http://jabber.org/protocol/disco#info" {
        let request_iq = &iq;
        let query = match parse_disco_info_query(request_iq) {
            Ok(query) => query,
            Err(_) => return vec![build_iq_error_xml(&id, "modify", "bad-request")],
        };

        if to.as_deref() == Some(muc_domain) {
            let identities = vec![Identity::muc_service(Some("Waddle Chatrooms"))];
            let mut features = vec![Feature::muc(), Feature::replies()];
            features.extend(
                state
                    .deps
                    .protocol
                    .extension_manager
                    .extension_features()
                    .into_iter()
                    .map(|ns| Feature::new(&ns)),
            );
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        // Disco info on a specific room
        if let Some(target) = to.as_deref() {
            let room_target = target.split('/').next().unwrap_or(target);
            if let Ok(room_jid) = room_target.parse::<BareJid>() {
                if let Some(room_actor) = get_room_actor(state, &room_jid).await {
                    let snapshot = match room_actor.ask(GetSnapshot).await {
                        Ok(snapshot) => snapshot.room,
                        Err(error) => {
                            warn!(
                                room = %room_jid,
                                error = ?error,
                                "Failed to load room snapshot for disco#info"
                            );
                            return vec![build_iq_error_xml_with_addresses(
                                &id,
                                response_from,
                                response_to,
                                "wait",
                                "internal-server-error",
                            )];
                        }
                    };
                    let identities = vec![Identity::muc_room(Some(&snapshot.config.name))];
                    let mut features = muc_room_features(
                        snapshot.config.persistent,
                        snapshot.config.members_only,
                        snapshot.config.moderated,
                        snapshot.config.forum,
                    );
                    features.extend(
                        state
                            .deps
                            .protocol
                            .extension_manager
                            .extension_features()
                            .into_iter()
                            .map(|ns| Feature::new(&ns)),
                    );
                    let response =
                        build_disco_info_response(request_iq, &identities, &features, None);
                    return vec![iq_to_xml(response)];
                }

                if is_muc_room_jid(state, &room_jid).await {
                    if let Some(channel) = get_managed_channel_for_room(state, &room_jid).await {
                        let identities = vec![Identity::muc_room(Some(&channel.name))];
                        let mut features =
                            muc_room_features(true, false, false, channel.channel_type == "forum");
                        features.extend(
                            state
                                .deps
                                .protocol
                                .extension_manager
                                .extension_features()
                                .into_iter()
                                .map(|ns| Feature::new(&ns)),
                        );
                        let response =
                            build_disco_info_response(request_iq, &identities, &features, None);
                        return vec![iq_to_xml(response)];
                    }

                    let room_name = room_jid
                        .node()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "Room".to_string());
                    let identities = vec![Identity::muc_room(Some(&room_name))];
                    let mut features = muc_room_features(false, false, false, false);
                    features.extend(
                        state
                            .deps
                            .protocol
                            .extension_manager
                            .extension_features()
                            .into_iter()
                            .map(|ns| Feature::new(&ns)),
                    );
                    let response =
                        build_disco_info_response(request_iq, &identities, &features, None);
                    return vec![iq_to_xml(response)];
                }
            }
        }

        if to.as_deref() == Some(domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let identities = vec![Identity::automation(Some("Ad-Hoc Commands"))];
            let features = vec![
                Feature::disco_info(),
                Feature::disco_items(),
                Feature::commands(),
            ];
            let response =
                build_disco_info_response(request_iq, &identities, &features, Some(NODE_COMMANDS));
            return vec![iq_to_xml(response)];
        }

        // Disco info on spaces service
        if to.as_deref() == Some(spaces_domain.as_str()) {
            if let Some(node) = query.node.as_deref() {
                let waddle =
                    match get_waddle_by_id(state.deps.app_state.db_pool.global(), node).await {
                        Ok(Some(waddle)) => waddle,
                        Ok(None) => {
                            return vec![build_iq_error_xml(&id, "cancel", "item-not-found")];
                        }
                        Err(err) => {
                            warn!(
                                node = %node,
                                error = %err,
                                "Failed to load space node for disco#info"
                            );
                            return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                        }
                    };

                let is_member = if single_tenant {
                    true
                } else if let Some(session) = authenticated_session {
                    match list_user_waddles(
                        state.deps.app_state.db_pool.global(),
                        &session.user_id,
                        200,
                        0,
                    )
                    .await
                    {
                        Ok(waddles) => waddles.iter().any(|candidate| candidate.id == node),
                        Err(err) => {
                            warn!(
                                user_id = %session.user_id,
                                node = %node,
                                error = %err,
                                "Failed membership check for space node disco#info"
                            );
                            false
                        }
                    }
                } else {
                    false
                };

                if !single_tenant && !is_member && !waddle.is_public {
                    return vec![build_iq_error_xml(&id, "cancel", "item-not-found")];
                }

                let identities = vec![Identity::pubsub_leaf(Some(&waddle.name))];
                let features = vec![
                    Feature::disco_info(),
                    Feature::pubsub(),
                    Feature::pubsub_retrieve_items(),
                    Feature::spaces(),
                ];
                let metadata = build_spaces_metadata_form(&WaddleDetails {
                    id: waddle.id.clone(),
                    name: waddle.name.clone(),
                    description: waddle.description.clone(),
                    owner_id: waddle.owner_user_id.clone(),
                    icon_url: waddle.icon_url.clone(),
                    is_public: waddle.is_public,
                    created_at: waddle.created_at.clone(),
                });
                let response = build_disco_info_response_with_extensions(
                    request_iq,
                    &identities,
                    &features,
                    Some(node),
                    &[metadata],
                );
                return vec![iq_to_xml(response)];
            }

            let identities = vec![Identity::spaces_service(Some("Spaces"))];
            let features = spaces_service_features();
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        // Disco info on upload service (XEP-0363)
        let upload_domain = format!("upload.{domain}");
        if to.as_deref() == Some(upload_domain.as_str()) {
            let identities = vec![Identity::upload_service(Some("HTTP File Upload"))];
            let features = upload_service_features();
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        // Disco info on server
        let identities = vec![Identity::server(Some("Waddle"))];
        let mut features = vec![
            Feature::ping(),
            Feature::replies(),
            Feature::disco_info(),
            Feature::disco_items(),
            Feature::commands(),
            Feature::spaces(),
        ];
        features.extend(
            state
                .deps
                .protocol
                .extension_manager
                .extension_features()
                .into_iter()
                .map(|ns| Feature::new(&ns)),
        );
        let response = build_disco_info_response(request_iq, &identities, &features, None);
        return vec![iq_to_xml(response)];
    }

    // Disco items - list services/rooms
    if payload_ns == "http://jabber.org/protocol/disco#items" {
        let request_iq = &iq;
        let query = match parse_disco_items_query(request_iq) {
            Ok(query) => query,
            Err(_) => return vec![build_iq_error_xml(&id, "modify", "bad-request")],
        };

        if to.as_deref() == Some(muc_domain) {
            debug!("Disco items query on MUC service");
            let mut rooms = list_room_jids(state).await;
            rooms.sort_by_key(|room| room.to_string());

            let items: Vec<DiscoItem> = if rooms.is_empty() {
                let lobby_jid = format!("lobby@{muc_domain}");
                vec![DiscoItem::muc_room(&lobby_jid, "Lobby")]
            } else {
                rooms
                    .into_iter()
                    .map(|room_jid| {
                        let room_jid_string = room_jid.to_string();
                        let name = room_jid
                            .node()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| room_jid_string.clone());
                        DiscoItem::muc_room(&room_jid_string, &name)
                    })
                    .collect()
            };

            let response = build_disco_items_response(request_iq, &items, None);
            return vec![iq_to_xml(response)];
        }

        if to.as_deref() == Some(domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs: Vec<(&str, &str)> = commands
                .iter()
                .map(|(node, name)| (node.as_str(), name.as_str()))
                .collect();
            let response = build_command_items(request_iq, &command_refs, domain);
            return vec![iq_to_xml(response)];
        }

        if to.as_deref() == Some(spaces_domain.as_str()) {
            let global_db = state.deps.app_state.db_pool.global();
            let items: Vec<DiscoItem> = match query.node.as_deref() {
                Some(node) => {
                    let can_list_channels = if single_tenant {
                        true
                    } else if let Some(session) = authenticated_session {
                        match list_user_waddles(global_db, &session.user_id, 200, 0).await {
                            Ok(waddles) => waddles.iter().any(|w| w.id == node),
                            Err(err) => {
                                warn!(
                                    user_id = %session.user_id,
                                    node = %node,
                                    error = %err,
                                    "Failed membership check for spaces node discovery"
                                );
                                false
                            }
                        }
                    } else {
                        false
                    };

                    if !can_list_channels {
                        vec![]
                    } else {
                        match state.deps.app_state.db_pool.get_waddle_db(node).await {
                            Ok(waddle_db) => {
                                match list_channels_from_db(&waddle_db, node, 200, 0).await {
                                    Ok(channels) => {
                                        channels
                                            .into_iter()
                                            .filter_map(|channel| {
                                                waddle_xmpp::managed_room_jid(
                                                    node,
                                                    &channel.id,
                                                    muc_domain,
                                                )
                                                .ok()
                                                .map(|room_jid| {
                                                    DiscoItem::muc_room(
                                                        &room_jid.to_string(),
                                                        &channel.name,
                                                    )
                                                })
                                            })
                                            .collect()
                                    }
                                    Err(err) => {
                                        warn!(
                                            node = %node,
                                            error = %err,
                                            "Failed to list channels for spaces node discovery"
                                        );
                                        vec![]
                                    }
                                }
                            }
                            Err(err) => {
                                warn!(
                                    node = %node,
                                    error = %err,
                                    "Failed to open waddle database for spaces node discovery"
                                );
                                vec![]
                            }
                        }
                    }
                }
                None => {
                    let waddles = if single_tenant {
                        match list_all_waddles_from_db(global_db, 1, 0).await {
                            Ok(rows) => rows,
                            Err(err) => {
                                warn!(error = %err, "Failed to list canonical single-tenant space");
                                vec![]
                            }
                        }
                    } else if let Some(session) = authenticated_session {
                        const PAGE_SIZE: usize = 200;
                        let mut offset = 0usize;
                        let mut all = Vec::new();

                        loop {
                            match list_user_waddles(global_db, &session.user_id, PAGE_SIZE, offset)
                                .await
                            {
                                Ok(page) => {
                                    let count = page.len();
                                    all.extend(page);
                                    if count < PAGE_SIZE {
                                        break;
                                    }
                                    offset += PAGE_SIZE;
                                }
                                Err(err) => {
                                    warn!(
                                        user_id = %session.user_id,
                                        error = %err,
                                        "Failed to list user spaces for discovery"
                                    );
                                    break;
                                }
                            }
                        }

                        all
                    } else {
                        vec![]
                    };

                    waddles
                        .into_iter()
                        .map(|w| DiscoItem::spaces_node(&spaces_domain, &w.id, Some(&w.name)))
                        .collect()
                }
            };

            let response = build_disco_items_response(request_iq, &items, query.node.as_deref());
            return vec![iq_to_xml(response)];
        }

        debug!("Disco items query on server");
        let upload_domain = format!("upload.{domain}");
        let items = vec![
            DiscoItem::muc_service(muc_domain, Some("Chatrooms")),
            DiscoItem::upload_service(&upload_domain, Some("HTTP File Upload")),
            DiscoItem::spaces_service(&spaces_domain, Some("Spaces")),
        ];
        let response = build_disco_items_response(request_iq, &items, None);
        return vec![iq_to_xml(response)];
    }

    if payload_ns == "http://jabber.org/protocol/commands" {
        return handle_command_iq(&iq, state, authenticated_session, phase.bound_jid()).await;
    }

    // MUC owner IQ (XEP-0045): instant room config submit and room destroy.
    // This is needed for clients that create a room by:
    // 1) joining via presence
    // 2) submitting an empty owner form (`jabber:x:data` type='submit')
    if payload_ns == "http://jabber.org/protocol/muc#owner" {
        let Some(target) = to.as_deref() else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "modify",
                "bad-request",
            )];
        };

        let room_target = target.split('/').next().unwrap_or(target);
        let Ok(room_jid) = room_target.parse::<BareJid>() else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "modify",
                "jid-malformed",
            )];
        };

        if !is_muc_room_jid(state, &room_jid).await {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "cancel",
                "item-not-found",
            )];
        }

        if has_destroy {
            if destroy_room_actor(state, &room_jid).await {
                debug!(room = %room_jid, "Destroyed MUC room via owner IQ");
                let room_jid_string = room_jid.to_string();
                return vec![build_iq_result_xml(
                    &id,
                    Some(room_jid_string.as_str()),
                    response_to,
                    None,
                )];
            }

            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "cancel",
                "item-not-found",
            )];
        }

        // Treat all other owner IQ sets as successful config submit for instant rooms.
        let room_jid_string = room_jid.to_string();
        return vec![build_iq_result_xml(
            &id,
            Some(room_jid_string.as_str()),
            response_to,
            None,
        )];
    }

    // MAM (Message Archive Management) query
    if is_mam_query(&iq) {
        let request_iq = &iq;
        let Some(target) = request_iq.to.as_ref().map(|jid| jid.to_string()) else {
            return vec![build_iq_error_xml(&id, "modify", "bad-request")];
        };

        let room_target = target.split('/').next().unwrap_or(target.as_str());
        let Ok(target_bare) = room_target.parse::<BareJid>() else {
            return vec![build_iq_error_xml(&id, "modify", "jid-malformed")];
        };

        // Determine whether this is a personal archive query (to=self) or a
        // MUC room archive query. Personal queries are allowed only when the
        // bound session identity matches the requested bare JID.
        let sender_bare = phase.bound_jid().map(|jid| jid.to_bare());

        let is_personal = sender_bare
            .as_ref()
            .is_some_and(|bare| *bare == target_bare);

        if !is_personal && !is_muc_room_jid(state, &target_bare).await {
            return vec![build_iq_error_xml(&id, "cancel", "item-not-found")];
        }

        let (query_id, query) = match parse_mam_query(request_iq) {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(error = %err, target = %target_bare, "Invalid MAM query");
                return vec![build_iq_error_xml(&id, "modify", "bad-request")];
            }
        };

        let archive_jid = target_bare.to_string();
        let mut result = match state
            .deps
            .protocol
            .mam_storage
            .query_messages(archive_jid.as_str(), &query)
            .await
        {
            Ok(result) => result,
            Err(err) => {
                warn!(error = %err, target = %target_bare, "MAM query failed");
                return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
            }
        };

        result.count = state
            .deps
            .protocol
            .mam_storage
            .count_messages(archive_jid.as_str())
            .await
            .ok();

        let recipient_jid = request_iq
            .from
            .as_ref()
            .map(|jid| jid.to_string())
            .or_else(|| phase.bound_jid().map(ToString::to_string))
            .unwrap_or_else(|| "unknown@localhost".to_string());

        let mut responses: Vec<String> =
            build_result_messages(&query_id, recipient_jid.as_str(), &result.messages)
                .into_iter()
                .map(|message| stanza_to_xml(&Stanza::Message(message)))
                .collect();
        responses.push(iq_to_xml(build_fin_iq(request_iq, &result)));
        return responses;
    }

    if is_inbox_iq(&iq) {
        let request_iq = &iq;
        let Some(user_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml(&id, "auth", "not-authorized")];
        };

        match &request_iq.payload {
            xmpp_parsers::iq::IqType::Get(_) => {
                let query = match parse_inbox_query(request_iq) {
                    Ok(query) => query,
                    Err(error) => {
                        warn!(error = %error, "Invalid inbox query");
                        return vec![build_iq_error_xml(&id, "modify", "bad-request")];
                    }
                };
                let entries = if query.threads {
                    if let Some(room) = &query.room {
                        match state
                            .deps
                            .protocol
                            .inbox_storage
                            .list_threads(&user_jid, room)
                            .await
                        {
                            Ok(entries) => entries,
                            Err(error) => {
                                warn!(error = %error, jid = %user_jid, "Failed to list thread inbox");
                                return vec![build_iq_error_xml(
                                    &id,
                                    "wait",
                                    "internal-server-error",
                                )];
                            }
                        }
                    } else {
                        return vec![build_iq_error_xml(&id, "modify", "bad-request")];
                    }
                } else {
                    match state.deps.protocol.inbox_storage.list(&user_jid).await {
                        Ok(entries) => entries,
                        Err(error) => {
                            warn!(error = %error, jid = %user_jid, "Failed to list inbox");
                            return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                        }
                    }
                };
                let total_unread = match state
                    .deps
                    .protocol
                    .inbox_storage
                    .total_unread(&user_jid)
                    .await
                {
                    Ok(total_unread) => total_unread,
                    Err(error) => {
                        warn!(error = %error, jid = %user_jid, "Failed to count inbox unread");
                        return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                    }
                };
                let response = build_inbox_query_result(
                    request_iq,
                    &filter_query(entries, &query),
                    total_unread,
                );
                return vec![iq_to_xml(response)];
            }
            xmpp_parsers::iq::IqType::Set(_) => {
                let mark_read = match parse_mark_read(request_iq) {
                    Ok(mark_read) => mark_read,
                    Err(error) => {
                        warn!(error = %error, "Invalid inbox mark-read");
                        return vec![build_iq_error_xml(&id, "modify", "bad-request")];
                    }
                };
                if let Err(error) = state
                    .deps
                    .protocol
                    .inbox_storage
                    .mark_read(
                        &user_jid,
                        &mark_read.partner,
                        mark_read.thread_id.as_deref(),
                    )
                    .await
                {
                    warn!(error = %error, jid = %user_jid, partner = %mark_read.partner, "Failed to mark inbox read");
                    return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                }
                return vec![iq_to_xml(build_mark_read_result(request_iq))];
            }
            _ => return vec![build_iq_error_xml(&id, "modify", "bad-request")],
        }
    }

    // urn:xmpp:carbons:2 enable/disable is now served by
    // protocol::handlers::carbons::CarbonsHandler via the short-circuit above.

    // XEP-0363: HTTP File Upload slot request
    if payload_ns == "urn:xmpp:http:upload:0" {
        let request_iq = &iq;
        if is_upload_request(request_iq) {
            let Some(sender_jid) = phase.bound_jid() else {
                return vec![build_iq_error_xml(&id, "auth", "not-authorized")];
            };
            let request = match parse_upload_request(request_iq) {
                Ok(req) => req,
                Err(e) => {
                    return vec![build_upload_error(&id, &e)];
                }
            };

            // Check file size limits (default 10 MB)
            let max_size: u64 = std::env::var("WADDLE_MAX_UPLOAD_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10 * 1024 * 1024);

            if request.size > max_size {
                return vec![build_upload_error(
                    &id,
                    &UploadError::FileTooLarge { max_size },
                )];
            }

            let safe_filename = sanitize_filename(&request.filename);
            let content_type = effective_content_type(request.content_type.as_deref()).to_string();
            let slot_id = uuid::Uuid::new_v4().to_string();
            let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(15)).to_rfc3339();

            let base_url =
                std::env::var("WADDLE_BASE_URL").unwrap_or_else(|_| format!("https://{}", domain));
            let base_url = base_url.trim_end_matches('/');
            let put_url = format!("{}/api/upload/{}", base_url, slot_id);
            let get_url = format!("{}/api/files/{}/{}", base_url, slot_id, safe_filename);

            let db = state.deps.app_state.db_pool.global();
            let conn = match db.guard().await {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "Failed to connect to database for upload slot");
                    return vec![build_upload_error(
                        &id,
                        &UploadError::InternalError(format!("Database error: {}", e)),
                    )];
                }
            };

            if let Err(e) = conn
                .execute(
                    "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at) VALUES (?, ?, ?, ?, ?, 'pending', ?)",
                    libsql::params![
                        slot_id.clone(),
                        sender_jid.to_bare().to_string(),
                        safe_filename.clone(),
                        request.size as i64,
                        content_type.clone(),
                        expires_at,
                    ],
                )
                .await
            {
                warn!(error = %e, "Failed to create upload slot in database");
                return vec![build_upload_error(
                    &id,
                    &UploadError::InternalError(format!("Database error: {}", e)),
                )];
            }

            debug!(
                slot_id = %slot_id,
                put_url = %put_url,
                get_url = %get_url,
                "Created upload slot via WebSocket"
            );

            let slot = UploadSlot {
                put_url,
                put_headers: vec![("Content-Type".to_string(), content_type)],
                get_url,
            };
            let response = build_upload_slot_response(request_iq, &slot);
            return vec![iq_to_xml(response)];
        }
    }

    // PubSub / PEP (XEP-0060, XEP-0163)
    if is_pubsub_iq(&iq) {
        if !phase.is_ready() {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "auth",
                "not-authorized",
            )];
        }

        let Some(user_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "auth",
                "not-authorized",
            )];
        };

        let target_jid = match &iq.to {
            Some(to_jid) => to_jid.to_bare(),
            None => user_jid.clone(),
        };

        let request = match parse_pubsub_iq(&iq) {
            Ok(req) => req,
            Err(e) => {
                warn!("Failed to parse PubSub request: {}", e);
                let error = build_pubsub_error(&iq, PubSubError::InvalidJid);
                return vec![iq_to_xml(error)];
            }
        };

        debug!(?request, "Handling PubSub request via WebSocket");

        match request {
            PubSubRequest::Publish { node, item } => {
                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .publish_item(&target_jid, &node, &item, Some(&user_jid), true)
                    .await;

                match result {
                    Ok(publish_result) => {
                        debug!(
                            node = %node,
                            item_id = %publish_result.item_id,
                            created = publish_result.node_created,
                            "PubSub item published via WebSocket"
                        );
                        let response =
                            build_pubsub_publish_result(&iq, &node, &publish_result.item_id);
                        return vec![iq_to_xml(response)];
                    }
                    Err(e) => {
                        warn!("PubSub publish failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::Items {
                node,
                max_items,
                item_ids,
            } => {
                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .get_items(&target_jid, &node, max_items, &item_ids)
                    .await;

                match result {
                    Ok(stored_items) => {
                        let items: Vec<_> =
                            stored_items.iter().map(|si| si.to_pubsub_item()).collect();
                        debug!(
                            node = %node,
                            count = items.len(),
                            "PubSub items retrieved via WebSocket"
                        );
                        let response = build_pubsub_items_result(&iq, &node, &items);
                        return vec![iq_to_xml(response)];
                    }
                    Err(e) => {
                        warn!("PubSub items retrieval failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::Retract {
                node,
                item_id,
                notify: _,
            } => {
                if target_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .retract_item(&target_jid, &node, &item_id)
                    .await;

                match result {
                    Ok(retracted) => {
                        if retracted {
                            debug!(node = %node, item_id = %item_id, "PubSub item retracted via WebSocket");
                            let response = build_pubsub_success(&iq);
                            return vec![iq_to_xml(response)];
                        } else {
                            let error = build_pubsub_error(&iq, PubSubError::ItemNotFound);
                            return vec![iq_to_xml(error)];
                        }
                    }
                    Err(e) => {
                        warn!("PubSub retract failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::CreateNode { node } => {
                if target_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .get_or_create_node(&target_jid, &node)
                    .await;

                match result {
                    Ok((_, created)) => {
                        if created {
                            debug!(node = %node, "PubSub node created via WebSocket");
                        } else {
                            debug!(node = %node, "PubSub node already exists");
                        }
                        let response = build_pubsub_success(&iq);
                        return vec![iq_to_xml(response)];
                    }
                    Err(e) => {
                        warn!("PubSub node creation failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::DeleteNode { node } => {
                if target_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .delete_node(&target_jid, &node)
                    .await;

                match result {
                    Ok(deleted) => {
                        if deleted {
                            debug!(node = %node, "PubSub node deleted via WebSocket");
                            let response = build_pubsub_success(&iq);
                            return vec![iq_to_xml(response)];
                        } else {
                            let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                            return vec![iq_to_xml(error)];
                        }
                    }
                    Err(e) => {
                        warn!("PubSub node deletion failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::Subscribe { .. } | PubSubRequest::Unsubscribe { .. } => {
                let response = build_pubsub_success(&iq);
                return vec![iq_to_xml(response)];
            }
        }
    }

    // Unknown IQ - log a compact summary and return an error.
    let payload_ns = (!payload_ns.is_empty()).then_some(payload_ns.as_str());
    warn!(id = %id, payload_ns, "Unhandled IQ stanza");
    vec![build_iq_error_xml_with_addresses(
        &id,
        response_from,
        response_to,
        "cancel",
        "feature-not-implemented",
    )]
}

/// Handle message stanzas including groupchat routing
async fn handle_message(
    mut incoming: xmpp_parsers::message::Message,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    let Some(sender_jid) = phase.bound_jid() else {
        warn!("Message received without authenticated session");
        return vec![];
    };

    // Always stamp the authenticated sender.
    incoming.from = Some(jid::Jid::from(sender_jid.clone()));

    // Handle groupchat messages
    if incoming.type_ == XmppMessageType::Groupchat {
        let Some(to_jid) = incoming.to.as_ref() else {
            warn!("Groupchat message without 'to' attribute");
            return vec![];
        };

        // Parse room JID (strip resource if present)
        let room_jid = to_jid.to_bare();

        if room_jid.domain().as_str() != muc_domain {
            warn!(to = %to_jid, "Groupchat message to non-MUC JID");
            return vec![];
        }

        debug!(room = %room_jid, sender = %sender_jid, "Groupchat message");

        let Some(room_actor) = get_room_actor(state, &room_jid).await else {
            warn!(room = %room_jid, "Message to non-existent room");
            return vec![];
        };

        // Build a prototype message, enrich once, then ask the room actor to fan it out.
        let mut prototype = incoming.clone();
        prototype.id = prototype
            .id
            .clone()
            .or_else(|| Some(uuid::Uuid::new_v4().to_string()));
        prototype.type_ = XmppMessageType::Groupchat;

        // Enrich: detect GitHub links and append embed XML elements (fail-open)
        let _embeds_added = state
            .deps
            .protocol
            .extension_manager
            .enrich_message(&mut prototype)
            .await;

        let broadcast = match room_actor
            .ask(BuildGroupchatBroadcast {
                sender_jid: sender_jid.clone(),
                message: prototype.clone(),
            })
            .await
        {
            Ok(broadcast) => broadcast,
            Err(error) => {
                warn!(
                    sender = %sender_jid,
                    room = %room_jid,
                    error = ?error,
                    "Sender not permitted to broadcast to MUC room"
                );
                return vec![];
            }
        };
        let sender_nick = broadcast.sender_nick;
        let mut local_messages = broadcast.federated_messages.local;
        let occupant_bare_jids = broadcast.occupant_bare_jids;

        let from_room_jid = format!("{}/{}", room_jid, sender_nick);
        if let Ok(from_jid) = from_room_jid.parse::<FullJid>() {
            prototype.from = Some(jid::Jid::from(from_jid));
        } else {
            prototype.from = Some(jid::Jid::from(sender_jid.clone()));
        }
        prototype.to = None;

        // Archive body-bearing room messages in XMPP MAM storage.
        let archive_id = archive_groupchat_message(state, &room_jid, &prototype).await;
        if let Some(ref archive_id) = archive_id {
            add_mam_stanza_id(&mut prototype, archive_id.as_str(), &room_jid.to_string());
        }

        if should_project_message(&prototype) {
            let timestamp = Utc::now().timestamp();
            let sender_bare = sender_jid.to_bare();
            let entry = groupchat_entry(room_jid.clone(), &prototype, timestamp);

            if let Err(error) = state
                .deps
                .protocol
                .inbox_storage
                .upsert(&sender_bare, entry.clone(), false)
                .await
            {
                warn!(jid = %sender_bare, room = %room_jid, error = %error, "Failed to update sender inbox for groupchat");
            }

            // Thread-level inbox projection: if the message carries a <thread/>,
            // upsert a thread-scoped entry alongside the channel-level one.
            let thread_entry = prototype.thread.as_ref().map(|thread| {
                // Resolve thread title: XEP-0508 thread-create title, or first message preview
                let forum_title = waddle_xmpp::xep::xep0508::extract_forum_action(&prototype)
                    .and_then(|action| match action {
                        waddle_xmpp::xep::xep0508::ForumAction::CreateThread(tc) => Some(tc.title),
                        _ => None,
                    });
                let title = forum_title.or_else(|| preview_text(&prototype));
                let author_nick = prototype
                    .from
                    .as_ref()
                    .and_then(|jid| jid.resource().map(|r| r.to_string()));
                groupchat_thread_entry(
                    room_jid.clone(),
                    &prototype,
                    timestamp,
                    &thread.0,
                    title.as_deref(),
                    author_nick.as_deref(),
                )
            });

            let mut projected_bares = std::collections::HashSet::new();
            for occupant_bare in occupant_bare_jids
                .iter()
                .filter_map(|jid| jid.parse::<BareJid>().ok())
                .filter(|jid| projected_bares.insert(jid.clone()))
            {
                match state
                    .deps
                    .protocol
                    .inbox_storage
                    .upsert(&occupant_bare, entry.clone(), true)
                    .await
                {
                    Ok(updated) => push_inbox_update(state, &occupant_bare, &updated).await,
                    Err(error) => {
                        warn!(jid = %occupant_bare, room = %room_jid, error = %error, "Failed to update occupant inbox for groupchat");
                    }
                }

                // Push thread-level entry too
                if let Some(ref thread_entry) = thread_entry {
                    match state
                        .deps
                        .protocol
                        .inbox_storage
                        .upsert(&occupant_bare, thread_entry.clone(), true)
                        .await
                    {
                        Ok(updated) => push_inbox_update(state, &occupant_bare, &updated).await,
                        Err(error) => {
                            warn!(jid = %occupant_bare, room = %room_jid, error = %error, "Failed to update occupant thread inbox");
                        }
                    }
                }
            }

            // Upsert thread entry for sender too (without incrementing unread)
            if let Some(ref thread_entry) = thread_entry {
                if let Err(error) = state
                    .deps
                    .protocol
                    .inbox_storage
                    .upsert(&sender_bare, thread_entry.clone(), false)
                    .await
                {
                    warn!(jid = %sender_bare, room = %room_jid, error = %error, "Failed to update sender thread inbox");
                }
            }
        }

        // Send to all occupants. Groupchat broadcasts are fire-and-forget:
        // message bodies are already archived to MAM, so any occupant the
        // server can't reach right now (backpressured or stale) will pick up
        // the message on their next MAM catch-up. Blocking here is what
        // caused join cascades under zombie load.
        //
        // Accounting invariant for the broadcast log below:
        //   `intended = delivered + dropped_full + dropped_closed + not_connected`
        // The sender is always one of `occupants` in a groupchat send but is
        // reached via the direct echo response (not `try_send_to`), so the
        // echo path counts as one `delivered` to keep the invariant true.
        let mut echo_response = None;
        let mut delivered = 0u32;
        let mut dropped_full = 0u32;
        let mut dropped_closed = 0u32;
        let mut not_connected = 0u32;
        let intended = local_messages.len();
        for mut outbound in local_messages.drain(..) {
            if let Some(ref archive_id) = archive_id {
                add_mam_stanza_id(&mut outbound.message, archive_id, &room_jid.to_string());
            }

            if outbound.to == *sender_jid {
                // Echo back to sender — serialize the enriched prototype
                echo_response = Some(stanza_to_xml(&Stanza::Message(outbound.message)));
                delivered += 1;
            } else {
                let stanza = Stanza::Message(outbound.message);
                match state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&outbound.to, stanza)
                {
                    BroadcastOutcome::Delivered => delivered += 1,
                    BroadcastOutcome::DroppedFull => dropped_full += 1,
                    BroadcastOutcome::DroppedClosed => dropped_closed += 1,
                    BroadcastOutcome::NotConnected => not_connected += 1,
                }
            }
        }

        debug_assert_eq!(
            intended as u32,
            delivered + dropped_full + dropped_closed + not_connected,
            "broadcast accounting must cover every occupant exactly once"
        );

        info!(
            room = %room_jid,
            sender = %sender_nick,
            intended,
            delivered,
            dropped_full,
            dropped_closed,
            not_connected,
            "Groupchat message broadcast"
        );

        // Return the echo to the sender
        return echo_response.into_iter().collect();
    }

    // Handle direct messages (chat)
    if incoming.type_ == XmppMessageType::Chat {
        if let Some(to_jid) = incoming.to.as_ref() {
            debug!(to = %to_jid, from = %sender_jid, "Direct chat message");

            // Build a prototype message and enrich it with embeds before routing.
            // Enrichment is fail-open: errors are logged but never block delivery.
            let mut prototype = incoming.clone();
            if prototype.id.is_none() {
                prototype.id = extract_origin_id(&prototype)
                    .or_else(|| Some(uuid::Uuid::new_v4().to_string()));
            }
            prototype.from = Some(jid::Jid::from(sender_jid.clone()));
            prototype.type_ = XmppMessageType::Chat;
            let should_carbon =
                prototype.type_ == XmppMessageType::Chat && should_copy_message(&prototype);

            // Enrich: detect GitHub links and append embed XML elements
            let _embeds_added = state
                .deps
                .protocol
                .extension_manager
                .enrich_message(&mut prototype)
                .await;
            let has_github_embed = message_has_embed_for_namespaces(
                &prototype,
                state.deps.protocol.extension_manager.feature_namespaces(),
            );

            // Archive body-bearing DMs to both sender's and recipient's personal MAM.
            archive_direct_message(state, sender_jid, to_jid, &prototype).await;

            if should_project_message(&prototype) {
                let timestamp = Utc::now().timestamp();
                let sender_bare = sender_jid.to_bare();
                let recipient_bare = to_jid.to_bare();

                if let Err(error) = state
                    .deps
                    .protocol
                    .inbox_storage
                    .upsert(
                        &sender_bare,
                        direct_message_entry(recipient_bare.clone(), &prototype, timestamp),
                        false,
                    )
                    .await
                {
                    warn!(jid = %sender_bare, partner = %recipient_bare, error = %error, "Failed to update sender inbox for direct message");
                }

                if recipient_bare.domain() == sender_bare.domain() {
                    match state
                        .deps
                        .protocol
                        .inbox_storage
                        .upsert(
                            &recipient_bare,
                            direct_message_entry(sender_bare.clone(), &prototype, timestamp),
                            true,
                        )
                        .await
                    {
                        Ok(updated) => push_inbox_update(state, &recipient_bare, &updated).await,
                        Err(error) => {
                            warn!(jid = %recipient_bare, partner = %sender_bare, error = %error, "Failed to update recipient inbox for direct message");
                        }
                    }
                }
            }

            // Route the enriched message
            let delivered_full_jid = if let Ok(to_full_jid) = to_jid.clone().try_into_full() {
                let mut msg = prototype.clone();
                msg.to = Some(jid::Jid::from(to_full_jid.clone()));
                let stanza = Stanza::Message(msg);
                match state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(&to_full_jid, stanza)
                    .await
                {
                    SendResult::Sent => Some(to_full_jid),
                    SendResult::NotConnected | SendResult::ChannelClosed => None,
                }
            } else {
                let to_bare_jid = to_jid.to_bare();
                let resources = state
                    .deps
                    .protocol
                    .connection_registry
                    .get_resources_for_user(&to_bare_jid);
                for resource_jid in resources {
                    let mut msg = prototype.clone();
                    msg.to = Some(jid::Jid::from(resource_jid.clone()));
                    let stanza = Stanza::Message(msg);
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .send_to(&resource_jid, stanza)
                        .await;
                }
                None
            };

            if should_carbon {
                if let Some(ref recipient_full_jid) = delivered_full_jid {
                    send_received_carbons_to_websocket_resources(
                        state,
                        recipient_full_jid,
                        &prototype,
                    )
                    .await;
                }
                send_sent_carbons_to_websocket_resources(state, sender_jid, &prototype).await;
            }

            if has_github_embed {
                let echo = prototype.clone();
                return vec![stanza_to_xml(&Stanza::Message(echo))];
            }
        } else {
            warn!("Direct chat message without 'to' attribute");
        }
        return vec![];
    }

    debug!(msg_type = ?incoming.type_, "Message stanza received");
    vec![]
}

async fn send_sent_carbons_to_websocket_resources(
    state: &WebSocketState,
    sender_jid: &FullJid,
    message: &xmpp_parsers::message::Message,
) {
    let sender_bare = sender_jid.to_bare();
    let resources = state
        .deps
        .protocol
        .connection_registry
        .get_other_carbon_resources_for_user(&sender_bare, sender_jid);

    for resource_jid in resources {
        let carbon =
            match build_sent_carbon(message, &sender_bare.to_string(), &resource_jid.to_string()) {
                Ok(carbon) => carbon,
                Err(error) => {
                    warn!(error = %error, to = %resource_jid, "Failed to build sent carbon");
                    continue;
                }
            };
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource_jid, Stanza::Message(carbon))
            .await;
    }
}

async fn send_received_carbons_to_websocket_resources(
    state: &WebSocketState,
    recipient_jid: &FullJid,
    message: &xmpp_parsers::message::Message,
) {
    let recipient_bare = recipient_jid.to_bare();
    let resources = state
        .deps
        .protocol
        .connection_registry
        .get_other_carbon_resources_for_user(&recipient_bare, recipient_jid);

    for resource_jid in resources {
        let carbon = match build_received_carbon(
            message,
            &recipient_bare.to_string(),
            &resource_jid.to_string(),
        ) {
            Ok(carbon) => carbon,
            Err(error) => {
                warn!(error = %error, to = %resource_jid, "Failed to build received carbon");
                continue;
            }
        };
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource_jid, Stanza::Message(carbon))
            .await;
    }
}

/// Push an inbox update headline to all connected sessions of a user.
async fn push_inbox_update(
    state: &WebSocketState,
    user: &BareJid,
    entry: &waddle_xmpp::inbox::InboxEntry,
) {
    let resources = state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(user);
    for resource_jid in resources {
        let msg = build_inbox_push(jid::Jid::from(resource_jid.clone()), entry);
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource_jid, Stanza::Message(msg))
            .await;
    }
}

/// Returns true if this groupchat message should be written to the MAM archive.
///
/// Mirrors the `should_archive_timeline_message` predicate in `connection.rs`:
/// body/subject-bearing messages are always archived; body-less protocol
/// events (reactions, retractions, moderation, file-shares, stickers) are
/// archived too so that MAM replay faithfully reproduces the room timeline.
/// Error messages and messages carrying a `<no-store/>` hint are excluded.
fn should_archive_groupchat_message(msg: &xmpp_parsers::message::Message) -> bool {
    if matches!(msg.type_, XmppMessageType::Error) || should_skip_storage(msg) {
        return false;
    }

    if !msg.bodies.is_empty() || !msg.subjects.is_empty() {
        return true;
    }

    is_reaction_message(msg)
        || is_retraction_message(msg)
        || is_moderation_request_message(msg)
        || is_moderation_result_message(msg)
        || has_file_sharing(msg)
        || is_sticker_message(msg)
}

async fn archive_groupchat_message(
    state: &WebSocketState,
    room_jid: &BareJid,
    message: &xmpp_parsers::message::Message,
) -> Option<String> {
    if !should_archive_groupchat_message(message) {
        return None;
    }

    let body = prototype_body(message)
        .map(|value| value.trim().to_string())
        .unwrap_or_default();

    let (reply_to_id, reply_to_jid) = extract_reply_reference(message);
    let origin_id = extract_origin_id(message);

    let archived = ArchivedMessage {
        id: String::new(),
        timestamp: Utc::now(),
        from: message
            .from
            .as_ref()
            .map(|jid| jid.to_string())
            .unwrap_or_default(),
        to: room_jid.to_string(),
        body,
        stanza_id: message.id.clone(),
        thread_id: message.thread.as_ref().map(|thread| thread.0.clone()),
        reply_to_id,
        reply_to_jid,
        origin_id,
        message_type: mam_message_type(&message.type_),
        stanza_xml: Some(archived_stanza_xml(message)),
    };

    let archive_jid = room_jid.to_string();
    match state
        .deps
        .protocol
        .mam_storage
        .store_message(archive_jid.as_str(), &archived)
        .await
    {
        Ok(archive_id) => Some(archive_id),
        Err(err) => {
            warn!(
                room = %room_jid,
                error = %err,
                "Failed to archive groupchat message to MAM"
            );
            None
        }
    }
}

/// Archive a direct (type="chat") message to both the sender's and recipient's
/// personal MAM archives.  Only messages with a `<body>` are stored.
async fn archive_direct_message(
    state: &WebSocketState,
    sender_jid: &FullJid,
    to_jid: &jid::Jid,
    message: &xmpp_parsers::message::Message,
) {
    let Some(body) = prototype_body(message)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    let (reply_to_id, reply_to_jid) = extract_reply_reference(message);
    let origin_id = extract_origin_id(message);

    let archived = ArchivedMessage {
        id: String::new(),
        timestamp: Utc::now(),
        from: sender_jid.to_bare().to_string(),
        to: to_jid.to_bare().to_string(),
        body,
        stanza_id: message.id.clone(),
        thread_id: message.thread.as_ref().map(|thread| thread.0.clone()),
        reply_to_id,
        reply_to_jid,
        origin_id,
        message_type: mam_message_type(&message.type_),
        stanza_xml: Some(archived_stanza_xml(message)),
    };

    // Store in sender's personal archive
    let sender_bare = sender_jid.to_bare().to_string();
    if let Err(err) = state
        .deps
        .protocol
        .mam_storage
        .store_message(sender_bare.as_str(), &archived)
        .await
    {
        warn!(
            from = %sender_jid,
            to = %to_jid,
            error = %err,
            "Failed to archive DM to sender's personal MAM"
        );
    }

    // Store in recipient's personal archive
    let recipient_bare = to_jid.to_bare().to_string();
    if let Err(err) = state
        .deps
        .protocol
        .mam_storage
        .store_message(recipient_bare.as_str(), &archived)
        .await
    {
        warn!(
            from = %sender_jid,
            to = %to_jid,
            error = %err,
            "Failed to archive DM to recipient's personal MAM"
        );
    }
}

fn mam_message_type(message_type: &XmppMessageType) -> String {
    match message_type {
        XmppMessageType::Chat => "chat".to_string(),
        XmppMessageType::Error => "error".to_string(),
        XmppMessageType::Groupchat => "groupchat".to_string(),
        XmppMessageType::Headline => "headline".to_string(),
        XmppMessageType::Normal => "normal".to_string(),
    }
}

fn extract_reply_reference(
    message: &xmpp_parsers::message::Message,
) -> (Option<String>, Option<String>) {
    let Some(reply) = message
        .payloads
        .iter()
        .find(|payload| payload.name() == "reply" && payload.ns() == NS_REPLY)
    else {
        return (None, None);
    };

    (
        reply.attr("id").map(ToOwned::to_owned),
        reply.attr("to").map(ToOwned::to_owned),
    )
}

fn extract_origin_id(message: &xmpp_parsers::message::Message) -> Option<String> {
    message
        .payloads
        .iter()
        .find(|payload| payload.name() == "origin-id" && payload.ns() == STANZA_ID_NS)
        .and_then(|origin| origin.attr("id").map(ToOwned::to_owned))
}

fn prototype_body(message: &xmpp_parsers::message::Message) -> Option<String> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .map(|body| body.0.clone())
}

fn build_muc_join_presence_xml(
    room_jid: &BareJid,
    nick: &str,
    to_jid: &FullJid,
    affiliation: &str,
    role: &str,
    real_jid: &FullJid,
    include_self_status: bool,
) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| to_jid.clone());

    let mut user_payload = Element::builder("x", "http://jabber.org/protocol/muc#user").append(
        Element::builder("item", "http://jabber.org/protocol/muc#user")
            .attr("affiliation", affiliation)
            .attr("role", role)
            .attr("jid", real_jid.to_string())
            .build(),
    );

    if include_self_status {
        user_payload = user_payload.append(
            Element::builder("status", "http://jabber.org/protocol/muc#user")
                .attr("code", "110")
                .build(),
        );
    }

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", to_jid.to_string())
            .append(user_payload.build())
            .build(),
    )
}

/// XEP-0045 §7.2.9 conflict presence: the requested nick is already in use
/// by a different user. The joiner receives a `<presence type='error'/>` and
/// no room state changes.
fn build_muc_conflict_presence_xml(room_jid: &BareJid, nick: &str, to_jid: &FullJid) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| to_jid.clone());

    let error_payload = Element::builder("error", waddle_xmpp::ns::JABBER_CLIENT)
        .attr("type", "cancel")
        .append(Element::builder("conflict", "urn:ietf:params:xml:ns:xmpp-stanzas").build())
        .build();

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", to_jid.to_string())
            .attr("type", "error")
            .append(error_payload)
            .build(),
    )
}

fn build_muc_subject_message_xml(room_jid: &BareJid, to_jid: &FullJid, room_name: &str) -> String {
    element_to_xml(
        Element::builder("message", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", room_jid.to_string())
            .attr("to", to_jid.to_string())
            .attr("type", "groupchat")
            .append(
                Element::builder("subject", waddle_xmpp::ns::JABBER_CLIENT)
                    .append(format!("Welcome to {}!", room_name))
                    .build(),
            )
            .build(),
    )
}

fn build_muc_self_unavailable_xml(room_jid: &BareJid, nick: &str, sender_jid: &FullJid) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| sender_jid.clone());

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", sender_jid.to_string())
            .attr("type", "unavailable")
            .append(
                Element::builder("x", "http://jabber.org/protocol/muc#user")
                    .append(
                        Element::builder("item", "http://jabber.org/protocol/muc#user")
                            .attr("affiliation", "member")
                            .attr("role", "none")
                            .build(),
                    )
                    .append(
                        Element::builder("status", "http://jabber.org/protocol/muc#user")
                            .attr("code", "110")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
}

/// Create a presence stanza for MUC
fn create_presence_stanza(
    room_jid: &BareJid,
    nick: &str,
    real_jid: &FullJid,
    to_jid: &FullJid,
    _is_self: bool,
) -> xmpp_parsers::presence::Presence {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| real_jid.clone());

    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.from = Some(jid::Jid::from(from_jid));
    presence.to = Some(jid::Jid::from(to_jid.clone()));

    // In a full implementation, we'd add the MUC user extension here
    // For now, the XML generation handles it

    presence
}

/// Convert Affiliation to string
fn affiliation_str(affiliation: Affiliation) -> &'static str {
    match affiliation {
        Affiliation::Owner => "owner",
        Affiliation::Admin => "admin",
        Affiliation::Member => "member",
        Affiliation::Outcast => "outcast",
        Affiliation::None => "none",
    }
}

/// Convert Role to string
fn role_str(role: Role) -> &'static str {
    match role {
        Role::Moderator => "moderator",
        Role::Participant => "participant",
        Role::Visitor => "visitor",
        Role::None => "none",
    }
}

/// Derive waddle_id and channel_id from a room's bare JID node.

///
/// Convention: node is "waddleId_channelId" (first underscore separates).
/// Falls back to ("default", "default") if the node can't be parsed.
fn parse_room_jid_context(room_jid: &jid::BareJid) -> (String, String) {
    if let Some((waddle_id, channel_id)) = waddle_xmpp::parse_managed_room_jid(room_jid) {
        return (waddle_id, channel_id);
    }
    ("default".to_string(), "default".to_string())
}

async fn get_managed_channel_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Option<crate::server::routes::channels::ChannelResponse> {
    let (waddle_id, channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid)?;
    let waddle_db = state
        .deps
        .app_state
        .db_pool
        .get_waddle_db(&waddle_id)
        .await
        .ok()?;
    get_channel_from_db(&waddle_db, &waddle_id, &channel_id)
        .await
        .ok()
        .flatten()
}

fn build_xmpp_error_response(request_iq: &xmpp_parsers::iq::Iq, err: XmppError) -> String {
    match err {
        XmppError::Stanza {
            condition,
            error_type,
            text,
        } => waddle_xmpp::generate_iq_error(
            &request_iq.id,
            request_iq
                .from
                .as_ref()
                .map(|jid| jid.to_string())
                .as_deref(),
            request_iq.to.as_ref().map(|jid| jid.to_string()).as_deref(),
            condition,
            error_type,
            text.as_deref(),
        ),
        other => waddle_xmpp::generate_iq_error(
            &request_iq.id,
            request_iq
                .from
                .as_ref()
                .map(|jid| jid.to_string())
                .as_deref(),
            request_iq.to.as_ref().map(|jid| jid.to_string()).as_deref(),
            StanzaErrorCondition::InternalServerError,
            StanzaErrorType::Wait,
            Some(&other.to_string()),
        ),
    }
}

async fn handle_command_iq(
    request_iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    bound_jid: Option<&FullJid>,
) -> Vec<String> {
    let sender_jid: Jid = match bound_jid.cloned().map(Jid::from) {
        Some(jid) => jid,
        None => {
            return vec![build_xmpp_error_response(
                request_iq,
                XmppError::not_authorized(Some("Authenticated session required".to_string())),
            )];
        }
    };

    let command = match parse_command_from_iq(request_iq) {
        Ok(command) => command,
        Err(err) => {
            return vec![build_xmpp_error_response(
                request_iq,
                XmppError::bad_request(Some(format!("Invalid command request: {err}"))),
            )];
        }
    };

    let node = command.node.clone();
    let session_id = command.session_id.clone();
    let ctx = CommandContext {
        from: sender_jid,
        authenticated_user_id: authenticated_session
            .as_ref()
            .map(|session| session.user_id.clone()),
        iq: request_iq.clone(),
        command,
    };

    let result = state.deps.protocol.command_registry.dispatch(ctx).await;
    let response_command = match result {
        CommandResult::Executing {
            form,
            session_id,
            notes,
        } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Executing);
            command.session_id = Some(session_id);
            command.form = Some(form);
            command.notes = notes;
            command
        }
        CommandResult::Completed { form, notes } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Completed);
            command.session_id = session_id;
            command.form = form;
            command.notes = notes;
            command
        }
        CommandResult::Canceled { notes } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Canceled);
            command.session_id = session_id;
            command.notes = notes;
            command
        }
        CommandResult::Error(err) => return vec![build_xmpp_error_response(request_iq, err)],
    };

    vec![iq_to_xml(build_command_result(
        request_iq,
        &response_command,
    ))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::server::AppState;
    use futures::Sink;
    use hmac::{Hmac, Mac};
    use pbkdf2::pbkdf2_hmac;
    use sha2::{Digest, Sha256};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use tokio::sync::mpsc;

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
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config)
            .await
            .expect("db pool");

        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.expect("migrations");

        let server_config = ServerConfig::test_homeserver();
        let app_state = Arc::new(AppState::new(Arc::new(db_pool)));
        let auth_state = Arc::new(AuthState::new(
            app_state.clone(),
            &server_config,
            Some(b"test-encryption-key-32-bytes!!!"),
        ));
        let mam_db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .expect("mam db");
        let mam_conn = mam_db.connect().expect("mam conn");
        let mam_storage = Arc::new(LibSqlMamStorage::new(mam_conn));
        mam_storage.initialize().await.expect("mam init");

        let mut dispatcher = StanzaDispatcher::new();
        waddle_xmpp::protocol::handlers::register_default_handlers(&mut dispatcher);

        Arc::new(WebSocketState {
            deps: WebSocketDeps {
                app_state,
                auth_state,
                protocol: ProtocolServices {
                    connection_registry: Arc::new(ConnectionRegistry::new()),
                    room_registry: kameo::spawn(RoomRegistryActor::new(
                        "muc.example.com".to_string(),
                    )),
                    mam_storage,
                    inbox_storage: Arc::new(
                        waddle_xmpp::inbox::storage::InMemoryInboxStorage::new(),
                    ),
                    command_registry: Arc::new(CommandRegistry::new()),
                    extension_manager: Arc::new(
                        ExtensionManager::from_env().expect("extension manager"),
                    ),
                    dispatcher: Arc::new(dispatcher),
                    pubsub_storage: Arc::new(waddle_xmpp::pubsub::InMemoryPubSubStorage::new()),
                    sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
                    resumable_sessions: Arc::new(dashmap::DashMap::new()),
                },
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

    async fn register_test_native_user(state: &WebSocketState, username: &str, password: &str) {
        let native_user_store =
            NativeUserStore::new(Arc::new(state.deps.app_state.db_pool.global().clone()));
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

    fn parse_iq_for_test(xml: &str) -> xmpp_parsers::iq::Iq {
        match parse_frame(xml).expect("iq parses") {
            InboundFrame::Stanza(stanza) => match *stanza {
                Stanza::Iq(iq) => iq,
                _ => panic!("expected iq stanza"),
            },
            _ => panic!("expected iq stanza"),
        }
    }

    fn ready_phase(jid: &FullJid) -> ConnectionPhase {
        ConnectionPhase::ready(jid.clone(), false)
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
        let mut conn = LegacyConnState::new();

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
        let session = create_test_session(state.as_ref(), "alice").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let mut conn = LegacyConnState::new();

        let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

        assert_eq!(responses, vec![sasl_success_xml()]);
        assert!(conn.phase.is_authenticated());
        assert!(conn.authenticated_session.is_some());
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_auth_returns_malformed_request() {
        let state = create_test_websocket_state().await;
        let mut conn = LegacyConnState::new();

        let responses = handle_xmpp_frame(
            r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl">payload</auth>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

        assert_eq!(responses, vec![sasl_failure_xml("malformed-request")]);
    }

    #[tokio::test]
    async fn handle_xmpp_frame_malformed_sasl_response_returns_malformed_request() {
        let state = create_test_websocket_state().await;
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();
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
        let mut conn = LegacyConnState::new();

        let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

        assert_eq!(responses, vec![sasl_failure_xml("invalid-mechanism")]);
        assert!(!conn.phase.is_authenticated());
        assert!(!conn.phase.is_ready());
        assert!(conn.authenticated_session.is_none());
    }

    #[tokio::test]
    async fn websocket_oauthbearer_authenticates_session_token() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
        let frame = element_to_xml(
            Element::builder("auth", waddle_xmpp::ns::SASL)
                .attr("mechanism", "OAUTHBEARER")
                .append(payload)
                .build(),
        );
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let room_jid: BareJid = "waddle_channel@muc.example.com".parse().expect("room jid");
        let current_jid: FullJid = "alice@example.com/current".parse().expect("current jid");
        let stale_jid: FullJid = "alice@example.com/stale".parse().expect("stale jid");

        handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &current_jid,
            "alice",
            &None,
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
        let room_jid: BareJid = "waddle_channel@muc.example.com".parse().expect("room jid");
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

        let responses = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &sender_jid,
            "alice",
            &None,
        )
        .await;

        assert_eq!(responses.len(), 2);

        let self_presence = Element::from_str(&responses[0]).expect("self presence xml");
        assert_eq!(self_presence.name(), "presence");
        assert_eq!(self_presence.ns(), waddle_xmpp::ns::JABBER_CLIENT);
        let user_x = self_presence
            .get_child("x", "http://jabber.org/protocol/muc#user")
            .expect("muc user payload");
        assert!(user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("110")));

        let subject_message = Element::from_str(&responses[1]).expect("subject xml");
        assert_eq!(subject_message.name(), "message");
        assert_eq!(subject_message.ns(), waddle_xmpp::ns::JABBER_CLIENT);
        assert_eq!(subject_message.attr("type"), Some("groupchat"));
    }

    #[test]
    fn test_parse_room_jid_valid() {
        let jid: jid::BareJid = "waddle123_channel456@muc.example.com".parse().unwrap();
        let (waddle, channel) = parse_room_jid_context(&jid);
        assert_eq!(waddle, "waddle123");
        assert_eq!(channel, "channel456");
    }

    #[test]
    fn test_parse_room_jid_fallback() {
        // No underscore
        let jid: jid::BareJid = "singlename@muc.example.com".parse().unwrap();
        let (waddle, channel) = parse_room_jid_context(&jid);
        assert_eq!(waddle, "default");
        assert_eq!(channel, "default");

        // Leading underscore (empty waddle)
        let jid: jid::BareJid = "_channel@muc.example.com".parse().unwrap();
        let (waddle, channel) = parse_room_jid_context(&jid);
        assert_eq!(waddle, "default");
        assert_eq!(channel, "default");

        // Trailing underscore (empty channel)
        let jid: jid::BareJid = "waddle_@muc.example.com".parse().unwrap();
        let (waddle, channel) = parse_room_jid_context(&jid);
        assert_eq!(waddle, "default");
        assert_eq!(channel, "default");
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
        let mut conn = LegacyConnState::new();
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
        let mut conn = LegacyConnState::new();
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
        let mut conn = LegacyConnState::new();
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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();
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
    async fn handle_iq_command_request_routes_to_registry() {
        let state = create_test_websocket_state().await;
        state
            .deps
            .protocol
            .command_registry
            .register(
                "waddle:create-channel",
                "Create Channel",
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
        let frame = r#"<iq xmlns="jabber:client" id="cmd-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="waddle:create-channel" action="execute"/></iq>"#;
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
                "waddle:create-channel",
                "Create Channel",
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
        let frame = r#"<iq xmlns="jabber:client" id="cmd-prebind-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="waddle:create-channel" action="execute"/></iq>"#;
        let responses = handle_iq_with_conn_state(
            parse_iq_for_test(frame),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session),
            &ConnectionPhase::authenticated(&pending_jid),
            &mut carbons_enabled,
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

    async fn seed_waddle(
        state: &WebSocketState,
        owner_id: &str,
        waddle_id: &str,
        waddle_name: &str,
        is_public: bool,
    ) {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = state
            .deps
            .app_state
            .db_pool
            .global()
            .guard()
            .await
            .expect("persistent connection");
        conn.execute(
            "INSERT INTO waddles (id, name, description, owner_id, icon_url, is_public, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            libsql::params![
                waddle_id,
                waddle_name,
                Option::<String>::None,
                owner_id,
                Option::<String>::None,
                if is_public { 1i64 } else { 0i64 },
                now.clone(),
                now
            ],
        )
        .await
        .expect("insert waddle");
    }

    async fn seed_user_waddle_membership(
        state: &WebSocketState,
        user_id: &str,
        waddle_id: &str,
        waddle_name: &str,
    ) {
        seed_waddle(state, user_id, waddle_id, waddle_name, true).await;
        let conn = state
            .deps
            .app_state
            .db_pool
            .global()
            .guard()
            .await
            .expect("persistent connection");
        conn.execute(
            "INSERT INTO waddle_members (waddle_id, user_id, role, joined_at) VALUES (?, ?, 'member', ?)",
            libsql::params![waddle_id, user_id, chrono::Utc::now().to_rfc3339()],
        )
        .await
        .expect("insert waddle membership");
    }

    #[tokio::test]
    async fn handle_iq_disco_info_advertises_replies() {
        let server_domain = "example.com";
        let muc_domain = "muc.example.com";
        let state = create_test_websocket_state().await;

        let server_query = r#"<iq xmlns="jabber:client" id="srv1" type="get" to="example.com"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#;
        let server_responses = handle_iq(
            server_query,
            server_domain,
            muc_domain,
            state.as_ref(),
            &None,
            &ConnectionPhase::Unauthenticated,
        )
        .await;
        let server_response = server_responses.first().expect("server disco response");
        assert!(server_response.contains("urn:xmpp:reply:0"));
        assert!(server_response.contains("urn:waddle:github:0"));

        let muc_query = r#"<iq xmlns="jabber:client" id="muc1" type="get" to="muc.example.com"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#;
        let muc_responses = handle_iq(
            muc_query,
            server_domain,
            muc_domain,
            state.as_ref(),
            &None,
            &ConnectionPhase::Unauthenticated,
        )
        .await;
        let muc_response = muc_responses.first().expect("muc disco response");
        assert!(muc_response.contains("urn:xmpp:reply:0"));
        assert!(muc_response.contains("urn:waddle:github:0"));

        let room_query = r#"<iq xmlns="jabber:client" id="room1" type="get" to="room@muc.example.com"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#;
        let room_responses = handle_iq(
            room_query,
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
        assert!(room_response.contains("urn:waddle:github:0"));
    }

    #[tokio::test]
    async fn handle_iq_disco_items_server_advertises_spaces_service() {
        let state = create_test_websocket_state().await;
        let query = r#"<iq xmlns="jabber:client" id="srv-items" type="get" to="example.com"><query xmlns="http://jabber.org/protocol/disco#items"/></iq>"#;

        let responses = handle_iq(
            query,
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
    async fn handle_iq_disco_items_spaces_lists_user_waddles() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        seed_user_waddle_membership(
            state.as_ref(),
            &session.user_id,
            "waddle-alpha",
            "Alpha Space",
        )
        .await;

        let authenticated_session = Some(session);
        let authenticated_phase = authenticated_phase_for_session(
            authenticated_session.as_ref().expect("session"),
            "example.com",
        );
        let query = r#"<iq xmlns="jabber:client" id="spaces-items" type="get" to="spaces.example.com"><query xmlns="http://jabber.org/protocol/disco#items"/></iq>"#;

        let responses = handle_iq(
            query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &authenticated_session,
            &authenticated_phase,
        )
        .await;
        let response = responses.first().expect("spaces disco items response");

        assert!(
            response.contains("waddle-alpha"),
            "expected space node in spaces disco#items: {response}"
        );
        assert!(
            response.contains("Alpha Space"),
            "expected space name in spaces disco#items: {response}"
        );
    }

    #[tokio::test]
    async fn handle_iq_disco_items_spaces_node_lists_channels() {
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let waddle_id = "waddle-bravo";
        seed_user_waddle_membership(state.as_ref(), &session.user_id, waddle_id, "Bravo Space")
            .await;

        let waddle_db = state
            .deps
            .app_state
            .db_pool
            .create_waddle_db(waddle_id)
            .await
            .expect("create waddle db");
        MigrationRunner::waddle()
            .run(&waddle_db)
            .await
            .expect("waddle migrations");
        let conn = waddle_db.guard().await.expect("persistent connection");
        conn.execute(
            "INSERT INTO channels (id, name, channel_type, position, is_default) VALUES (?, ?, 'text', 0, 0)",
            libsql::params!["general", "General"],
        )
        .await
        .expect("insert channel");
        drop(conn);

        let authenticated_session = Some(session);
        let authenticated_phase = authenticated_phase_for_session(
            authenticated_session.as_ref().expect("session"),
            "example.com",
        );
        let query = r#"<iq xmlns="jabber:client" id="space-node-items" type="get" to="spaces.example.com"><query xmlns="http://jabber.org/protocol/disco#items" node="waddle-bravo"/></iq>"#;

        let responses = handle_iq(
            query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &authenticated_session,
            &authenticated_phase,
        )
        .await;
        let response = responses.first().expect("spaces node disco items response");

        assert!(
            response.contains("waddle-bravo_general@muc.example.com"),
            "expected channel room JID in spaces node disco#items: {response}"
        );
        assert!(
            response.contains("General"),
            "expected channel name in spaces node disco#items: {response}"
        );
    }

    #[tokio::test]
    async fn handle_iq_disco_info_spaces_node_reports_open_for_public_space() {
        let state = create_test_websocket_state().await;
        let owner = create_test_session(state.as_ref(), "owner").await;
        let viewer = create_test_session(state.as_ref(), "viewer").await;
        seed_waddle(
            state.as_ref(),
            &owner.user_id,
            "waddle-public",
            "Public Space",
            true,
        )
        .await;

        let viewer_phase = authenticated_phase_for_session(&viewer, "example.com");
        let query = r#"<iq xmlns="jabber:client" id="space-node-info" type="get" to="spaces.example.com"><query xmlns="http://jabber.org/protocol/disco#info" node="waddle-public"/></iq>"#;
        let responses = handle_iq(
            query,
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
    async fn handle_iq_disco_info_spaces_node_hides_private_space_from_non_member() {
        let state = create_test_websocket_state().await;
        let owner = create_test_session(state.as_ref(), "owner").await;
        let viewer = create_test_session(state.as_ref(), "viewer").await;
        seed_waddle(
            state.as_ref(),
            &owner.user_id,
            "waddle-private",
            "Private Space",
            false,
        )
        .await;

        let viewer_phase = authenticated_phase_for_session(&viewer, "example.com");
        let query = r#"<iq xmlns="jabber:client" id="space-node-info-private" type="get" to="spaces.example.com"><query xmlns="http://jabber.org/protocol/disco#info" node="waddle-private"/></iq>"#;
        let responses = handle_iq(
            query,
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
            "private space should not be discoverable by non-members: {response}"
        );
    }

    #[tokio::test]
    async fn handle_message_direct_with_github_embed_returns_sender_echo() {
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
        let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
        let state = create_test_websocket_state().await;

        let (recipient_tx, mut recipient_rx) = mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register(recipient_jid.clone(), recipient_tx);

        let frame = format!(
            "<message xmlns='jabber:client' to='{}' type='chat' id='dm-github-1'>\
                <body>Repo payload already attached</body>\
                <repo xmlns='urn:waddle:github:0' owner='rust-lang' name='rust' \
                      url='https://github.com/rust-lang/rust'/>\
             </message>",
            recipient_jid
        );

        let responses = handle_message(
            parse_message_for_test(&frame),
            "muc.example.com",
            state.as_ref(),
            &ConnectionPhase::ready(sender_jid.clone(), false),
            &None,
        )
        .await;

        assert_eq!(responses.len(), 1, "sender should get an echo response");
        let sender_echo = &responses[0];
        assert!(
            sender_echo.contains("to=\"bob@example.com/mobile\"")
                || sender_echo.contains("to='bob@example.com/mobile'"),
            "sender echo should preserve original recipient JID: {sender_echo}"
        );
        assert!(
            sender_echo.contains("urn:waddle:github:0"),
            "sender echo should include GitHub payload: {sender_echo}"
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
                    routed_xml.contains("urn:waddle:github:0"),
                    "routed stanza should preserve GitHub payload: {routed_xml}"
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

        let responses = handle_message(
            parse_message_for_test(
                "<message xmlns='jabber:client' to='alice@example.com' type='chat' id='dm-bare-1'>\
                <body>hello all resources</body>\
             </message>",
            ),
            "muc.example.com",
            state.as_ref(),
            &ConnectionPhase::ready(sender_jid.clone(), false),
            &None,
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

        let web_xml = web_chat.expect("web resource should receive original bare-JID message");
        let mobile_xml =
            mobile_chat.expect("mobile resource should receive original bare-JID message");
        assert!(
            web_xml.contains("to=\"alice@example.com/web-123\"")
                || web_xml.contains("to='alice@example.com/web-123'"),
            "web delivery should target the web resource: {web_xml}"
        );
        assert!(
            mobile_xml.contains("to=\"alice@example.com/mobile-456\"")
                || mobile_xml.contains("to='alice@example.com/mobile-456'"),
            "mobile delivery should target the mobile resource: {mobile_xml}"
        );
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

        let responses = handle_message(
            parse_message_for_test(
                "<message xmlns='jabber:client' to='ghost@example.com' type='chat' id='sent-carbon-1'>\
                <body>sent carbon over websocket</body>\
             </message>",
            ),
            "muc.example.com",
            state.as_ref(),
            &ConnectionPhase::ready(sender_jid.clone(), false),
            &None,
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

        let responses = handle_message(
            parse_message_for_test(frame.as_str()),
            "muc.example.com",
            state.as_ref(),
            &ConnectionPhase::ready(sender_jid.clone(), false),
            &None,
        )
        .await;

        assert!(responses.is_empty(), "plain DM should not echo to sender");

        let mut delivered_original = None;
        while let Ok(outbound) = recipient_rx.try_recv() {
            let xml = stanza_to_xml(&outbound.stanza);
            if xml.contains("received carbon over websocket") && !xml.contains("urn:xmpp:carbons:2")
            {
                delivered_original = Some(xml);
                break;
            }
        }
        assert!(
            delivered_original.is_some(),
            "targeted recipient should receive original message"
        );

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
        let responses = handle_message(
            parse_message_for_test(message_xml.as_str()),
            "muc.example.com",
            state.as_ref(),
            &ConnectionPhase::ready(alice_jid.clone(), false),
            &Some(alice_session),
        )
        .await;
        assert!(responses.is_empty(), "plain DM should not echo to sender");

        let inbox_query = format!(
            "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-1'>\
                <query xmlns='urn:xmpp:inbox:0'/>\
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
                <mark-read xmlns='urn:xmpp:inbox:0' partner='alice@example.com'/>\
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
                <query xmlns='urn:xmpp:inbox:0' only-unread='true'/>\
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
        let frame = r#"<iq xmlns='jabber:client' type='get' to='bob@example.com' id='inbox-prebind-1'><query xmlns='urn:xmpp:inbox:0'/></iq>"#;
        let responses = handle_iq_with_conn_state(
            parse_iq_for_test(frame),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session),
            &ConnectionPhase::authenticated(&pending_jid),
            &mut carbons_enabled,
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
        let responses = handle_message(
            parse_message_for_test(message_xml.as_str()),
            "muc.example.com",
            state.as_ref(),
            &ConnectionPhase::ready(alice_jid.clone(), false),
            &Some(alice_session),
        )
        .await;
        assert!(
            responses.is_empty(),
            "encrypted file-sharing DM should not echo to sender"
        );

        let inbox_query = format!(
            "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-esfs-1'>\
                <query xmlns='urn:xmpp:inbox:0'/>\
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
        let room_jid: BareJid = format!("{waddle_id}_{channel_id}@muc.example.com")
            .parse()
            .expect("room jid");
        let sender_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
            .parse()
            .expect("sender jid");
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
        let message_responses = handle_message(
            parse_message_for_test(message_xml.as_str()),
            "muc.example.com",
            state.as_ref(),
            &ConnectionPhase::ready(sender_jid.clone(), false),
            &Some(session.clone()),
        )
        .await;
        assert_eq!(
            message_responses.len(),
            1,
            "sender should receive reflected echo"
        );

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
        let message_responses = handle_message(
            parse_message_for_test(message_xml.as_str()),
            "muc.example.com",
            state.as_ref(),
            &ConnectionPhase::ready(alice_jid.clone(), false),
            &Some(alice_session),
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

        // Add a payload element (simulating a GitHub embed)
        let embed = xmpp_parsers::minidom::Element::builder("repo", "urn:waddle:github:0")
            .attr("owner", "cuenv")
            .attr("name", "cuenv")
            .build();
        msg.payloads.push(embed);

        let xml = stanza_to_xml(&Stanza::Message(msg));

        assert!(xml.contains("<body>Hello</body>"), "body must be present");
        assert!(
            xml.contains("urn:waddle:github:0"),
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
        let room_jid: BareJid = "rejoin_channel@muc.example.com".parse().expect("room");
        let first: FullJid = "alice@example.com/tab-1".parse().expect("first");
        let second: FullJid = "alice@example.com/tab-2".parse().expect("second");

        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &first,
            "alice",
            &None,
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
    async fn muc_nick_collision_returns_conflict_presence() {
        // Two different users try to hold the same nick — second gets a
        // <presence type='error'/> with <conflict/>, and room state for
        // the incumbent is untouched.
        let state = create_test_websocket_state().await;
        let room_jid: BareJid = "conflict_channel@muc.example.com".parse().expect("room");
        let alice: FullJid = "alice@example.com/desktop".parse().expect("alice");
        let bob: FullJid = "bob@example.com/phone".parse().expect("bob");

        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "dino",
            &None,
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
    async fn sm_enable_requires_resource_binding() {
        let state = create_test_websocket_state().await;
        let mut conn = LegacyConnState::new();
        // Without resource_bound, enable must fail.
        let frame = "<enable xmlns='urn:xmpp:sm:3' resume='true'/>";
        let responses = handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(responses.len(), 1);
        let el = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(el.name(), "failed");
        assert!(!conn.sm_state.enabled);
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
        let mut conn = LegacyConnState::new();

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
        let mut conn = LegacyConnState::new();
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
        let mut conn = LegacyConnState::new();
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
        let mut conn = LegacyConnState::new();
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
        let mut conn = LegacyConnState::new();
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
        let mut conn = LegacyConnState::new();
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
        };
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(detached)
            .await
            .expect("store");

        let mut conn = LegacyConnState::new();
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
    async fn sm_resume_with_unknown_stream_id_fails() {
        let state = create_test_websocket_state().await;
        let mut conn = LegacyConnState::new();
        let frame = "<resume xmlns='urn:xmpp:sm:3' previd='does-not-exist' h='0'/>";
        let responses = handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(responses.len(), 1);
        let el = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(el.name(), "failed");
        // Must NOT mark the session as authenticated/bound.
        assert!(!conn.phase.is_authenticated());
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
            jid,
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
        };
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(detached)
            .await
            .expect("store");

        let mut conn = LegacyConnState::new();
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
        let room_jid: BareJid = "detached_channel@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");

        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &jid,
            "alice",
            &None,
        )
        .await;
        let (tx, _rx) = mpsc::channel::<OutboundStanza>(4);
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx);

        let mut conn = LegacyConnState::new();
        conn.phase = ConnectionPhase::ready(jid.clone(), false);
        conn.sm_state
            .enable("stream-detach".to_string(), true, Some(300));

        cleanup_connection_shutdown(state.as_ref(), &conn, false).await;

        assert!(!state.deps.protocol.connection_registry.is_connected(&jid));
        let detached = state
            .deps
            .protocol
            .sm_session_registry
            .take_session("stream-detach")
            .await
            .expect("registry lookup");
        assert!(
            detached.is_some(),
            "resumable transport drop must detach SM state"
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
        let room_jid: BareJid = "closing_channel@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");

        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &jid,
            "alice",
            &None,
        )
        .await;
        let (tx, _rx) = mpsc::channel::<OutboundStanza>(4);
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx);

        let mut conn = LegacyConnState::new();
        conn.phase = ConnectionPhase::ready(jid.clone(), false);
        conn.sm_state
            .enable("stream-close".to_string(), false, Some(300));

        cleanup_connection_shutdown(state.as_ref(), &conn, false).await;

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
    async fn sm_janitor_helper_drains_expired_and_cleans_muc() {
        // Exercise the pieces the janitor composes: drain_expired() returns
        // the removed sessions, and cleanup_muc_presence_for_jid removes the
        // occupant that was held while the session was detached.
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        let state = create_test_websocket_state().await;
        let room_jid: BareJid = "expired_channel@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");

        // Put alice in the room, as if she'd detached with SM.
        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &jid,
            "alice",
            &None,
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
