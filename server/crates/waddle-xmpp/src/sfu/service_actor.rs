//! SfuServiceActor — top-level Kameo actor for the SFU XMPP component.
//!
//! Receives Jingle IQs addressed to `sfu.{domain}` and dispatches them
//! to the appropriate [`SfuRoomActor`].

use super::room_actor::{AddParticipant, RemoveParticipant, SfuRoomActor};
use super::sdp;
use super::{RoomKey, SfuRegistry};
use jid::FullJid;
use kameo::Actor;
use minidom::Element;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{debug, info, warn};
use xmpp_parsers::iq::{Iq, IqType};

const JINGLE_NS: &str = "urn:xmpp:jingle:1";

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// Top-level SFU XMPP component actor that dispatches Jingle IQs to room actors.
#[derive(Actor)]
pub struct SfuServiceActor {
    sfu_domain: String,
    registry: Arc<SfuRegistry>,
    udp_addr: SocketAddr,
}

impl SfuServiceActor {
    pub fn new(sfu_domain: String, registry: Arc<SfuRegistry>, udp_addr: SocketAddr) -> Self {
        Self {
            sfu_domain,
            registry,
            udp_addr,
        }
    }

    async fn handle_session_initiate(
        &self,
        iq_id: String,
        sid: String,
        jingle: &Element,
        sender_jid: FullJid,
    ) -> JingleIqResponse {
        let sdp_offer = match sdp::extract_sdp_from_jingle(jingle) {
            Some(sdp) => sdp,
            None => {
                warn!(sid = %sid, "No SDP found in session-initiate Jingle element");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: "Missing SDP offer in session-initiate".to_string(),
                };
            }
        };

        let room_key = match RoomKey::from_session_id(&sid) {
            Some(key) => key,
            None => {
                warn!(sid = %sid, "Invalid session ID format");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: format!("Invalid session ID format: {sid}"),
                };
            }
        };

        // Get or create the room actor.
        let room_ref = match self.registry.get_room(&room_key).await {
            Some(r) => r,
            None => {
                info!(room = %room_key.0, "Creating new SFU room actor");
                let actor_ref = kameo::spawn(SfuRoomActor::new(room_key.clone(), self.udp_addr));
                self.registry.insert_room(room_key, actor_ref.clone()).await;
                actor_ref
            }
        };

        // Ask the room actor to add the participant.
        let answer_sdp = match room_ref
            .ask(AddParticipant {
                sid: sid.clone(),
                jid: sender_jid.clone(),
                sdp_offer,
            })
            .await
        {
            Ok(answer) => answer,
            Err(e) => {
                warn!(sid = %sid, error = %e, "Failed to add participant to SFU room");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: format!("Failed to add participant: {e}"),
                };
            }
        };

        debug!(sid = %sid, jid = %sender_jid, "Session initiated successfully");
        let accept_element = sdp::build_jingle_session_accept(&sid, &answer_sdp);
        JingleIqResponse::Accept {
            id: iq_id,
            jingle: accept_element,
        }
    }

    async fn handle_session_terminate(&self, iq_id: String, sid: String) -> JingleIqResponse {
        let room_key = match RoomKey::from_session_id(&sid) {
            Some(key) => key,
            None => {
                warn!(sid = %sid, "Invalid session ID format in session-terminate");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: format!("Invalid session ID format: {sid}"),
                };
            }
        };

        let room_ref = match self.registry.get_room(&room_key).await {
            Some(r) => r,
            None => {
                warn!(sid = %sid, room = %room_key.0, "Room not found for session-terminate");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: format!("No active room for sid '{sid}'"),
                };
            }
        };

        match room_ref.ask(RemoveParticipant { sid: sid.clone() }).await {
            Ok(true) => {
                info!(room = %room_key.0, "Room is now empty, removing from registry");
                self.registry.remove_room(&room_key).await;
            }
            Ok(false) => {
                debug!(sid = %sid, "Participant removed, room still active");
            }
            Err(e) => {
                warn!(sid = %sid, error = %e, "Failed to remove participant from room");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: format!("Failed to remove participant: {e}"),
                };
            }
        }

        JingleIqResponse::Ack { id: iq_id }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Incoming Jingle IQ to be dispatched.
pub struct HandleJingleIq {
    pub iq: Iq,
    pub sender_jid: FullJid,
}

/// Response variants for a handled Jingle IQ.
#[derive(kameo::Reply)]
pub enum JingleIqResponse {
    /// Session accepted with a Jingle accept element.
    Accept { id: String, jingle: Element },
    /// Simple acknowledgement (empty result IQ).
    Ack { id: String },
    /// Error response with a reason string.
    Rejection { id: String, reason: String },
}

impl kameo::message::Message<HandleJingleIq> for SfuServiceActor {
    type Reply = JingleIqResponse;

    async fn handle(
        &mut self,
        msg: HandleJingleIq,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let iq_id = msg.iq.id.clone();

        // Extract the Jingle element from the IQ payload.
        let jingle = match msg.iq.payload {
            IqType::Set(ref el) if el.is("jingle", JINGLE_NS) => el,
            _ => {
                warn!(id = %iq_id, "Expected IQ set with Jingle payload");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: "Expected IQ type='set' with a Jingle element".to_string(),
                };
            }
        };

        let action = match sdp::extract_action(jingle) {
            Some(a) => a.to_string(),
            None => {
                warn!(id = %iq_id, "Jingle element missing 'action' attribute");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: "Missing 'action' attribute on Jingle element".to_string(),
                };
            }
        };

        let sid = match sdp::extract_sid(jingle) {
            Some(s) => s.to_string(),
            None => {
                warn!(id = %iq_id, "Jingle element missing 'sid' attribute");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: "Missing 'sid' attribute on Jingle element".to_string(),
                };
            }
        };

        debug!(id = %iq_id, action = %action, sid = %sid, "Dispatching Jingle IQ");

        match action.as_str() {
            "session-initiate" => {
                self.handle_session_initiate(iq_id, sid, jingle, msg.sender_jid)
                    .await
            }
            "session-terminate" => self.handle_session_terminate(iq_id, sid).await,
            "transport-info" => {
                debug!(sid = %sid, "transport-info acknowledged (placeholder)");
                JingleIqResponse::Ack { id: iq_id }
            }
            other => {
                warn!(action = %other, sid = %sid, "Unsupported Jingle action");
                JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: format!("Unsupported Jingle action: {other}"),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_service_actor() {
        let registry = Arc::new(SfuRegistry::new());
        let _actor = kameo::spawn(SfuServiceActor::new(
            "sfu.waddle.social".to_string(),
            registry.clone(),
            "127.0.0.1:10000".parse().unwrap(),
        ));
        // Actor spawned successfully
        let key = RoomKey("test".to_string());
        assert!(registry.get_room(&key).await.is_none());
    }
}
