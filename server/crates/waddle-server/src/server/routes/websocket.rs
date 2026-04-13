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
use futures::{SinkExt, StreamExt};
use jid::{BareJid, FullJid};
use std::{str::FromStr, sync::Arc};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use waddle_xmpp::{
    auth::{parse_oauthbearer, OAuthBearerResult},
    connection::Stanza,
    disco::{
        build_disco_info_response, build_disco_info_response_with_extensions,
        build_disco_items_response, parse_disco_info_query, parse_disco_items_query,
        spaces_service_features, DiscoItem, Feature, Identity,
    },
    mam::{
        add_stanza_id as add_mam_stanza_id, build_fin_iq, build_result_messages, is_mam_query,
        parse_mam_query, ArchivedMessage, LibSqlMamStorage, MamStorage, STANZA_ID_NS,
    },
    muc::{MucRoomRegistry, Occupant, RoomConfig},
    registry::{ConnectionRegistry, OutboundStanza},
    xep::{build_spaces_metadata_form, NS_REPLY},
    Affiliation, Role, WaddleDetails,
};
use xmpp_parsers::message::MessageType as XmppMessageType;
use xmpp_parsers::minidom::Element;

use waddle_xmpp_xep_github::{message_has_github_embed, MessageEnricher};

use super::auth::AuthState;
use crate::auth::{localpart_to_jid, Session};
use crate::server::routes::channels::list_channels_from_db;
use crate::server::routes::waddles::{
    get_waddle_by_id, list_all_waddles_from_db, list_user_waddles,
};
use crate::server::AppState;

/// WebSocket state containing all necessary registries for message routing
pub struct WebSocketState {
    /// Core app state for accessing the global and per-waddle databases.
    pub app_state: Arc<AppState>,
    /// Authentication state for session validation
    pub auth_state: Arc<AuthState>,
    /// Registry for tracking active connections by JID
    pub connection_registry: Arc<ConnectionRegistry>,
    /// Registry for MUC rooms
    pub muc_registry: Arc<MucRoomRegistry>,
    /// Shared XMPP MAM storage for archived message history.
    pub mam_storage: Arc<LibSqlMamStorage>,
    /// GitHub link enricher for message embeds
    pub github_enricher: Arc<MessageEnricher>,
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
    let domain = extract_domain(&state.auth_state.base_url);
    info!(domain = %domain, "XMPP WebSocket connection established");

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Create outbound channel for receiving messages from other connections
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<OutboundStanza>(OUTBOUND_CHANNEL_SIZE);

    // Track connection state
    let mut authenticated = false;
    let mut session_jid: Option<FullJid> = None;
    let mut authenticated_session: Option<Session> = None;
    let mut registered = false;
    let mut resource_bound = false;

    loop {
        tokio::select! {
            // Handle inbound WebSocket messages from the client
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        debug!(len = text.len(), content = %text, "Received XMPP WebSocket message");

                        // Handle XMPP framing (RFC 7395)
                        let responses = handle_xmpp_frame(
                            &text,
                            &domain,
                            &state,
                            &mut authenticated,
                            &mut session_jid,
                            &mut authenticated_session,
                            &mut resource_bound,
                        ).await;

                        // Register connection after successful authentication AND resource binding
                        // This ensures the JID in ConnectionRegistry matches the JID stored in MUC room occupants
                        if authenticated && resource_bound && session_jid.is_some() && !registered {
                            if let Some(ref jid) = session_jid {
                                state.connection_registry.register(jid.clone(), outbound_tx.clone());
                                registered = true;
                                info!(jid = %jid, "WebSocket connection registered");
                            }
                        }

                        for response in responses {
                            debug!(response = %response, "Sending XMPP WebSocket response");
                            if let Err(e) = ws_sender.send(Message::Text(response)).await {
                                error!(error = %e, "Failed to send WebSocket message");
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        warn!("Received binary WebSocket message (not supported for XMPP)");
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if let Err(e) = ws_sender.send(Message::Pong(data)).await {
                            error!(error = %e, "Failed to send pong");
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
                        if let Err(e) = ws_sender.send(Message::Text(xml)).await {
                            error!(error = %e, "Failed to send outbound stanza");
                            // Don't break - the client might still be readable
                        }
                    }
                    None => {
                        // Outbound channel closed - this shouldn't happen during normal operation
                        debug!("Outbound channel closed");
                    }
                }
            }
        }
    }

    // Unregister connection on disconnect
    if let Some(ref jid) = session_jid {
        state.connection_registry.unregister(jid);
        info!(jid = %jid, "WebSocket connection unregistered");

        // Remove from any MUC rooms
        cleanup_muc_presence(&state, jid).await;
    }

    info!("XMPP WebSocket connection closed");
}

/// Clean up MUC room presence when a connection disconnects
async fn cleanup_muc_presence(state: &WebSocketState, jid: &FullJid) {
    // Get all rooms and remove this user from any they're in
    for room_jid in state.muc_registry.list_rooms() {
        if let Some(room_data) = state.muc_registry.get_room_data(&room_jid) {
            let mut room = room_data.write().await;
            if let Some(nick) = room.find_nick_by_real_jid(jid).map(|s| s.to_owned()) {
                room.remove_occupant(&nick);
                debug!(room = %room_jid, nick = %nick, "Removed user from MUC room on disconnect");
            }
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

fn iq_to_xml(iq: xmpp_parsers::iq::Iq) -> String {
    stanza_to_xml(&Stanza::Iq(iq))
}

fn parse_iq_frame(frame: &str) -> Option<xmpp_parsers::iq::Iq> {
    let element = xmpp_parsers::minidom::Element::from_str(frame).ok()?;
    xmpp_parsers::iq::Iq::try_from(element).ok()
}

fn build_iq_error_xml(id: &str, error_type: &str, condition: &str) -> String {
    let iq = xmpp_parsers::minidom::Element::builder("iq", "jabber:client")
        .attr("id", id)
        .attr("type", "error")
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

fn build_stream_features_xml(authenticated: bool) -> String {
    let feature = if authenticated {
        Element::builder("bind", waddle_xmpp::ns::BIND).build()
    } else {
        Element::builder("mechanisms", waddle_xmpp::ns::SASL)
            .append(
                Element::builder("mechanism", waddle_xmpp::ns::SASL)
                    .append("OAUTHBEARER")
                    .build(),
            )
            .build()
    };

    element_to_xml(
        Element::builder("features", waddle_xmpp::ns::STREAM)
            .append(feature)
            .build(),
    )
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

fn parse_sasl_auth_frame(frame: &str) -> Result<(String, String), String> {
    let element =
        Element::from_str(frame).map_err(|err| format!("Invalid SASL auth XML: {err}"))?;
    if element.name() != "auth" {
        return Err("SASL frame is not an auth element".to_string());
    }

    let mechanism = element
        .attr("mechanism")
        .ok_or_else(|| "SASL auth missing mechanism".to_string())?
        .to_string();

    Ok((mechanism, element.text().trim().to_string()))
}

/// Handle an XMPP frame per RFC 7395
async fn handle_xmpp_frame(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    authenticated: &mut bool,
    session_jid: &mut Option<FullJid>,
    authenticated_session: &mut Option<Session>,
    resource_bound: &mut bool,
) -> Vec<String> {
    let frame = frame.trim();
    let muc_domain = format!("muc.{}", domain);

    // RFC 7395: <open> element starts the stream
    if frame.starts_with("<open") {
        info!("XMPP stream open requested");

        // RFC 7395: Send <open> and <stream:features> as SEPARATE WebSocket messages
        let open_element = format!(
            r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" from="{}" id="{}" version="1.0" xml:lang="en"/>"#,
            domain,
            uuid::Uuid::new_v4()
        );
        let features_element = build_stream_features_xml(*authenticated);
        return vec![open_element, features_element];
    }

    // RFC 7395: <close> element ends the stream
    if frame.starts_with("<close") {
        info!("XMPP stream close requested");
        return vec![r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.to_string()];
    }

    if frame.starts_with("<auth") {
        let (mechanism, data) = match parse_sasl_auth_frame(frame) {
            Ok(auth) => auth,
            Err(err) => {
                warn!(error = %err, "Invalid SASL auth frame");
                return vec![sasl_failure_xml("not-authorized")];
            }
        };

        return match mechanism.as_str() {
            "OAUTHBEARER" => {
                handle_sasl_oauthbearer(
                    &data,
                    state,
                    authenticated,
                    session_jid,
                    authenticated_session,
                )
                .await
            }
            mechanism => {
                warn!(mechanism = %mechanism, "Unsupported SASL mechanism");
                vec![sasl_failure_xml("invalid-mechanism")]
            }
        };
    }

    // Handle resource binding
    if frame.contains("urn:ietf:params:xml:ns:xmpp-bind") && frame.starts_with("<iq") {
        let (responses, success) = handle_resource_binding(frame, domain, session_jid);
        if success {
            *resource_bound = true;
        }
        return responses;
    }

    // Handle presence
    if frame.starts_with("<presence") {
        return handle_presence(frame, domain, &muc_domain, state, session_jid).await;
    }

    // Handle IQ stanzas
    if frame.starts_with("<iq") {
        return handle_iq(frame, domain, &muc_domain, state, authenticated_session).await;
    }

    // Handle message stanzas
    if frame.starts_with("<message") {
        return handle_message(
            frame,
            &muc_domain,
            state,
            session_jid,
            authenticated_session,
        )
        .await;
    }

    warn!(frame = %frame, "Unhandled XMPP frame");
    vec![]
}

/// Handle SASL OAUTHBEARER authentication.
async fn handle_sasl_oauthbearer(
    b64_data: &str,
    state: &WebSocketState,
    authenticated: &mut bool,
    session_jid: &mut Option<FullJid>,
    authenticated_session: &mut Option<Session>,
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
        .auth_state
        .session_manager
        .validate_session(&token)
        .await
    {
        Ok(session) => {
            let bare_jid_str =
                match localpart_to_jid(&session.xmpp_localpart, &state.auth_state.xmpp_domain) {
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

            *authenticated = true;
            *authenticated_session = Some(session);
            *session_jid = Some(full_jid);

            vec![sasl_success_xml()]
        }
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER authentication failed");
            vec![sasl_failure_xml("not-authorized")]
        }
    }
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

/// Handle resource binding IQ
/// Returns (responses, success) where success indicates if binding completed successfully
fn handle_resource_binding(
    frame: &str,
    _domain: &str,
    session_jid: &mut Option<FullJid>,
) -> (Vec<String>, bool) {
    let Some(ref jid) = session_jid else {
        warn!("Resource binding without authenticated session");
        return (vec![], false);
    };

    let id = extract_attr(frame, "id").unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let resource =
        extract_element_text(frame, "resource").unwrap_or_else(|| "websocket".to_string());

    // Create the full JID with the requested resource
    let bare_jid = jid.to_bare();
    let full_jid_str = format!("{}/{}", bare_jid, resource);

    if let Ok(full_jid) = full_jid_str.parse::<FullJid>() {
        info!(jid = %full_jid, id = %id, "Resource bound");
        *session_jid = Some(full_jid.clone());

        (vec![build_bind_result_xml(&id, &full_jid)], true)
    } else {
        warn!(jid = %full_jid_str, "Invalid JID during resource binding");
        (vec![], false)
    }
}

/// Handle presence stanzas including MUC join/leave
async fn handle_presence(
    frame: &str,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    session_jid: &Option<FullJid>,
) -> Vec<String> {
    let to = extract_attr(frame, "to");
    let presence_type = extract_attr(frame, "type");

    // Check if this is a MUC presence (to room@muc.domain/nick)
    if let Some(ref to_jid) = to {
        if to_jid.contains(muc_domain) {
            // MUC presence handling
            let parts: Vec<&str> = to_jid.split('/').collect();
            let room_jid_str = parts.first().copied().unwrap_or(to_jid);
            let nick = parts.get(1).copied().unwrap_or("anonymous");

            let Ok(room_jid) = room_jid_str.parse::<BareJid>() else {
                warn!(room = %room_jid_str, "Invalid room JID");
                return vec![];
            };

            let Some(ref sender_jid) = session_jid else {
                warn!("MUC presence without authenticated session");
                return vec![];
            };

            // Check if this is a leave presence
            if presence_type.as_deref() == Some("unavailable") {
                return handle_muc_leave(state, &room_jid, sender_jid, nick).await;
            }

            // This is a join presence
            return handle_muc_join(state, domain, &room_jid, sender_jid, nick).await;
        }
    }

    debug!("Presence stanza received");
    // Regular presence - just acknowledge
    vec![]
}

/// Handle MUC room join
async fn handle_muc_join(
    state: &WebSocketState,
    _domain: &str,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC join request");

    // Get or create the room
    let room_data = match state.muc_registry.get_room_data(room_jid) {
        Some(data) => data,
        None => {
            // Create the room if it doesn't exist
            let config = RoomConfig {
                name: room_jid
                    .node()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "Room".to_string()),
                members_only: false, // Allow anyone to join for now
                ..Default::default()
            };

            // Derive waddle_id and channel_id from the room JID node.
            // Convention: node is "waddle_channel" (underscore-separated).
            let (waddle_id, channel_id) = parse_room_jid_context(room_jid);

            match state.muc_registry.get_or_create_room(
                room_jid.clone(),
                waddle_id,
                channel_id,
                config,
            ) {
                Ok(_handle) => state
                    .muc_registry
                    .get_room_data(room_jid)
                    .expect("Room just created"),
                Err(e) => {
                    warn!(room = %room_jid, error = %e, "Failed to create room");
                    return vec![];
                }
            }
        }
    };

    let mut room = room_data.write().await;

    // Get existing occupants before adding the new one
    let existing_occupants: Vec<(FullJid, String, Affiliation, Role)> = room
        .occupants
        .values()
        .filter(|o| o.real_jid != *sender_jid)
        .map(|o| (o.real_jid.clone(), o.nick.clone(), o.affiliation, o.role))
        .collect();

    // Add the new occupant
    let occupant = Occupant {
        real_jid: sender_jid.clone(),
        nick: nick.to_string(),
        role: Role::Participant,
        affiliation: Affiliation::Member,
        is_remote: false,
        home_server: None,
    };
    room.add_occupant(occupant);

    let occupant_count = room.occupant_count();
    drop(room);

    info!(room = %room_jid, nick = %nick, occupants = occupant_count, "User joined MUC room");

    let mut responses = Vec::new();

    // Send existing occupants' presence to the joining user
    for (existing_jid, existing_nick, affiliation, role) in &existing_occupants {
        let presence = format!(
            r#"<presence from="{}/{}" to="{}"><x xmlns="http://jabber.org/protocol/muc#user"><item affiliation="{}" role="{}" jid="{}"/></x></presence>"#,
            room_jid,
            existing_nick,
            sender_jid,
            affiliation_str(*affiliation),
            role_str(*role),
            existing_jid
        );
        responses.push(presence);
    }

    // Broadcast the new occupant's presence to all existing occupants
    for (existing_jid, _, _, _) in &existing_occupants {
        let presence_stanza =
            create_presence_stanza(room_jid, nick, sender_jid, existing_jid, false);
        let stanza = Stanza::Presence(presence_stanza);
        let _ = state
            .connection_registry
            .send_to(existing_jid, stanza)
            .await;
    }

    // Send self-presence to the joining user (with status code 110)
    let self_presence = format!(
        r#"<presence from="{}/{}" to="{}"><x xmlns="http://jabber.org/protocol/muc#user"><item affiliation="member" role="participant" jid="{}"/><status code="110"/></x></presence>"#,
        room_jid, nick, sender_jid, sender_jid
    );
    responses.push(self_presence);

    // Send room subject
    let room_name = room_jid
        .node()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "Waddle".to_string());
    let subject = format!(
        r#"<message from="{}" to="{}" type="groupchat"><subject>Welcome to {}!</subject></message>"#,
        room_jid, sender_jid, room_name
    );
    responses.push(subject);

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

    let Some(room_data) = state.muc_registry.get_room_data(room_jid) else {
        debug!(room = %room_jid, "Room not found for leave");
        return vec![];
    };

    let mut room = room_data.write().await;

    // Get remaining occupants before removing the leaving user
    let remaining_occupants: Vec<FullJid> = room
        .occupants
        .values()
        .filter(|o| o.real_jid != *sender_jid)
        .map(|o| o.real_jid.clone())
        .collect();

    // Remove the occupant
    room.remove_occupant(nick);
    drop(room);

    // Broadcast unavailable presence to remaining occupants
    for occupant_jid in &remaining_occupants {
        let from_jid = room_jid
            .clone()
            .with_resource_str(nick)
            .unwrap_or_else(|_| sender_jid.clone());
        let mut presence =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
        presence.from = Some(jid::Jid::from(from_jid));
        presence.to = Some(jid::Jid::from(occupant_jid.clone()));
        let stanza = Stanza::Presence(presence);
        let _ = state
            .connection_registry
            .send_to(occupant_jid, stanza)
            .await;
    }

    // Send self-presence unavailable to the leaving user
    vec![format!(
        r#"<presence from="{}/{}" to="{}" type="unavailable"><x xmlns="http://jabber.org/protocol/muc#user"><item affiliation="member" role="none"/><status code="110"/></x></presence>"#,
        room_jid, nick, sender_jid
    )]
}

/// Handle IQ stanzas
async fn handle_iq(
    frame: &str,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    let muc_registry = state.muc_registry.as_ref();
    let spaces_domain = format!("spaces.{domain}");
    let single_tenant = std::env::var("WADDLE_SINGLE_TENANT")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);

    let parsed_iq = parse_iq_frame(frame);
    let id = parsed_iq
        .as_ref()
        .map(|iq| iq.id.clone())
        .or_else(|| extract_attr(frame, "id"))
        .unwrap_or_default();
    let to = parsed_iq
        .as_ref()
        .and_then(|iq| iq.to.as_ref().map(|jid| jid.to_string()))
        .or_else(|| extract_attr(frame, "to"));

    // Ping
    if frame.contains("urn:xmpp:ping") {
        return vec![format!(r#"<iq id="{}" type="result"/>"#, id)];
    }

    // Session establishment (legacy, but some clients need it)
    if frame.contains("urn:ietf:params:xml:ns:xmpp-session") {
        debug!("Session establishment requested");
        return vec![format!(r#"<iq id="{}" type="result"/>"#, id)];
    }

    // Roster query
    if frame.contains("jabber:iq:roster") {
        debug!("Roster query");
        return vec![format!(
            r#"<iq id="{}" type="result"><query xmlns="jabber:iq:roster"/></iq>"#,
            id
        )];
    }

    // Disco info on MUC service
    if frame.contains("http://jabber.org/protocol/disco#info") {
        if let Some(request_iq) = parsed_iq.as_ref() {
            if to.as_deref() == Some(muc_domain) {
                let identities = vec![Identity::muc_service(Some("Waddle Chatrooms"))];
                let features = vec![Feature::muc(), Feature::replies(), Feature::waddle_github()];
                let response = build_disco_info_response(request_iq, &identities, &features, None);
                return vec![iq_to_xml(response)];
            }

            // Disco info on a specific room
            if let Some(target) = to.as_deref() {
                let room_target = target.split('/').next().unwrap_or(target);
                if let Ok(room_jid) = room_target.parse::<BareJid>() {
                    if muc_registry.is_muc_jid(&room_jid) {
                        let room_name = room_jid
                            .node()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "Room".to_string());
                        let identities = vec![Identity::muc_room(Some(&room_name))];
                        let features =
                            vec![Feature::muc(), Feature::replies(), Feature::waddle_github()];
                        let response =
                            build_disco_info_response(request_iq, &identities, &features, None);
                        return vec![iq_to_xml(response)];
                    }
                }
            }

            // Disco info on spaces service
            if to.as_deref() == Some(spaces_domain.as_str()) {
                let query = match parse_disco_info_query(request_iq) {
                    Ok(query) => query,
                    Err(_) => return vec![build_iq_error_xml(&id, "modify", "bad-request")],
                };

                if let Some(node) = query.node.as_deref() {
                    let waddle = match get_waddle_by_id(state.app_state.db_pool.global(), node)
                        .await
                    {
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
                            state.app_state.db_pool.global(),
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

            // Disco info on server
            let identities = vec![Identity::server(Some("Waddle"))];
            let features = vec![
                Feature::ping(),
                Feature::replies(),
                Feature::waddle_github(),
                Feature::disco_info(),
                Feature::disco_items(),
                Feature::spaces(),
            ];
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        return vec![build_iq_error_xml(&id, "modify", "bad-request")];
    }

    // Disco items - list services/rooms
    if frame.contains("http://jabber.org/protocol/disco#items") {
        if let Some(request_iq) = parsed_iq.as_ref() {
            if to.as_deref() == Some(muc_domain) {
                debug!("Disco items query on MUC service");
                let mut rooms = muc_registry.list_rooms();
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

            if to.as_deref() == Some(spaces_domain.as_str()) {
                let Ok(query) = parse_disco_items_query(request_iq) else {
                    return vec![build_iq_error_xml(&id, "modify", "bad-request")];
                };

                let global_db = state.app_state.db_pool.global();
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
                            match state.app_state.db_pool.get_waddle_db(node).await {
                                Ok(waddle_db) => {
                                    match list_channels_from_db(&waddle_db, node, 200, 0).await {
                                        Ok(channels) => channels
                                            .into_iter()
                                            .map(|channel| {
                                                let room_jid = format!(
                                                    "{}_{}@{}",
                                                    node, channel.id, muc_domain
                                                );
                                                DiscoItem::muc_room(&room_jid, &channel.name)
                                            })
                                            .collect(),
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
                                match list_user_waddles(
                                    global_db,
                                    &session.user_id,
                                    PAGE_SIZE,
                                    offset,
                                )
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

                let response =
                    build_disco_items_response(request_iq, &items, query.node.as_deref());
                return vec![iq_to_xml(response)];
            }

            debug!("Disco items query on server");
            let items = vec![
                DiscoItem::muc_service(muc_domain, Some("Chatrooms")),
                DiscoItem::spaces_service(&spaces_domain, Some("Spaces")),
            ];
            let response = build_disco_items_response(request_iq, &items, None);
            return vec![iq_to_xml(response)];
        }

        return vec![build_iq_error_xml(&id, "modify", "bad-request")];
    }

    // MUC owner IQ (XEP-0045): instant room config submit and room destroy.
    // This is needed for clients that create a room by:
    // 1) joining via presence
    // 2) submitting an empty owner form (`jabber:x:data` type='submit')
    if frame.contains("http://jabber.org/protocol/muc#owner") {
        let Some(target) = to.as_deref() else {
            return vec![format!(
                r#"<iq id="{}" type="error"><error type="modify"><bad-request xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#,
                id
            )];
        };

        let room_target = target.split('/').next().unwrap_or(target);
        let Ok(room_jid) = room_target.parse::<BareJid>() else {
            return vec![format!(
                r#"<iq id="{}" type="error"><error type="modify"><jid-malformed xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#,
                id
            )];
        };

        if !muc_registry.is_muc_jid(&room_jid) {
            return vec![format!(
                r#"<iq id="{}" type="error"><error type="cancel"><item-not-found xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#,
                id
            )];
        }

        if frame.contains("<destroy") {
            if muc_registry.destroy_room(&room_jid).is_some() {
                debug!(room = %room_jid, "Destroyed MUC room via owner IQ");
                return vec![format!(
                    r#"<iq id="{}" from="{}" type="result"/>"#,
                    id, room_jid
                )];
            }

            return vec![format!(
                r#"<iq id="{}" type="error"><error type="cancel"><item-not-found xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#,
                id
            )];
        }

        // Treat all other owner IQ sets as successful config submit for instant rooms.
        return vec![format!(
            r#"<iq id="{}" from="{}" type="result"/>"#,
            id, room_jid
        )];
    }

    // MAM (Message Archive Management) query
    if let Some(request_iq) = parsed_iq.as_ref() {
        if is_mam_query(request_iq) {
            let Some(target) = request_iq.to.as_ref().map(|jid| jid.to_string()) else {
                return vec![build_iq_error_xml(&id, "modify", "bad-request")];
            };

            let room_target = target.split('/').next().unwrap_or(target.as_str());
            let Ok(room_jid) = room_target.parse::<BareJid>() else {
                return vec![build_iq_error_xml(&id, "modify", "jid-malformed")];
            };

            if !muc_registry.is_muc_jid(&room_jid) {
                return vec![build_iq_error_xml(&id, "cancel", "item-not-found")];
            }

            let (query_id, query) = match parse_mam_query(request_iq) {
                Ok(parsed) => parsed,
                Err(err) => {
                    warn!(error = %err, room = %room_jid, "Invalid MAM query");
                    return vec![build_iq_error_xml(&id, "modify", "bad-request")];
                }
            };

            let archive_jid = room_jid.to_string();
            let mut result = match state
                .mam_storage
                .query_messages(archive_jid.as_str(), &query)
                .await
            {
                Ok(result) => result,
                Err(err) => {
                    warn!(error = %err, room = %room_jid, "MAM query failed");
                    return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                }
            };

            result.count = state
                .mam_storage
                .count_messages(archive_jid.as_str())
                .await
                .ok();

            let recipient_jid = request_iq
                .from
                .as_ref()
                .map(|jid| jid.to_string())
                .or_else(|| extract_attr(frame, "from"))
                .or_else(|| {
                    authenticated_session
                        .as_ref()
                        .map(|session| format!("{}@{}", session.xmpp_localpart, domain))
                })
                .unwrap_or_else(|| "unknown@localhost".to_string());

            let mut responses: Vec<String> =
                build_result_messages(&query_id, recipient_jid.as_str(), &result.messages)
                    .into_iter()
                    .map(|message| stanza_to_xml(&Stanza::Message(message)))
                    .collect();
            responses.push(iq_to_xml(build_fin_iq(request_iq, &result)));
            return responses;
        }
    }

    // Carbons enable
    if frame.contains("urn:xmpp:carbons:") {
        debug!("Carbons request");
        return vec![format!(r#"<iq id="{}" type="result"/>"#, id)];
    }

    // Unknown IQ - log it and return error
    warn!(id = %id, frame = %frame, "Unhandled IQ stanza");
    vec![format!(
        r#"<iq id="{}" type="error"><error type="cancel"><feature-not-implemented xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#,
        id
    )]
}

/// Handle message stanzas including groupchat routing
async fn handle_message(
    frame: &str,
    muc_domain: &str,
    state: &WebSocketState,
    session_jid: &Option<FullJid>,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    let Some(ref sender_jid) = session_jid else {
        warn!("Message received without authenticated session");
        return vec![];
    };

    let mut incoming = match parse_message_stanza(frame) {
        Ok(msg) => msg,
        Err(err) => {
            warn!(error = %err, "Failed to parse message stanza");
            return vec![];
        }
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

        // Get the room
        let Some(room_data) = state.muc_registry.get_room_data(&room_jid) else {
            warn!(room = %room_jid, "Message to non-existent room");
            return vec![];
        };

        let room = room_data.read().await;

        // Find the sender's nick
        let Some(sender_nick) = room.find_nick_by_real_jid(sender_jid) else {
            warn!(sender = %sender_jid, room = %room_jid, "Sender not in room");
            return vec![];
        };
        let sender_nick = sender_nick.to_string();

        // Get all occupants
        let occupants: Vec<(FullJid, String)> = room
            .occupants
            .values()
            .map(|o| (o.real_jid.clone(), o.nick.clone()))
            .collect();

        drop(room);

        // Build a prototype message, enrich once, then fan out to all occupants.
        let from_room_jid = format!("{}/{}", room_jid, sender_nick);
        let mut prototype = incoming.clone();
        prototype.id = prototype
            .id
            .clone()
            .or_else(|| Some(uuid::Uuid::new_v4().to_string()));
        if let Ok(from_jid) = from_room_jid.parse::<FullJid>() {
            prototype.from = Some(jid::Jid::from(from_jid));
        } else {
            prototype.from = Some(jid::Jid::from(sender_jid.clone()));
        }
        prototype.type_ = XmppMessageType::Groupchat;
        prototype.to = None;

        // Enrich: detect GitHub links and append embed XML elements (fail-open)
        let _embeds_added = state.github_enricher.enrich_message(&mut prototype).await;

        // Archive body-bearing room messages in XMPP MAM storage.
        if let Some(archive_id) = archive_groupchat_message(state, &room_jid, &prototype).await {
            add_mam_stanza_id(&mut prototype, archive_id.as_str(), &room_jid.to_string());
        }

        // Send to all occupants
        let mut echo_response = None;
        for (occupant_jid, _) in &occupants {
            if occupant_jid == sender_jid {
                // Echo back to sender — serialize the enriched prototype
                let stanza = {
                    let mut msg = prototype.clone();
                    msg.to = Some(jid::Jid::from(occupant_jid.clone()));
                    Stanza::Message(msg)
                };
                echo_response = Some(stanza_to_xml(&stanza));
            } else {
                let mut msg = prototype.clone();
                msg.to = Some(jid::Jid::from(occupant_jid.clone()));
                let stanza = Stanza::Message(msg);
                let _ = state
                    .connection_registry
                    .send_to(occupant_jid, stanza)
                    .await;
            }
        }

        info!(
            room = %room_jid,
            sender = %sender_nick,
            recipients = occupants.len(),
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
            prototype.from = Some(jid::Jid::from(sender_jid.clone()));
            prototype.type_ = XmppMessageType::Chat;

            // Enrich: detect GitHub links and append embed XML elements
            let _embeds_added = state.github_enricher.enrich_message(&mut prototype).await;
            let has_github_embed = message_has_github_embed(&prototype);

            // Route the enriched message
            if let Ok(to_full_jid) = to_jid.clone().try_into_full() {
                let mut msg = prototype.clone();
                msg.to = Some(jid::Jid::from(to_full_jid.clone()));
                let stanza = Stanza::Message(msg);
                let _ = state
                    .connection_registry
                    .send_to(&to_full_jid, stanza)
                    .await;
            } else {
                let to_bare_jid = to_jid.to_bare();
                let resources = state
                    .connection_registry
                    .get_resources_for_user(&to_bare_jid);
                for resource_jid in resources {
                    let mut msg = prototype.clone();
                    msg.to = Some(jid::Jid::from(resource_jid.clone()));
                    let stanza = Stanza::Message(msg);
                    let _ = state
                        .connection_registry
                        .send_to(&resource_jid, stanza)
                        .await;
                }
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

async fn archive_groupchat_message(
    state: &WebSocketState,
    room_jid: &BareJid,
    message: &xmpp_parsers::message::Message,
) -> Option<String> {
    let Some(body) = prototype_body(message)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return None;
    };

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
    };

    let archive_jid = room_jid.to_string();
    match state
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

/// Extract an XML attribute value
fn extract_attr(xml: &str, attr: &str) -> Option<String> {
    let pattern = format!("{}=\"", attr);
    if let Some(start) = xml.find(&pattern) {
        let rest = &xml[start + pattern.len()..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    // Also try single quotes
    let pattern = format!("{}='", attr);
    if let Some(start) = xml.find(&pattern) {
        let rest = &xml[start + pattern.len()..];
        if let Some(end) = rest.find('\'') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// Extract text content of an XML element
fn extract_element_text(xml: &str, element: &str) -> Option<String> {
    let open_tag = format!("<{}", element);
    if let Some(start) = xml.find(&open_tag) {
        let rest = &xml[start..];
        if let Some(tag_end) = rest.find('>') {
            let after_tag = &rest[tag_end + 1..];
            let close_tag = format!("</{}", element);
            if let Some(end) = after_tag.find(&close_tag) {
                return Some(after_tag[..end].to_string());
            }
        }
    }
    None
}

/// Parse a raw XMPP `<message/>` frame into an xmpp-parsers message stanza.
fn parse_message_stanza(frame: &str) -> Result<xmpp_parsers::message::Message, String> {
    let patched = add_default_namespace_if_missing(frame, "jabber:client");
    let element: Element = patched
        .parse()
        .map_err(|err| format!("Failed to parse message XML: {}", err))?;
    xmpp_parsers::message::Message::try_from(element)
        .map_err(|err| format!("Invalid message stanza: {:?}", err))
}

fn add_default_namespace_if_missing(xml: &str, default_ns: &str) -> String {
    let trimmed = xml.trim();
    if !trimmed.starts_with("<message") {
        return trimmed.to_string();
    }

    let open_end = match trimmed.find('>') {
        Some(idx) => idx,
        None => return trimmed.to_string(),
    };
    let open_tag = &trimmed[..open_end];

    if open_tag.contains("xmlns=") {
        return trimmed.to_string();
    }

    let tag_end = trimmed[1..]
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .map(|idx| idx + 1)
        .unwrap_or(open_end);

    format!(
        "{} xmlns='{}'{}",
        &trimmed[..tag_end],
        default_ns,
        &trimmed[tag_end..]
    )
}

/// Derive waddle_id and channel_id from a room's bare JID node.
///
/// Convention: node is "waddleId_channelId" (first underscore separates).
/// Falls back to ("default", "default") if the node can't be parsed.
fn parse_room_jid_context(room_jid: &jid::BareJid) -> (String, String) {
    if let Some(node) = room_jid.node() {
        let node_str = node.as_str();
        if let Some(idx) = node_str.find('_') {
            let waddle = &node_str[..idx];
            let channel = &node_str[idx + 1..];
            if !waddle.is_empty() && !channel.is_empty() {
                return (waddle.to_string(), channel.to_string());
            }
        }
    }
    ("default".to_string(), "default".to_string())
}

/// Extract domain from base URL
fn extract_domain(base_url: &str) -> String {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "localhost".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::server::AppState;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use waddle_xmpp_xep_github::GitHubClient;

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

        Arc::new(WebSocketState {
            app_state,
            auth_state,
            connection_registry: Arc::new(ConnectionRegistry::new()),
            muc_registry: Arc::new(MucRoomRegistry::new("muc.example.com".to_string())),
            mam_storage,
            github_enricher: Arc::new(MessageEnricher::new(Arc::new(GitHubClient::new(None)))),
        })
    }

    async fn create_test_session(state: &WebSocketState, username: &str) -> Session {
        let session = Session::new(&uuid::Uuid::new_v4().to_string(), username, username);
        state
            .auth_state
            .session_manager
            .create_session(&session)
            .await
            .expect("session");
        session
    }

    #[tokio::test]
    async fn websocket_features_advertise_oauthbearer() {
        let state = create_test_websocket_state().await;
        let mut authenticated = false;
        let mut session_jid = None;
        let mut authenticated_session = None;
        let mut resource_bound = false;

        let responses = handle_xmpp_frame(
            r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#,
            "example.com",
            state.as_ref(),
            &mut authenticated,
            &mut session_jid,
            &mut authenticated_session,
            &mut resource_bound,
        )
        .await;

        assert_eq!(responses.len(), 2);
        let features = &responses[1];
        assert!(
            features.contains("<mechanism>OAUTHBEARER</mechanism>"),
            "expected OAUTHBEARER in WebSocket SASL mechanisms: {features}"
        );
        assert!(
            !features.contains("<mechanism>PLAIN</mechanism>"),
            "expected WebSocket SASL mechanisms to exclude PLAIN: {features}"
        );
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
        let mut authenticated = false;
        let mut session_jid = None;
        let mut authenticated_session = None;
        let mut resource_bound = false;

        let responses = handle_xmpp_frame(
            &frame,
            "example.com",
            state.as_ref(),
            &mut authenticated,
            &mut session_jid,
            &mut authenticated_session,
            &mut resource_bound,
        )
        .await;

        assert_eq!(responses, vec![sasl_failure_xml("invalid-mechanism")]);
        assert!(!authenticated);
        assert!(!resource_bound);
        assert!(session_jid.is_none());
        assert!(authenticated_session.is_none());
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
        let mut authenticated = false;
        let mut session_jid = None;
        let mut authenticated_session = None;
        let mut resource_bound = false;

        let responses = handle_xmpp_frame(
            &frame,
            "example.com",
            state.as_ref(),
            &mut authenticated,
            &mut session_jid,
            &mut authenticated_session,
            &mut resource_bound,
        )
        .await;

        assert_eq!(responses, vec![sasl_success_xml()]);
        assert!(authenticated);
        assert!(!resource_bound);
        assert_eq!(
            authenticated_session.as_ref().map(|s| s.user_id.as_str()),
            Some(session.user_id.as_str())
        );
        let expected_bare =
            localpart_to_jid(&session.xmpp_localpart, &state.auth_state.xmpp_domain)
                .expect("session localpart should produce JID");
        assert_eq!(
            session_jid.as_ref().map(|jid| jid.to_bare().to_string()),
            Some(expected_bare)
        );
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
        let mut authenticated = false;
        let mut session_jid = None;
        let mut authenticated_session = None;
        let mut resource_bound = false;

        let auth_responses = handle_xmpp_frame(
            &auth_frame,
            "example.com",
            state.as_ref(),
            &mut authenticated,
            &mut session_jid,
            &mut authenticated_session,
            &mut resource_bound,
        )
        .await;
        assert_eq!(auth_responses, vec![sasl_success_xml()]);

        let bind_responses = handle_xmpp_frame(
            &bind_frame,
            "example.com",
            state.as_ref(),
            &mut authenticated,
            &mut session_jid,
            &mut authenticated_session,
            &mut resource_bound,
        )
        .await;

        assert!(resource_bound);
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
            localpart_to_jid(&session.xmpp_localpart, &state.auth_state.xmpp_domain)
                .expect("session localpart should produce JID");
        assert_eq!(jid.text(), format!("{expected_bare}/web"));
        assert_eq!(
            session_jid.as_ref().map(ToString::to_string),
            Some(format!("{expected_bare}/web"))
        );
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

    async fn seed_waddle(
        state: &WebSocketState,
        owner_id: &str,
        waddle_id: &str,
        waddle_name: &str,
        is_public: bool,
    ) {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = state
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
        )
        .await;
        let server_response = server_responses.first().expect("server disco response");
        assert!(server_response.contains("urn:xmpp:reply:0"));
        assert!(server_response.contains("urn:waddle:github:0"));

        let muc_query = r#"<iq xmlns="jabber:client" id="muc1" type="get" to="muc.example.com"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#;
        let muc_responses =
            handle_iq(muc_query, server_domain, muc_domain, state.as_ref(), &None).await;
        let muc_response = muc_responses.first().expect("muc disco response");
        assert!(muc_response.contains("urn:xmpp:reply:0"));
        assert!(muc_response.contains("urn:waddle:github:0"));
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
        let query = r#"<iq xmlns="jabber:client" id="spaces-items" type="get" to="spaces.example.com"><query xmlns="http://jabber.org/protocol/disco#items"/></iq>"#;

        let responses = handle_iq(
            query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &authenticated_session,
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
        let query = r#"<iq xmlns="jabber:client" id="space-node-items" type="get" to="spaces.example.com"><query xmlns="http://jabber.org/protocol/disco#items" node="waddle-bravo"/></iq>"#;

        let responses = handle_iq(
            query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &authenticated_session,
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

        let query = r#"<iq xmlns="jabber:client" id="space-node-info" type="get" to="spaces.example.com"><query xmlns="http://jabber.org/protocol/disco#info" node="waddle-public"/></iq>"#;
        let responses = handle_iq(
            query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(viewer),
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

        let query = r#"<iq xmlns="jabber:client" id="space-node-info-private" type="get" to="spaces.example.com"><query xmlns="http://jabber.org/protocol/disco#info" node="waddle-private"/></iq>"#;
        let responses = handle_iq(
            query,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(viewer),
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
            &frame,
            "muc.example.com",
            state.as_ref(),
            &Some(sender_jid),
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

        let routed = recipient_rx
            .recv()
            .await
            .expect("recipient should receive routed stanza");
        let routed_xml = stanza_to_xml(&routed.stanza);
        assert!(
            routed_xml.contains("to=\"bob@example.com/mobile\"")
                || routed_xml.contains("to='bob@example.com/mobile'"),
            "routed stanza should target recipient resource: {routed_xml}"
        );
        assert!(
            routed_xml.contains("urn:waddle:github:0"),
            "routed stanza should preserve GitHub payload: {routed_xml}"
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
        state
            .muc_registry
            .get_or_create_room(
                room_jid.clone(),
                waddle_id.to_string(),
                channel_id.to_string(),
                RoomConfig::default(),
            )
            .expect("create room");
        let room_data = state
            .muc_registry
            .get_room_data(&room_jid)
            .expect("room data");
        let mut room = room_data.write().await;
        room.add_occupant(Occupant {
            real_jid: sender_jid.clone(),
            nick: "alice".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: false,
            home_server: None,
        });
        drop(room);

        let message_xml = format!(
            "<message xmlns='jabber:client' to='{room_jid}' type='groupchat' id='client-msg-1'>\
                <body>Hello from WebSocket</body>\
             </message>"
        );
        let message_responses = handle_message(
            message_xml.as_str(),
            "muc.example.com",
            state.as_ref(),
            &Some(sender_jid),
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

        let parsed = parse_message_stanza(xml).expect("message parses");
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

        let parsed = parse_message_stanza(xml).expect("message parses");
        let rendered = stanza_to_xml(&Stanza::Message(parsed));
        let reparsed = parse_message_stanza(&rendered).expect("serialized message parses");
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
    fn add_default_namespace_if_missing_for_message() {
        let xml = "<message type='chat'><body>Hi</body></message>";
        let patched = add_default_namespace_if_missing(xml, "jabber:client");
        assert!(patched.contains("xmlns='jabber:client'"));
    }
}
