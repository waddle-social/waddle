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
use futures::{SinkExt, StreamExt};
use jid::{BareJid, FullJid};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use waddle_xmpp::{
    connection::Stanza,
    muc::{MucRoomRegistry, Occupant, RoomConfig},
    registry::{ConnectionRegistry, OutboundStanza},
    Affiliation, Role,
};

use super::auth::AuthState;

/// WebSocket state containing all necessary registries for message routing
pub struct WebSocketState {
    /// Authentication state for session validation
    pub auth_state: Arc<AuthState>,
    /// Registry for tracking active connections by JID
    pub connection_registry: Arc<ConnectionRegistry>,
    /// Registry for MUC rooms
    pub muc_registry: Arc<MucRoomRegistry>,
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
fn stanza_to_xml(stanza: &Stanza) -> String {
    match stanza {
        Stanza::Message(msg) => {
            // Build XML for message
            let to = msg
                .to
                .as_ref()
                .map(|j| format!(" to=\"{}\"", j))
                .unwrap_or_default();
            let from = msg
                .from
                .as_ref()
                .map(|j| format!(" from=\"{}\"", j))
                .unwrap_or_default();
            let id = msg
                .id
                .as_ref()
                .map(|i| format!(" id=\"{}\"", i))
                .unwrap_or_default();
            let msg_type = match msg.type_ {
                xmpp_parsers::message::MessageType::Chat => " type=\"chat\"",
                xmpp_parsers::message::MessageType::Groupchat => " type=\"groupchat\"",
                xmpp_parsers::message::MessageType::Normal => " type=\"normal\"",
                xmpp_parsers::message::MessageType::Headline => " type=\"headline\"",
                xmpp_parsers::message::MessageType::Error => " type=\"error\"",
            };

            let body = msg
                .bodies
                .get("")
                .or_else(|| msg.bodies.values().next())
                .map(|b| format!("<body>{}</body>", escape_xml(&b.0)))
                .unwrap_or_default();

            format!(
                "<message{}{}{}{}>{}</message>",
                to, from, id, msg_type, body
            )
        }
        Stanza::Presence(pres) => {
            let to = pres
                .to
                .as_ref()
                .map(|j| format!(" to=\"{}\"", j))
                .unwrap_or_default();
            let from = pres
                .from
                .as_ref()
                .map(|j| format!(" from=\"{}\"", j))
                .unwrap_or_default();
            let pres_type = match pres.type_ {
                xmpp_parsers::presence::Type::None => "",
                xmpp_parsers::presence::Type::Unavailable => " type=\"unavailable\"",
                _ => "",
            };
            format!("<presence{}{}{}/>", to, from, pres_type)
        }
        Stanza::Iq(iq) => {
            let to = iq
                .to
                .as_ref()
                .map(|j| format!(" to=\"{}\"", j))
                .unwrap_or_default();
            let from = iq
                .from
                .as_ref()
                .map(|j| format!(" from=\"{}\"", j))
                .unwrap_or_default();
            let iq_type = match iq.payload {
                xmpp_parsers::iq::IqType::Get(_) => "get",
                xmpp_parsers::iq::IqType::Set(_) => "set",
                xmpp_parsers::iq::IqType::Result(_) => "result",
                xmpp_parsers::iq::IqType::Error(_) => "error",
            };
            format!("<iq{}{} id=\"{}\" type=\"{}\"/>", to, from, iq.id, iq_type)
        }
    }
}

/// Escape XML special characters
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Handle an XMPP frame per RFC 7395
async fn handle_xmpp_frame(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    authenticated: &mut bool,
    session_jid: &mut Option<FullJid>,
    resource_bound: &mut bool,
) -> Vec<String> {
    let frame = frame.trim();
    let muc_domain = format!("muc.{}", domain);

    // RFC 7395: <open> element starts the stream
    if frame.starts_with("<open") {
        info!("XMPP stream open requested");

        // Respond with stream features
        let features = if *authenticated {
            // Post-auth features
            r#"<bind xmlns="urn:ietf:params:xml:ns:xmpp-bind"/>"#
        } else {
            // Pre-auth features - offer PLAIN auth with session token
            r#"<mechanisms xmlns="urn:ietf:params:xml:ns:xmpp-sasl">
                <mechanism>PLAIN</mechanism>
            </mechanisms>"#
        };

        // RFC 7395: Send <open> and <stream:features> as SEPARATE WebSocket messages
        let open_element = format!(
            r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" from="{}" id="{}" version="1.0" xml:lang="en"/>"#,
            domain,
            uuid::Uuid::new_v4()
        );
        let features_element = format!(
            r#"<stream:features xmlns:stream="http://etherx.jabber.org/streams">{}</stream:features>"#,
            features
        );
        return vec![open_element, features_element];
    }

    // RFC 7395: <close> element ends the stream
    if frame.starts_with("<close") {
        info!("XMPP stream close requested");
        return vec![r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.to_string()];
    }

    // Handle SASL authentication
    if frame.starts_with("<auth") && frame.contains("PLAIN") {
        return handle_sasl_plain(frame, domain, state, authenticated, session_jid).await;
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
        return handle_iq(frame, domain, &muc_domain);
    }

    // Handle message stanzas
    if frame.starts_with("<message") {
        return handle_message(frame, &muc_domain, state, session_jid).await;
    }

    warn!(frame = %frame, "Unhandled XMPP frame");
    vec![]
}

/// Handle SASL PLAIN authentication
async fn handle_sasl_plain(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    authenticated: &mut bool,
    session_jid: &mut Option<FullJid>,
) -> Vec<String> {
    debug!(frame = %frame, "SASL PLAIN auth attempt");

    // Extract base64 credentials
    let Some(start) = frame.find('>') else {
        warn!("SASL PLAIN: could not find opening tag end");
        return vec![
            r#"<failure xmlns="urn:ietf:params:xml:ns:xmpp-sasl"><not-authorized/></failure>"#
                .to_string(),
        ];
    };

    let Some(end) = frame[start..].find('<') else {
        warn!("SASL PLAIN: could not find closing tag");
        return vec![
            r#"<failure xmlns="urn:ietf:params:xml:ns:xmpp-sasl"><not-authorized/></failure>"#
                .to_string(),
        ];
    };

    let b64_creds = frame[start + 1..start + end].trim();
    debug!(b64 = %b64_creds, "Extracted base64 credentials");

    let decoded =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64_creds) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, b64 = %b64_creds, "Failed to decode base64 credentials");
                return vec![
                r#"<failure xmlns="urn:ietf:params:xml:ns:xmpp-sasl"><not-authorized/></failure>"#
                    .to_string(),
            ];
            }
        };

    // PLAIN format: \0authzid\0username\0password or \0username\0password
    let parts: Vec<&[u8]> = decoded.split(|&b| b == 0).collect();
    debug!(parts_count = parts.len(), "SASL PLAIN parts");

    let (username, password) = if parts.len() >= 3 {
        (
            String::from_utf8_lossy(parts[1]),
            String::from_utf8_lossy(parts[2]),
        )
    } else if parts.len() == 2 {
        (
            String::from_utf8_lossy(parts[0]),
            String::from_utf8_lossy(parts[1]),
        )
    } else {
        warn!(
            parts_count = parts.len(),
            "SASL PLAIN: unexpected number of parts"
        );
        return vec![
            r#"<failure xmlns="urn:ietf:params:xml:ns:xmpp-sasl"><not-authorized/></failure>"#
                .to_string(),
        ];
    };

    debug!(username = %username, password_len = password.len(), "SASL PLAIN credentials");

    // The password is the session token
    match state
        .auth_state
        .session_manager
        .validate_session(&password)
        .await
    {
        Ok(session) => {
            info!(jid = %username, did = %session.did, "SASL PLAIN authentication successful");
            *authenticated = true;

            // Create a bare JID string (full JID is set during resource binding)
            let bare_jid_str = if username.contains('@') {
                username.to_string()
            } else {
                format!("{}@{}", username, domain)
            };

            // Store as a temporary placeholder - will be replaced during resource binding
            // For now, create with a temporary resource
            if let Ok(full_jid) = format!("{}/pending", bare_jid_str).parse::<FullJid>() {
                *session_jid = Some(full_jid);
            }

            vec![r#"<success xmlns="urn:ietf:params:xml:ns:xmpp-sasl"/>"#.to_string()]
        }
        Err(e) => {
            warn!(username = %username, error = %e, "SASL PLAIN authentication failed");
            vec![
                r#"<failure xmlns="urn:ietf:params:xml:ns:xmpp-sasl"><not-authorized/></failure>"#
                    .to_string(),
            ]
        }
    }
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
        info!(jid = %full_jid, "Resource bound");
        *session_jid = Some(full_jid.clone());

        (
            vec![format!(
                r#"<iq id="{}" type="result"><bind xmlns="urn:ietf:params:xml:ns:xmpp-bind"><jid>{}</jid></bind></iq>"#,
                id, full_jid
            )],
            true,
        )
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
    domain: &str,
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
fn handle_iq(frame: &str, domain: &str, muc_domain: &str) -> Vec<String> {
    let id = extract_attr(frame, "id").unwrap_or_default();
    let to = extract_attr(frame, "to");

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
        if to.as_deref() == Some(muc_domain) {
            return vec![format!(
                r#"<iq id="{}" from="{}" type="result"><query xmlns="http://jabber.org/protocol/disco#info"><identity category="conference" type="text" name="Waddle Chatrooms"/><feature var="http://jabber.org/protocol/muc"/></query></iq>"#,
                id, muc_domain
            )];
        }
        // Disco info on server
        return vec![format!(
            r#"<iq id="{}" from="{}" type="result"><query xmlns="http://jabber.org/protocol/disco#info"><identity category="server" type="im" name="Waddle"/><feature var="urn:xmpp:ping"/><feature var="http://jabber.org/protocol/disco#info"/><feature var="http://jabber.org/protocol/disco#items"/></query></iq>"#,
            id, domain
        )];
    }

    // Disco items - list services/rooms
    if frame.contains("http://jabber.org/protocol/disco#items") {
        if to.as_deref() == Some(muc_domain) {
            debug!("Disco items query on MUC service");
            return vec![format!(
                r#"<iq id="{}" from="{}" type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="lobby@{}" name="Lobby"/></query></iq>"#,
                id, muc_domain, muc_domain
            )];
        }
        debug!("Disco items query on server");
        return vec![format!(
            r#"<iq id="{}" from="{}" type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="{}" name="Chatrooms"/></query></iq>"#,
            id, domain, muc_domain
        )];
    }

    // MAM (Message Archive Management) query
    if frame.contains("urn:xmpp:mam:") {
        debug!("MAM query");
        return vec![format!(
            r#"<iq id="{}" type="result"><fin xmlns="urn:xmpp:mam:2" complete="true"><set xmlns="http://jabber.org/protocol/rsm"><count>0</count></set></fin></iq>"#,
            id
        )];
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
) -> Vec<String> {
    let msg_type = extract_attr(frame, "type");
    let to = extract_attr(frame, "to");
    let id = extract_attr(frame, "id");
    let body = extract_element_text(frame, "body");

    let Some(ref sender_jid) = session_jid else {
        warn!("Message received without authenticated session");
        return vec![];
    };

    // Handle groupchat messages
    if msg_type.as_deref() == Some("groupchat") {
        let Some(ref to_jid_str) = to else {
            warn!("Groupchat message without 'to' attribute");
            return vec![];
        };

        if !to_jid_str.contains(muc_domain) {
            warn!(to = %to_jid_str, "Groupchat message to non-MUC JID");
            return vec![];
        }

        // Parse room JID (strip resource if present)
        let room_jid_str = to_jid_str.split('/').next().unwrap_or(to_jid_str);
        let Ok(room_jid) = room_jid_str.parse::<BareJid>() else {
            warn!(room = %room_jid_str, "Invalid room JID in message");
            return vec![];
        };

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

        // Build the message from the room JID with sender's nick
        let from_room_jid = format!("{}/{}", room_jid, sender_nick);
        let msg_id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let body_text = body.unwrap_or_default();

        // Send to all occupants
        let mut echo_response = None;
        for (occupant_jid, _) in &occupants {
            let msg_xml = format!(
                r#"<message from="{}" to="{}" id="{}" type="groupchat"><body>{}</body></message>"#,
                from_room_jid,
                occupant_jid,
                msg_id,
                escape_xml(&body_text)
            );

            if occupant_jid == sender_jid {
                // This is the echo back to the sender
                echo_response = Some(msg_xml);
            } else {
                // Route to other occupants via the connection registry
                let mut msg =
                    xmpp_parsers::message::Message::new(Some(jid::Jid::from(occupant_jid.clone())));
                if let Ok(from_jid) = from_room_jid.parse::<FullJid>() {
                    msg.from = Some(jid::Jid::from(from_jid));
                } else {
                    msg.from = Some(jid::Jid::from(sender_jid.clone()));
                }
                msg.id = Some(msg_id.clone());
                msg.type_ = xmpp_parsers::message::MessageType::Groupchat;
                msg.bodies.insert(
                    String::new(),
                    xmpp_parsers::message::Body(body_text.clone()),
                );
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
    if msg_type.as_deref() == Some("chat") {
        if let Some(ref to_jid_str) = to {
            debug!(to = %to_jid_str, from = %sender_jid, "Direct chat message");

            // Try to parse as FullJid first, then BareJid
            if let Ok(to_full_jid) = to_jid_str.parse::<FullJid>() {
                let mut msg =
                    xmpp_parsers::message::Message::new(Some(jid::Jid::from(to_full_jid.clone())));
                msg.from = Some(jid::Jid::from(sender_jid.clone()));
                msg.id = id.clone();
                msg.type_ = xmpp_parsers::message::MessageType::Chat;
                if let Some(b) = body {
                    msg.bodies
                        .insert(String::new(), xmpp_parsers::message::Body(b));
                }
                let stanza = Stanza::Message(msg);
                let _ = state
                    .connection_registry
                    .send_to(&to_full_jid, stanza)
                    .await;
            } else if let Ok(to_bare_jid) = to_jid_str.parse::<BareJid>() {
                // Route to all resources of this bare JID
                let resources = state
                    .connection_registry
                    .get_resources_for_user(&to_bare_jid);
                for resource_jid in resources {
                    let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
                        resource_jid.clone(),
                    )));
                    msg.from = Some(jid::Jid::from(sender_jid.clone()));
                    msg.id = id.clone();
                    msg.type_ = xmpp_parsers::message::MessageType::Chat;
                    if let Some(ref b) = body {
                        msg.bodies
                            .insert(String::new(), xmpp_parsers::message::Body(b.clone()));
                    }
                    let stanza = Stanza::Message(msg);
                    let _ = state
                        .connection_registry
                        .send_to(&resource_jid, stanza)
                        .await;
                }
            }
        }
        return vec![];
    }

    debug!(msg_type = ?msg_type, "Message stanza received");
    vec![]
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
}
