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
use std::time::Duration as StdDuration;

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use jid::{BareJid, FullJid};
use tracing::{debug, info, warn};
use waddle_sfu::{
    verify_webhook_signature, CallId, LiveKitWebhookEvent, ParticipantEnvelope, SfuReconciler,
    WebhookVerifyError, RECONCILE_GRACE_SECONDS,
};

use super::muc_muji_clear::clear_muji_presence_for_departure;
use super::websocket::WebSocketState;

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
            // LiveKit typically emits `participant_left` for every
            // still-connected participant before `room_finished`, but
            // each event is retried independently — a brief outage on
            // our endpoint can exhaust the 5-retry budget for some
            // participants while later events deliver successfully.
            // Iterate the SFU registry's surviving identities and
            // clear each one's MUC state as a safety net so a
            // straggling participant_left loss can't leave permanent
            // ghost Muji presence in the room.
            info!(room = %env.room.name, "LiveKit reported room finished; sweeping surviving participants");
            if let Ok(call_id) = CallId::new(env.room.name.clone()) {
                let survivors = sfu.participants_for_call(&call_id);
                for identity in survivors {
                    process_participant_left_for_identity(
                        &state,
                        &env.room.name,
                        identity.as_jid().to_string().as_str(),
                    )
                    .await;
                }
            } else {
                warn!(
                    room = %env.room.name,
                    "LiveKit room_finished room name is not a valid MUC bare JID; cannot sweep survivors",
                );
            }
        }
        LiveKitWebhookEvent::ParticipantJoined(_) | LiveKitWebhookEvent::Other => {
            // Informational; the join path already flows through
            // Jingle session-initiate → `register_call_participant`.
        }
    }

    StatusCode::OK
}

/// How often the reconciliation backstop polls LiveKit for the true
/// participant set of every active call. Frequent enough that a ghost
/// left by a lost `participant_left`/`room_finished` webhook clears
/// within ~a minute; infrequent enough that the `ListParticipants`
/// fan-out is negligible even with many concurrent calls.
const RECONCILE_INTERVAL: StdDuration = StdDuration::from_secs(60);

/// `true` when `call_id` is a MUC (Muji) room JID and therefore has
/// MUC presence to clear. A MUC room is `localpart@muc.domain`, so we
/// require both the `@` (a bare *domain-only* string like `c-abc123`
/// is a valid JID but is not a room) and a successful `BareJid` parse
/// (a 1:1 scoped id `<initiator-bare>::<sid>` has a `::`-bearing domain
/// part that fails to parse). 1:1 ids are still cleared from the SFU
/// registry by the reconciler; they just carry no MUC `<presence/>`.
fn is_muc_call(call_id: &str) -> bool {
    call_id.contains('@') && call_id.parse::<BareJid>().is_ok()
}

/// Spawn the periodic SFU ghost-reconciliation task. Drives
/// [`SfuReconciler::reconcile_active_calls`] on an interval and clears
/// the MUC Muji presence of any participant LiveKit no longer reports —
/// the safety net for a `participant_left`/`room_finished` webhook
/// whose delivery (and all retries) were lost. No-op until the first
/// tick after [`RECONCILE_INTERVAL`].
pub fn spawn_reconciliation_task(state: Arc<WebSocketState>, reconciler: Arc<dyn SfuReconciler>) {
    let grace = chrono::Duration::seconds(RECONCILE_GRACE_SECONDS);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(RECONCILE_INTERVAL);
        // Drop the immediate first tick `interval` yields so we don't
        // reconcile at boot before any call exists.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            reconcile_once(&state, reconciler.as_ref(), grace).await;
        }
    });
}

/// One reconciliation pass: sweep registry ghosts via the reconciler,
/// then clear each swept MUC participant's Muji presence through the
/// same idempotent path the `participant_left` webhook uses.
async fn reconcile_once(
    state: &WebSocketState,
    reconciler: &dyn SfuReconciler,
    grace: chrono::Duration,
) {
    let swept = reconciler.reconcile_active_calls(grace).await;
    if swept.is_empty() {
        return;
    }
    info!(
        count = swept.len(),
        "SFU reconciliation swept ghost participants; clearing MUC Muji presence"
    );
    for (call_id, identity) in swept {
        if is_muc_call(call_id.as_str()) {
            process_participant_left_for_identity(
                state,
                call_id.as_str(),
                identity.as_jid().to_string().as_str(),
            )
            .await;
        }
    }
}

async fn process_participant_left(state: &WebSocketState, env: &ParticipantEnvelope) {
    process_participant_left_for_identity(state, &env.room.name, &env.participant.identity).await;
}

/// Shared cleanup implementation used by both the
/// `ParticipantLeft` / `ParticipantConnectionAborted` arms and the
/// `RoomFinished` survivor sweep. Takes the raw identity string so
/// the survivor sweep (which iterates `Identity` values out of the
/// SFU registry) can call it with the same shape.
async fn process_participant_left_for_identity(
    state: &WebSocketState,
    room_name: &str,
    identity: &str,
) {
    let Ok(full_jid) = identity.parse::<FullJid>() else {
        warn!(
            identity = %identity,
            room = %room_name,
            "LiveKit participant identity is not a valid full JID; skipping cleanup",
        );
        return;
    };
    let Ok(room_jid) = room_name.parse::<BareJid>() else {
        warn!(
            room = %room_name,
            identity = %identity,
            "LiveKit room name is not a valid MUC bare JID; skipping cleanup",
        );
        return;
    };

    clear_muji_presence_for_departure(state, &room_jid, &full_jid).await;
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
    fn is_muc_call_distinguishes_room_jids_from_scoped_1to1_ids() {
        // MUC (Muji) call ids are bare room JIDs → have presence.
        assert!(is_muc_call("general@muc.waddle.social"));
        assert!(is_muc_call("team-standup@muc.waddle.social"));
        // 1:1 scoped ids (`<initiator-bare>::<sid>`) are not valid
        // JIDs (the `::` lands in the domain part) → no MUC presence.
        assert!(!is_muc_call("alice@waddle.social::c-abc123"));
        assert!(!is_muc_call("c-abc123"));
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
