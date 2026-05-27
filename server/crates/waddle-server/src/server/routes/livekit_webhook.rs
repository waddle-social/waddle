//! `/api/v1/livekit/webhook` ingestion: LiveKit → MUC Muji-presence bridge.
//!
//! LiveKit posts signed webhook deliveries when participant /
//! room lifecycle events fire (see <https://docs.livekit.io/home/server/webhooks/>).
//! This handler is the **server-side authority** that makes the chat
//! UI's XEP-0272 Muji presence state reflect the SFU's truth without
//! waiting for XMPP-level XEP-0198 SM timeouts (~minutes) to clean
//! up dead resources.
//!
//! Wire shape stays XEP-0272 conformant: the handler dispatches the
//! same `ClearMujiPresence` room-actor message the existing
//! client-driven path uses, then broadcasts the resulting per-session
//! Muji state via `<presence/>` to remaining occupants. Server-
//! originated MUC presence is XEP-0045 §7.7-permitted when the server
//! is the MUC service, and is already precedented in the
//! XMPP-disconnect cleanup path (`cleanup.rs`).
//!
//! Signature verification + replay protection live in
//! [`waddle_sfu::verify_webhook_signature`]; this module composes
//! that with the MUC dispatch + SFU registry teardown.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use jid::{BareJid, FullJid};
use tracing::{debug, info, warn};
use waddle_sfu::{
    verify_webhook_signature, CallId, LiveKitWebhookEvent, ParticipantEnvelope, WebhookVerifyError,
};
use waddle_xmpp::muc::build_occupant_presence;
use waddle_xmpp::muc::room_actor::{ClearMujiPresence, MujiPresenceUpdateOutcome};
use waddle_xmpp::xep::xep0272::Muji;
use waddle_xmpp::xep::xep0421::OccupantIdentity;
use waddle_xmpp_core::Stanza;

use super::websocket::{get_room_actor, unregister_participant_from_room, WebSocketState};

/// Upper bound on remembered event ids for delivery deduplication.
/// LiveKit retries up to 5 times per delivery; a small LRU is enough
/// to absorb the retry storm without paying unbounded memory.
const SEEN_EVENT_ID_CAPACITY: usize = 1024;

/// Shared, bounded LRU of LiveKit event ids the handler has already
/// processed. Used so the retries LiveKit sends for the same delivery
/// (per the LK best-practices field guide: up to 5 retries, include a
/// dedupe key) collapse into a single MUC broadcast.
#[derive(Debug, Default)]
pub struct SeenEventIds {
    inner: Mutex<SeenEventIdsInner>,
}

#[derive(Debug, Default)]
struct SeenEventIdsInner {
    order: VecDeque<String>,
    set: std::collections::HashSet<String>,
}

impl SeenEventIds {
    /// Returns `true` if `id` was not previously seen and is now
    /// recorded; `false` if it is a duplicate that should be dropped.
    /// `None`-id events are always treated as fresh (the caller may
    /// still log them, but cannot dedupe).
    pub fn observe(&self, id: Option<&str>) -> bool {
        let Some(id) = id else {
            return true;
        };
        let mut guard = self.inner.lock().expect("SeenEventIds mutex poisoned");
        if !guard.set.insert(id.to_string()) {
            return false;
        }
        guard.order.push_back(id.to_string());
        while guard.order.len() > SEEN_EVENT_ID_CAPACITY {
            if let Some(stale) = guard.order.pop_front() {
                guard.set.remove(&stale);
            }
        }
        true
    }
}

/// Axum router for the LiveKit webhook endpoint. Mounted under
/// `/api/v1/livekit/webhook` by [`crate::server::http`].
pub fn router(websocket_state: Arc<WebSocketState>) -> Router {
    let seen = Arc::new(SeenEventIds::default());
    Router::new()
        .route("/api/v1/livekit/webhook", post(livekit_webhook_handler))
        .layer(Extension(websocket_state))
        .with_state(seen)
}

async fn livekit_webhook_handler(
    Extension(state): Extension<Arc<WebSocketState>>,
    State(seen): State<Arc<SeenEventIds>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        warn!("LiveKit webhook received but no SFU is configured; dropping");
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let secret = sfu.webhook_secret();

    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let event = match verify_webhook_signature(secret, auth, &body) {
        Ok(event) => event,
        Err(WebhookVerifyError::MissingAuthorization)
        | Err(WebhookVerifyError::MalformedAuthorization) => {
            return StatusCode::UNAUTHORIZED;
        }
        Err(WebhookVerifyError::Jwt(error)) => {
            warn!(error = ?error, "LiveKit webhook JWT validation failed");
            return StatusCode::UNAUTHORIZED;
        }
        Err(WebhookVerifyError::MissingBodyHash) | Err(WebhookVerifyError::BodyHashMismatch) => {
            return StatusCode::UNAUTHORIZED;
        }
        Err(WebhookVerifyError::BodyJson(error)) => {
            warn!(error = ?error, "LiveKit webhook body JSON parse failed");
            return StatusCode::BAD_REQUEST;
        }
    };

    if !seen.observe(event.event_id()) {
        debug!(event_id = ?event.event_id(), "LiveKit webhook duplicate; dropping");
        return StatusCode::OK;
    }

    match &event {
        LiveKitWebhookEvent::ParticipantLeft(env)
        | LiveKitWebhookEvent::ParticipantConnectionAborted(env) => {
            process_participant_left(&state, env).await;
        }
        LiveKitWebhookEvent::RoomFinished(env) => {
            info!(room = %env.room.name, "LiveKit reported room finished; clearing SFU registry");
            // The participant set in the SFU registry is the
            // best-effort cleanup target. We don't have per-participant
            // identity here (LK's `room_finished` carries only the
            // room), so any surviving Muji presence will be cleaned by
            // the XMPP-side disconnect path once individual sessions
            // drop. Future enhancement: track participant set in the
            // SFU registry and iterate here.
            if let Ok(call_id) = CallId::new(env.room.name.clone()) {
                // No public iteration API on LiveKitSfu; fall through.
                // The participant_left events for each occupant will
                // arrive separately per the LK delivery spec.
                let _ = (call_id, sfu);
            }
        }
        LiveKitWebhookEvent::ParticipantJoined(_) | LiveKitWebhookEvent::Other => {
            // Informational; the join path already flows through
            // Jingle session-initiate → `register_call_participant`.
        }
    }

    StatusCode::OK
}

async fn process_participant_left(state: &WebSocketState, env: &ParticipantEnvelope) {
    let Ok(full_jid) = env.participant.identity.parse::<FullJid>() else {
        warn!(
            identity = %env.participant.identity,
            room = %env.room.name,
            "LiveKit participant identity is not a valid full JID; skipping cleanup",
        );
        return;
    };
    let Ok(room_jid) = env.room.name.parse::<BareJid>() else {
        warn!(
            room = %env.room.name,
            identity = %env.participant.identity,
            "LiveKit room name is not a valid MUC bare JID; skipping cleanup",
        );
        return;
    };

    debug!(
        room = %room_jid,
        identity = %full_jid,
        event_id = ?env.id,
        "LiveKit webhook: clearing Muji presence for departed participant"
    );

    // Clear the room-actor's authoritative per-session Muji state for
    // this participant. The handler is idempotent — a participant that
    // already left via the XMPP-driven path returns `Ok(None)` and we
    // skip the broadcast.
    let Some(actor) = get_room_actor(state, &room_jid).await else {
        // Room has no active actor (no occupants), nothing to clear.
        return;
    };
    let outcome = match actor
        .ask(ClearMujiPresence {
            sender_jid: full_jid.clone(),
        })
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            debug!(
                room = %room_jid,
                identity = %full_jid,
                "LiveKit webhook: participant not in MUC actor; SFU registry cleanup only"
            );
            unregister_participant_from_room(state, &room_jid, &full_jid);
            return;
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                identity = %full_jid,
                error = ?error,
                "LiveKit webhook: room actor rejected Muji clear; falling through to SFU unregister"
            );
            unregister_participant_from_room(state, &room_jid, &full_jid);
            return;
        }
    };

    broadcast_muji_clear_from_sfu(state, &room_jid, &full_jid, &outcome);
    unregister_participant_from_room(state, &room_jid, &full_jid);
}

/// Broadcast a server-originated Muji-presence clear to every remaining
/// occupant of the room.
///
/// Wire shape mirrors what the client-driven Muji-clear path emits in
/// `muc_update::try_handle_muc_presence_update`: an in-room
/// `<presence/>` per recipient per surviving Muji owner, with the
/// departed participant getting an empty Muji payload (XEP-0272
/// §Leaving: "absence of the `<muji/>` element is the leave marker").
/// Sibling sessions with surviving Muji state retain their advertised
/// `<muji/>` payload so multi-resource participants don't lose
/// preparing/active state held by a different resource.
fn broadcast_muji_clear_from_sfu(
    state: &WebSocketState,
    room_jid: &BareJid,
    leaving_real_jid: &FullJid,
    outcome: &MujiPresenceUpdateOutcome,
) {
    let from_room_jid = room_jid
        .clone()
        .with_resource_str(&outcome.update.sender_nick)
        .unwrap_or_else(|_| leaving_real_jid.clone());

    // Owner entries to reflect: the leaving session (no Muji payload =
    // leave marker) plus every surviving sibling-session Muji.
    let mut entries: Vec<(FullJid, Option<Muji>)> =
        Vec::with_capacity(outcome.session_mujis.len() + 1);
    entries.push((leaving_real_jid.clone(), None));
    for (owner, muji) in &outcome.session_mujis {
        if owner == leaving_real_jid {
            continue;
        }
        entries.push((owner.clone(), Some(muji.clone())));
    }

    for recipient in &outcome.update.recipients {
        for (owner_jid, muji) in &entries {
            let owner_bare = owner_jid.to_bare();
            let identity = OccupantIdentity {
                bare_jid: &owner_bare,
                real_jid: Some(owner_jid),
                secret: &state.deps.occupant_id_secret,
            };
            let is_self = recipient.to_bare() == owner_bare;
            let mut presence = build_occupant_presence(
                &from_room_jid,
                recipient,
                outcome.update.sender_affiliation,
                outcome.update.sender_role,
                is_self,
                &identity,
            );
            if let Some(muji_ref) = muji {
                if !muji_ref.is_empty() {
                    presence.payloads.push(muji_ref.to_element());
                }
            }
            let _ = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(recipient, Stanza::Presence(presence));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_event_ids_deduplicates_repeat_observations() {
        let seen = SeenEventIds::default();
        assert!(seen.observe(Some("EV_1")));
        assert!(!seen.observe(Some("EV_1")));
        assert!(seen.observe(Some("EV_2")));
        assert!(!seen.observe(Some("EV_2")));
    }

    #[test]
    fn seen_event_ids_treats_missing_id_as_fresh() {
        let seen = SeenEventIds::default();
        assert!(seen.observe(None));
        assert!(seen.observe(None));
    }

    #[test]
    fn seen_event_ids_evicts_oldest_past_capacity() {
        let seen = SeenEventIds::default();
        for i in 0..(SEEN_EVENT_ID_CAPACITY + 10) {
            assert!(seen.observe(Some(&format!("EV_{i}"))));
        }
        // The oldest entry should now be evicted, so re-observing it
        // returns "fresh".
        assert!(seen.observe(Some("EV_0")));
    }
}
