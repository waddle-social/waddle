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
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use super::auth::AuthState;

/// Create the WebSocket router
pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route("/xmpp-websocket", get(xmpp_websocket_handler))
        .with_state(auth_state)
}

/// GET /xmpp-websocket
///
/// WebSocket endpoint for XMPP over WebSocket (RFC 7395).
/// Upgrades HTTP connection to WebSocket and handles XMPP framing.
async fn xmpp_websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AuthState>>,
) -> Response {
    info!("XMPP WebSocket connection request");

    ws.protocols(["xmpp"])
        .on_upgrade(move |socket| handle_xmpp_websocket(socket, state))
}

/// Handle an XMPP WebSocket connection
async fn handle_xmpp_websocket(socket: WebSocket, state: Arc<AuthState>) {
    let domain = extract_domain(&state.base_url);
    info!(domain = %domain, "XMPP WebSocket connection established");

    let (mut sender, mut receiver) = socket.split();

    // Track connection state
    let mut authenticated = false;
    let mut _session_jid: Option<String> = None;

    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                debug!(len = text.len(), content = %text, "Received XMPP WebSocket message");

                // Handle XMPP framing (RFC 7395)
                if let Some(response) = handle_xmpp_frame(&text, &domain, &state, &mut authenticated, &mut _session_jid).await {
                    debug!(response = %response, "Sending XMPP WebSocket response");
                    if let Err(e) = sender.send(Message::Text(response)).await {
                        error!(error = %e, "Failed to send WebSocket message");
                        break;
                    }
                }
            }
            Ok(Message::Binary(_)) => {
                warn!("Received binary WebSocket message (not supported for XMPP)");
            }
            Ok(Message::Ping(data)) => {
                if let Err(e) = sender.send(Message::Pong(data)).await {
                    error!(error = %e, "Failed to send pong");
                    break;
                }
            }
            Ok(Message::Pong(_)) => {
                // Ignore pongs
            }
            Ok(Message::Close(_)) => {
                info!("WebSocket close requested");
                break;
            }
            Err(e) => {
                error!(error = %e, "WebSocket error");
                break;
            }
        }
    }

    info!("XMPP WebSocket connection closed");
}

/// Handle an XMPP frame per RFC 7395
async fn handle_xmpp_frame(
    frame: &str,
    domain: &str,
    state: &AuthState,
    authenticated: &mut bool,
    session_jid: &mut Option<String>,
) -> Option<String> {
    let frame = frame.trim();

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

        return Some(format!(
            r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" from="{}" id="{}" version="1.0" xml:lang="en"/>"#,
            domain,
            uuid::Uuid::new_v4()
        ) + &format!(
            r#"<features xmlns="http://etherx.jabber.org/streams">{}</features>"#,
            features
        ));
    }

    // RFC 7395: <close> element ends the stream
    if frame.starts_with("<close") {
        info!("XMPP stream close requested");
        return Some(r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.to_string());
    }

    // Handle SASL authentication
    if frame.starts_with("<auth") && frame.contains("PLAIN") {
        debug!(frame = %frame, "SASL PLAIN auth attempt");

        // Extract base64 credentials
        if let Some(start) = frame.find('>') {
            if let Some(end) = frame[start..].find('<') {
                let b64_creds = &frame[start + 1..start + end].trim();
                debug!(b64 = %b64_creds, "Extracted base64 credentials");

                match base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    b64_creds,
                ) {
                    Ok(decoded) => {
                        // PLAIN format: \0authzid\0username\0password
                        let parts: Vec<&[u8]> = decoded.split(|&b| b == 0).collect();
                        debug!(parts_count = parts.len(), "SASL PLAIN parts");

                        if parts.len() >= 3 {
                            let authzid = String::from_utf8_lossy(parts[0]);
                            let username = String::from_utf8_lossy(parts[1]);
                            let password = String::from_utf8_lossy(parts[2]);

                            debug!(
                                authzid = %authzid,
                                username = %username,
                                password_len = password.len(),
                                password = %password,
                                "SASL PLAIN credentials"
                            );

                            // The password is the session token
                            match state.session_manager.validate_session(&password).await {
                                Ok(session) => {
                                    info!(jid = %username, did = %session.did, "SASL PLAIN authentication successful");
                                    *authenticated = true;

                                    // Use the username as-is if it contains @, otherwise append domain
                                    let full_jid = if username.contains('@') {
                                        username.to_string()
                                    } else {
                                        format!("{}@{}", username, domain)
                                    };
                                    *session_jid = Some(full_jid);

                                    return Some(
                                        r#"<success xmlns="urn:ietf:params:xml:ns:xmpp-sasl"/>"#.to_string()
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        username = %username,
                                        session_id = %password,
                                        error = %e,
                                        "SASL PLAIN authentication failed - session validation error"
                                    );
                                }
                            }
                        } else if parts.len() == 2 {
                            // Some clients send just \0username\0password
                            let username = String::from_utf8_lossy(parts[0]);
                            let password = String::from_utf8_lossy(parts[1]);

                            debug!(
                                username = %username,
                                password = %password,
                                "SASL PLAIN credentials (2-part format)"
                            );

                            match state.session_manager.validate_session(&password).await {
                                Ok(session) => {
                                    info!(jid = %username, did = %session.did, "SASL PLAIN authentication successful");
                                    *authenticated = true;

                                    let full_jid = if username.contains('@') {
                                        username.to_string()
                                    } else {
                                        format!("{}@{}", username, domain)
                                    };
                                    *session_jid = Some(full_jid);

                                    return Some(
                                        r#"<success xmlns="urn:ietf:params:xml:ns:xmpp-sasl"/>"#.to_string()
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        username = %username,
                                        error = %e,
                                        "SASL PLAIN authentication failed"
                                    );
                                }
                            }
                        } else {
                            warn!(parts_count = parts.len(), "SASL PLAIN: unexpected number of parts");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, b64 = %b64_creds, "Failed to decode base64 credentials");
                    }
                }
            } else {
                warn!("SASL PLAIN: could not find closing tag");
            }
        } else {
            warn!("SASL PLAIN: could not find opening tag end");
        }

        return Some(
            r#"<failure xmlns="urn:ietf:params:xml:ns:xmpp-sasl"><not-authorized/></failure>"#.to_string()
        );
    }

    // Handle resource binding
    if frame.contains("urn:ietf:params:xml:ns:xmpp-bind") && frame.starts_with("<iq") {
        if let Some(jid) = session_jid {
            // Extract the iq id
            let id = extract_attr(frame, "id").unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let resource = extract_element_text(frame, "resource").unwrap_or_else(|| "websocket".to_string());
            let full_jid = format!("{}/{}", jid, resource);

            info!(jid = %full_jid, "Resource bound");

            return Some(format!(
                r#"<iq id="{}" type="result"><bind xmlns="urn:ietf:params:xml:ns:xmpp-bind"><jid>{}</jid></bind></iq>"#,
                id, full_jid
            ));
        }
    }

    // Handle presence
    if frame.starts_with("<presence") {
        let to = extract_attr(frame, "to");

        // MUC join (presence to room@muc.domain/nick)
        if let Some(to_jid) = &to {
            let muc_domain = format!("muc.{}", domain);
            if to_jid.contains(&muc_domain) {
                info!(room = %to_jid, "MUC join presence");

                // Extract room and nick from to_jid (room@muc.domain/nick)
                let parts: Vec<&str> = to_jid.split('/').collect();
                let room_jid = parts.first().copied().unwrap_or(to_jid);
                let nick = parts.get(1).copied().unwrap_or("anonymous");

                // Send self-presence (indicating successful join)
                let self_presence = format!(
                    r#"<presence from="{}/{}" to="{}"><x xmlns="http://jabber.org/protocol/muc#user"><item affiliation="member" role="participant"/><status code="110"/></x></presence>"#,
                    room_jid, nick, session_jid.as_deref().unwrap_or("unknown")
                );

                // Send room subject
                let subject = format!(
                    r#"<message from="{}" to="{}" type="groupchat"><subject>Welcome to Waddle!</subject></message>"#,
                    room_jid, session_jid.as_deref().unwrap_or("unknown")
                );

                return Some(format!("{}{}", self_presence, subject));
            }
        }

        debug!("Presence stanza received");
        // Regular presence - just acknowledge
        return None;
    }

    // Handle IQ stanzas
    if frame.starts_with("<iq") {
        let id = extract_attr(frame, "id").unwrap_or_default();
        let to = extract_attr(frame, "to");
        let muc_domain = format!("muc.{}", domain);

        // Ping
        if frame.contains("urn:xmpp:ping") {
            return Some(format!(r#"<iq id="{}" type="result"/>"#, id));
        }

        // Session establishment (legacy, but some clients need it)
        if frame.contains("urn:ietf:params:xml:ns:xmpp-session") {
            debug!("Session establishment requested");
            return Some(format!(r#"<iq id="{}" type="result"/>"#, id));
        }

        // Roster query
        if frame.contains("jabber:iq:roster") {
            debug!("Roster query");
            return Some(format!(
                r#"<iq id="{}" type="result"><query xmlns="jabber:iq:roster"/></iq>"#,
                id
            ));
        }

        // Disco info on MUC service
        if frame.contains("http://jabber.org/protocol/disco#info") {
            if to.as_deref() == Some(&muc_domain) {
                return Some(format!(
                    r#"<iq id="{}" from="{}" type="result"><query xmlns="http://jabber.org/protocol/disco#info"><identity category="conference" type="text" name="Waddle Chatrooms"/><feature var="http://jabber.org/protocol/muc"/></query></iq>"#,
                    id, muc_domain
                ));
            }
            // Disco info on server
            return Some(format!(
                r#"<iq id="{}" from="{}" type="result"><query xmlns="http://jabber.org/protocol/disco#info"><identity category="server" type="im" name="Waddle"/><feature var="urn:xmpp:ping"/><feature var="http://jabber.org/protocol/disco#info"/><feature var="http://jabber.org/protocol/disco#items"/></query></iq>"#,
                id, domain
            ));
        }

        // Disco items - list services/rooms
        if frame.contains("http://jabber.org/protocol/disco#items") {
            // Disco items on MUC service - list available rooms
            if to.as_deref() == Some(&muc_domain) {
                debug!("Disco items query on MUC service");
                // TODO: Query actual rooms from database
                // For now return a default "lobby" room
                return Some(format!(
                    r#"<iq id="{}" from="{}" type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="lobby@{}" name="Lobby"/></query></iq>"#,
                    id, muc_domain, muc_domain
                ));
            }
            // Disco items on server - list available services
            debug!("Disco items query on server");
            return Some(format!(
                r#"<iq id="{}" from="{}" type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="{}" name="Chatrooms"/></query></iq>"#,
                id, domain, muc_domain
            ));
        }

        // MAM (Message Archive Management) query
        if frame.contains("urn:xmpp:mam:") {
            debug!("MAM query");
            // Return empty archive for now
            return Some(format!(
                r#"<iq id="{}" type="result"><fin xmlns="urn:xmpp:mam:2" complete="true"><set xmlns="http://jabber.org/protocol/rsm"><count>0</count></set></fin></iq>"#,
                id
            ));
        }

        // Carbons enable
        if frame.contains("urn:xmpp:carbons:") {
            debug!("Carbons request");
            return Some(format!(r#"<iq id="{}" type="result"/>"#, id));
        }

        // Unknown IQ - log it and return error
        warn!(id = %id, frame = %frame, "Unhandled IQ stanza");
        return Some(format!(
            r#"<iq id="{}" type="error"><error type="cancel"><feature-not-implemented xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#,
            id
        ));
    }

    // Handle message stanzas
    if frame.starts_with("<message") {
        debug!("Message stanza received");
        // TODO: Route messages
        return None;
    }

    warn!(frame = %frame, "Unhandled XMPP frame");
    None
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

/// Extract domain from base URL
fn extract_domain(base_url: &str) -> String {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "localhost".to_string())
}
