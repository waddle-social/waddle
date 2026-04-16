//! SfuServiceActor — top-level Kameo actor for the SFU XMPP component.
//!
//! Receives Jingle IQs addressed to `sfu.{domain}` and dispatches them
//! to the appropriate [`SfuRoomActor`].

use super::room_actor::{AddParticipant, RemoveParticipant, SfuRoomActor};
use super::sdp;
use super::{RoomKey, SfuRegistry};
use crate::xep::xep0166::NS_JINGLE;
use jid::FullJid;
use kameo::Actor;
use minidom::Element;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tracing::{debug, info, warn};
use xmpp_parsers::iq::{Iq, IqType};

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

    async fn resolve_candidate_addr(&self) -> SocketAddr {
        if let Ok(raw_addr) = std::env::var("WADDLE_SFU_CANDIDATE_ADDR") {
            if let Some(candidate_addr) = Self::resolve_env_candidate(&raw_addr).await {
                return candidate_addr;
            }
        }

        if !self.udp_addr.ip().is_unspecified() {
            return self.udp_addr;
        }

        match tokio::net::lookup_host((self.sfu_domain.as_str(), self.udp_addr.port())).await {
            Ok(addresses) => {
                let resolved: Vec<SocketAddr> = addresses
                    .filter(|addr| {
                        let ip = addr.ip();
                        !ip.is_unspecified() && !ip.is_loopback()
                    })
                    .collect();
                if let Some(candidate_addr) = resolved
                    .iter()
                    .copied()
                    .find(|addr| addr.is_ipv4())
                    .or_else(|| resolved.first().copied())
                {
                    info!(
                        domain = %self.sfu_domain,
                        candidate = %candidate_addr,
                        "Resolved SFU candidate address from domain"
                    );
                    return candidate_addr;
                }
            }
            Err(err) => {
                warn!(
                    domain = %self.sfu_domain,
                    error = %err,
                    "Failed to resolve SFU candidate address from domain"
                );
            }
        }

        let fallback = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), self.udp_addr.port());
        warn!(
            bind_addr = %self.udp_addr,
            fallback = %fallback,
            "Falling back to loopback SFU candidate address; set WADDLE_SFU_CANDIDATE_ADDR for production"
        );
        fallback
    }

    /// Parse `WADDLE_SFU_CANDIDATE_ADDR` as either an `ip:port` literal or a
    /// `host:port` that resolves via DNS. Returns `None` for empty values or
    /// when resolution yields no usable address (logging a warning in that
    /// case), so callers can fall back to other discovery paths.
    async fn resolve_env_candidate(raw_addr: &str) -> Option<SocketAddr> {
        let trimmed = raw_addr.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Ok(candidate_addr) = trimmed.parse::<SocketAddr>() {
            return Some(candidate_addr);
        }

        match tokio::net::lookup_host(trimmed).await {
            Ok(addresses) => {
                let resolved: Vec<SocketAddr> = addresses
                    .filter(|addr| {
                        let ip = addr.ip();
                        !ip.is_unspecified() && !ip.is_loopback()
                    })
                    .collect();
                if let Some(addr) = resolved
                    .iter()
                    .copied()
                    .find(|a| a.is_ipv4())
                    .or_else(|| resolved.first().copied())
                {
                    info!(
                        value = %trimmed,
                        candidate = %addr,
                        "Resolved WADDLE_SFU_CANDIDATE_ADDR via DNS"
                    );
                    return Some(addr);
                }
                warn!(
                    value = %trimmed,
                    "WADDLE_SFU_CANDIDATE_ADDR resolved but produced no usable addresses"
                );
                None
            }
            Err(err) => {
                warn!(
                    value = %trimmed,
                    error = %err,
                    "Failed to resolve WADDLE_SFU_CANDIDATE_ADDR; ignoring"
                );
                None
            }
        }
    }

    async fn handle_session_initiate(
        &self,
        iq_id: String,
        sid: String,
        jingle: &Element,
        sender_jid: FullJid,
    ) -> JingleIqResponse {
        let sdp_offer = match sdp::extract_sdp_offer_from_jingle(jingle) {
            Ok(sdp) => sdp,
            Err(e) => {
                warn!(sid = %sid, error = %e, "Failed to extract SDP from session-initiate Jingle element");
                return JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: format!("Failed to extract SDP offer: {e}"),
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
                let candidate_addr = self.resolve_candidate_addr().await;
                let actor_ref = kameo::spawn(SfuRoomActor::new(
                    room_key.clone(),
                    candidate_addr,
                    Arc::clone(&self.registry.peer_store),
                ));
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
        match sdp::build_jingle_session_accept(&sid, &self.sfu_domain, &answer_sdp) {
            Ok(accept_element) => JingleIqResponse::Accept {
                id: iq_id,
                jingle: accept_element,
            },
            Err(e) => {
                warn!(sid = %sid, error = %e, "Failed to build session-accept from SDP answer");
                JingleIqResponse::Rejection {
                    id: iq_id,
                    reason: format!("Failed to build session-accept: {e}"),
                }
            }
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
            IqType::Set(ref el) if el.is("jingle", NS_JINGLE) => el,
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
            "127.0.0.1:10000".parse().expect("valid addr"),
        ));
        // Actor spawned successfully
        let key = RoomKey("test".to_string());
        assert!(registry.get_room(&key).await.is_none());
    }

    #[tokio::test]
    async fn resolve_env_candidate_accepts_ip_literal() {
        let addr = SfuServiceActor::resolve_env_candidate("198.51.100.7:30000").await;
        assert_eq!(
            addr,
            Some("198.51.100.7:30000".parse().expect("valid addr"))
        );
    }

    #[tokio::test]
    async fn resolve_env_candidate_trims_whitespace_around_ip_literal() {
        let addr = SfuServiceActor::resolve_env_candidate("  198.51.100.7:30000  ").await;
        assert_eq!(
            addr,
            Some("198.51.100.7:30000".parse().expect("valid addr"))
        );
    }

    #[tokio::test]
    async fn resolve_env_candidate_returns_none_for_empty() {
        assert_eq!(SfuServiceActor::resolve_env_candidate("").await, None);
        assert_eq!(SfuServiceActor::resolve_env_candidate("   ").await, None);
    }

    #[tokio::test]
    async fn resolve_env_candidate_returns_none_for_unresolvable_hostname() {
        // RFC 2606 reserves .invalid for guaranteed non-resolution.
        let addr = SfuServiceActor::resolve_env_candidate(
            "definitely-not-a-real-host.waddle.invalid:30000",
        )
        .await;
        assert_eq!(addr, None);
    }

    #[tokio::test]
    async fn resolve_env_candidate_returns_none_for_garbage() {
        let addr = SfuServiceActor::resolve_env_candidate("not a socket addr").await;
        assert_eq!(addr, None);
    }
}
