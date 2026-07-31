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

use std::sync::Arc;
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
    verify_webhook_signature, CallCorrelationId, CallId, LiveKitWebhookEvent, ObservedCallSids,
    ParticipantEnvelope, ParticipantInfo, RoomInfo, SfuReconciler, SidObservationDisposition,
    TeardownDisposition, WebhookVerifyError, RECONCILE_GRACE_SECONDS,
};
use waddle_xmpp::telemetry::attributes::{MetricAttribute, WebhookEventType, WebhookOutcome};
use waddle_xmpp::telemetry::call::increment_call_teardown_stale_dropped;

use super::muc_muji_clear::{clear_muji_presence_for_departure, WebhookEffectOutcome};
use super::webhook_delivery::{
    DatabaseWebhookDeliveryStore, WebhookDeliveryObservation, WebhookDeliveryStore,
};
use super::websocket::{note_participant_left_by_call_id, WebSocketState};

#[derive(Clone)]
struct WebhookRouterState {
    deliveries: Arc<dyn WebhookDeliveryStore>,
}

/// Axum router for the LiveKit webhook endpoint. Mounted under
/// `/api/v1/livekit/webhook` by [`crate::server::http`].
pub fn router(websocket_state: Arc<WebSocketState>) -> Router {
    let deliveries = Arc::new(DatabaseWebhookDeliveryStore::new(
        websocket_state.deps.app_state.db_pool.global().clone(),
    ));
    Router::new()
        .route("/api/v1/livekit/webhook", post(livekit_webhook_handler))
        .layer(Extension(websocket_state))
        .with_state(WebhookRouterState { deliveries })
}

/// Which LiveKit payload kind a delivery carried, as the closed
/// metric attribute (#1452).
fn webhook_event_type(event: &LiveKitWebhookEvent) -> WebhookEventType {
    match event {
        LiveKitWebhookEvent::ParticipantJoined(_) => WebhookEventType::ParticipantJoined,
        LiveKitWebhookEvent::ParticipantLeft(_) => WebhookEventType::ParticipantLeft,
        LiveKitWebhookEvent::ParticipantConnectionAborted(_) => {
            WebhookEventType::ParticipantConnectionAborted
        }
        LiveKitWebhookEvent::RoomFinished(_) => WebhookEventType::RoomFinished,
        LiveKitWebhookEvent::Other => WebhookEventType::Other,
    }
}

/// The LiveKit room name a typed delivery names, or `None` for a
/// payload kind Waddle does not model. This is the join key for
/// [`CallCorrelationId`] — the client and the server call-setup path
/// derive the same value from the same room name (#1452).
fn webhook_room_name(event: &LiveKitWebhookEvent) -> Option<&str> {
    match event {
        LiveKitWebhookEvent::ParticipantJoined(env)
        | LiveKitWebhookEvent::ParticipantLeft(env)
        | LiveKitWebhookEvent::ParticipantConnectionAborted(env) => Some(env.room.name.as_str()),
        LiveKitWebhookEvent::RoomFinished(env) => Some(env.room.name.as_str()),
        LiveKitWebhookEvent::Other => None,
    }
}

async fn livekit_webhook_handler(
    Extension(state): Extension<Arc<WebSocketState>>,
    State(router_state): State<WebhookRouterState>,
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
            record_webhook_outcome(WebhookEventType::Unknown, WebhookOutcome::SignatureFailed);
            return StatusCode::UNAUTHORIZED;
        }
        Err(WebhookVerifyError::Jwt(error)) => {
            record_webhook_outcome(WebhookEventType::Unknown, WebhookOutcome::SignatureFailed);
            warn!(error = ?error, "LiveKit webhook JWT validation failed");
            return StatusCode::UNAUTHORIZED;
        }
        Err(WebhookVerifyError::MissingBodyHash) | Err(WebhookVerifyError::BodyHashMismatch) => {
            record_webhook_outcome(WebhookEventType::Unknown, WebhookOutcome::SignatureFailed);
            return StatusCode::UNAUTHORIZED;
        }
        Err(WebhookVerifyError::BodyJson(error)) => {
            record_webhook_outcome(WebhookEventType::Unknown, WebhookOutcome::DecodeFailed);
            warn!(error = ?error, "LiveKit webhook body JSON parse failed");
            return StatusCode::BAD_REQUEST;
        }
    };

    let event_type = webhook_event_type(&event);
    let correlation = webhook_room_name(&event).map(CallCorrelationId::for_room_name);
    let call_id_field = correlation
        .as_ref()
        .map(CallCorrelationId::as_str)
        .unwrap_or("unknown");

    if let Some(event_id) = event.event_id() {
        match router_state.deliveries.observe(event_id).await {
            Ok(WebhookDeliveryObservation::Done) => {
                record_webhook_outcome(event_type, WebhookOutcome::Duplicate);
                debug!(
                    event_id,
                    event = %event_type.value(),
                    call.id = %call_id_field,
                    "LiveKit webhook duplicate; dropping",
                );
                return StatusCode::OK;
            }
            Ok(WebhookDeliveryObservation::Processing) => {}
            Err(error) => {
                warn!(
                    event_id,
                    event = %event_type.value(),
                    error = %error,
                    "failed to record LiveKit webhook delivery; asking LiveKit to retry"
                );
                record_webhook_outcome(event_type, WebhookOutcome::RetryableFailure);
                return StatusCode::SERVICE_UNAVAILABLE;
            }
        }
    }
    record_webhook_outcome(event_type, WebhookOutcome::Received);
    // One disposition line per accepted delivery (#1452): what LiveKit
    // sent, which call it belongs to, and that we took it. Paired with
    // the `waddle.call.webhook.events` counter so a silent ingestion
    // stall is visible in both logs and metrics.
    info!(
        event = %event_type.value(),
        event_id = ?event.event_id(),
        call.id = %call_id_field,
        outcome = %WebhookOutcome::Received.value(),
        "LiveKit webhook accepted",
    );

    let effect_outcome = match &event {
        LiveKitWebhookEvent::ParticipantLeft(env)
        | LiveKitWebhookEvent::ParticipantConnectionAborted(env) => {
            process_participant_left(&state, env).await
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
            match CallId::new(env.room.name.clone()) {
                Ok(call_id) => {
                    let observed_sids = observed_sids_for_room(&env.room);
                    let survivors = sfu.participants_for_call(&call_id);
                    let mut outcome = WebhookEffectOutcome::Completed;
                    for identity in survivors {
                        let survivor_outcome = process_participant_left_for_identity(
                            &state,
                            &env.room.name,
                            identity.as_jid().to_string().as_str(),
                            Some(&observed_sids),
                        )
                        .await;
                        if matches!(survivor_outcome, WebhookEffectOutcome::Retryable(_)) {
                            outcome = survivor_outcome;
                            break;
                        }
                        if matches!(survivor_outcome, WebhookEffectOutcome::Permanent(_)) {
                            outcome = survivor_outcome;
                        }
                    }
                    outcome
                }
                Err(error) => {
                    warn!(
                        room = %env.room.name,
                        %error,
                        "LiveKit room_finished room name is not a valid call id; cannot sweep survivors",
                    );
                    WebhookEffectOutcome::Permanent("invalid_call_id")
                }
            }
        }
        LiveKitWebhookEvent::ParticipantJoined(env) => {
            // Normally the join already flowed through Jingle
            // session-initiate → `register_call_participant`, but after
            // a process restart this webhook is also the authoritative
            // recovery signal that restores local registry membership.
            // It is the ONLY moment
            // we learn that a specific token was actually redeemed —
            // and therefore the only place a stale token can be
            // caught.
            //
            // Self-hosted LiveKit derives permissions from the JWT at
            // join and does not invalidate previously-minted tokens
            // when permissions change (that is a LiveKit Cloud
            // feature), and our own JTI revocation is local
            // bookkeeping LiveKit never consults. So an occupant who
            // was de-voiced between mint and connect — or who
            // reconnects with a cached pre-demotion token — would
            // otherwise publish for the remainder of the token's TTL.
            // Re-deriving their current voice here and pushing it
            // converges them within one webhook round-trip.
            let observed_sids = observed_sids_for_participant(&env.room, &env.participant);
            if register_participant_from_join(&state, env, &observed_sids)
                == SidObservationDisposition::StaleSid
            {
                WebhookEffectOutcome::Completed
            } else if reassert_voice_grants_on_join(
                &state,
                &env.room.name,
                &env.participant.identity,
            )
            .await
                == ReassertOutcome::RetryableFailure
            {
                warn!(
                    room = %env.room.name,
                    call.id = %call_id_field,
                    "asking LiveKit to retry participant_joined after a transient failure",
                );
                WebhookEffectOutcome::Retryable("voice_grant_reassert_failed")
            } else {
                WebhookEffectOutcome::Completed
            }
        }
        LiveKitWebhookEvent::Other => WebhookEffectOutcome::Completed,
    };

    match effect_outcome {
        WebhookEffectOutcome::Retryable(reason) => {
            warn!(
                event_id = ?event.event_id(),
                event = %event_type.value(),
                call.id = %call_id_field,
                reason,
                "LiveKit webhook effects were not completed; asking LiveKit to retry"
            );
            record_webhook_outcome(event_type, WebhookOutcome::RetryableFailure);
            StatusCode::SERVICE_UNAVAILABLE
        }
        WebhookEffectOutcome::Completed | WebhookEffectOutcome::Permanent(_) => {
            if let WebhookEffectOutcome::Permanent(reason) = effect_outcome {
                warn!(
                    event_id = ?event.event_id(),
                    event = %event_type.value(),
                    call.id = %call_id_field,
                    reason,
                    "LiveKit webhook had a permanent effect failure; acknowledging delivery"
                );
                record_webhook_outcome(event_type, WebhookOutcome::PermanentFailure);
            }
            if let Some(event_id) = event.event_id() {
                if let Err(error) = router_state.deliveries.complete(event_id).await {
                    warn!(
                        event_id,
                        event = %event_type.value(),
                        error = %error,
                        "failed to complete LiveKit webhook delivery; asking LiveKit to retry"
                    );
                    record_webhook_outcome(event_type, WebhookOutcome::RetryableFailure);
                    return StatusCode::SERVICE_UNAVAILABLE;
                }
            }
            StatusCode::OK
        }
    }
}

fn record_webhook_outcome(event: WebhookEventType, outcome: WebhookOutcome) {
    add_webhook_event(1, event, outcome);
}

fn add_webhook_event(count: u64, event: WebhookEventType, outcome: WebhookOutcome) {
    waddle_xmpp::counter_add!(
        "waddle.call.webhook.events",
        "1",
        "LiveKit webhook ingestion events by event type and outcome.",
        count,
        event,
        outcome,
    );
}

/// `add(0)` the reachable (event, outcome) webhook series so a fresh
/// pod exports them before the first delivery of a deploy (#1436):
/// `LiveKitWebhookRejections` reads the failure outcomes via
/// `increase()` and would otherwise evaluate to no-data. Only the
/// combinations the handler can actually emit are registered — a
/// delivery that fails verification is never typed, and a typed event
/// is never a verification failure.
pub(crate) fn register_webhook_counters() {
    for outcome in [
        WebhookOutcome::SignatureFailed,
        WebhookOutcome::DecodeFailed,
    ] {
        add_webhook_event(0, WebhookEventType::Unknown, outcome);
    }
    for event in [
        WebhookEventType::ParticipantJoined,
        WebhookEventType::ParticipantLeft,
        WebhookEventType::ParticipantConnectionAborted,
        WebhookEventType::RoomFinished,
        WebhookEventType::Other,
    ] {
        for outcome in [
            WebhookOutcome::Received,
            WebhookOutcome::Duplicate,
            WebhookOutcome::RetryableFailure,
            WebhookOutcome::PermanentFailure,
        ] {
            add_webhook_event(0, event, outcome);
        }
    }
}

fn observed_sids_for_participant(
    room: &RoomInfo,
    participant: &ParticipantInfo,
) -> ObservedCallSids {
    ObservedCallSids::new(room.sid.clone(), participant.sid.clone())
}

fn observed_sids_for_room(room: &RoomInfo) -> ObservedCallSids {
    ObservedCallSids::new(room.sid.clone(), None)
}

fn register_participant_from_join(
    state: &WebSocketState,
    env: &ParticipantEnvelope,
    observed_sids: &ObservedCallSids,
) -> SidObservationDisposition {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return SidObservationDisposition::Applied;
    };
    let Ok(call_id) = CallId::new(env.room.name.clone()) else {
        return SidObservationDisposition::Applied;
    };
    let Ok(full_jid) = env.participant.identity.parse::<FullJid>() else {
        return SidObservationDisposition::Applied;
    };
    let disposition = sfu.register_call_participant_observed(
        &call_id,
        &waddle_sfu::Identity::from_jid(full_jid),
        observed_sids,
    );
    if disposition == SidObservationDisposition::StaleSid {
        increment_call_teardown_stale_dropped();
    }
    disposition
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
        // Owned by the loop so the non-occupant confirmation streaks
        // survive across ticks.
        let mut non_occupant_streaks = super::sfu_voice_reconcile::NonOccupantStreaks::default();
        let mut ticker = tokio::time::interval(RECONCILE_INTERVAL);
        // Drop the immediate first tick `interval` yields so we don't
        // reconcile at boot before any call exists.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            reconcile_once(
                &state,
                reconciler.as_ref(),
                grace,
                &mut non_occupant_streaks,
            )
            .await;
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
    non_occupant_streaks: &mut super::sfu_voice_reconcile::NonOccupantStreaks,
) {
    let started = std::time::Instant::now();
    let summary = reconciler.reconcile_active_calls(grace).await;
    waddle_xmpp::telemetry::call::record_reconcile_pass_duration(started.elapsed().as_secs_f64());
    waddle_xmpp::telemetry::call::add_reconcile_rooms_examined(summary.rooms_examined);
    waddle_xmpp::telemetry::call::add_reconcile_rooms_adopted(summary.rooms_adopted);
    waddle_xmpp::telemetry::call::add_reconcile_rooms_swept(summary.rooms_swept);
    waddle_xmpp::telemetry::call::add_reconcile_occupancy_failures(summary.occupancy_failures);
    if !summary.swept.is_empty() {
        info!(
            count = summary.swept.len(),
            "SFU reconciliation swept ghost participants; clearing MUC Muji presence"
        );
        for (call_id, identity) in summary.swept {
            if is_muc_call(call_id.as_str()) {
                process_participant_left_for_identity(
                    state,
                    call_id.as_str(),
                    identity.as_jid().to_string().as_str(),
                    None,
                )
                .await;
            }
        }
    }
    super::sfu_voice_reconcile::reconcile_voice_grants(state, reconciler, non_occupant_streaks)
        .await;
}

async fn process_participant_left(
    state: &WebSocketState,
    env: &ParticipantEnvelope,
) -> WebhookEffectOutcome {
    let observed_sids = observed_sids_for_participant(&env.room, &env.participant);
    process_participant_left_for_identity(
        state,
        &env.room.name,
        &env.participant.identity,
        Some(&observed_sids),
    )
    .await
}

/// Shared cleanup implementation used by both the
/// `ParticipantLeft` / `ParticipantConnectionAborted` arms and the
/// `RoomFinished` survivor sweep. Takes the raw identity string so
/// the survivor sweep (which iterates `Identity` values out of the
/// SFU registry) can call it with the same shape.
/// Whether a `participant_joined` delivery was fully handled, or could
/// not be enforced and should be retried by LiveKit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReassertOutcome {
    /// Grants were converged, or no MUC authorization applies to this
    /// participant. Nothing a retry would improve.
    Handled,
    /// This node structurally cannot resolve the room — its actor is
    /// claimed by another replica. Retrying would 5xx roughly half of
    /// all joins in a two-replica cluster for a condition that is not a
    /// failure, so the delivery is acknowledged and convergence is left
    /// to [`reconcile_voice_grants`] on the owning node.
    UnenforceableHere,
    /// A transient failure (the local actor flaked) left grants
    /// possibly stale. The delivery is NOT acknowledged so LiveKit's
    /// retry can repair it — without this the up-front dedupe record
    /// would make the failure permanent.
    RetryableFailure,
}

/// Re-derive a just-joined participant's XEP-0045 voice and push it to
/// the SFU, so the permissions LiveKit granted from their token match
/// the room's current authorization state.
///
/// This is the enforcement point for stale tokens: LiveKit reads
/// grants off the JWT at join and self-hosted deployments never
/// invalidate an already-minted token when permissions change, so
/// without this a token minted before a demotion still admits its
/// holder with publish rights until it expires.
///
/// Only MUC (Muji) rooms participate — a 1:1 call's room name is a
/// scoped call id, not a room JID, and 1:1 peers have no role model.
///
/// A participant LiveKit reports as joined who is NOT a current
/// occupant is evicted rather than downgraded: they hold a token for a
/// room they no longer belong to (a voluntary leave followed by a
/// LiveKit reconnect reaches no other teardown path, because the
/// registry-based sweeps only chase participants LiveKit has already
/// dropped).
///
/// Every branch that cannot establish the caller's voice logs at WARN:
/// this is the only enforcement point against stale tokens, so a
/// silent skip would be indistinguishable from success. Notably the
/// room actor may be claimed by another cluster node, in which case
/// this node cannot resolve the role at all.
async fn reassert_voice_grants_on_join(
    state: &WebSocketState,
    room_name: &str,
    identity: &str,
) -> ReassertOutcome {
    let Ok(full_jid) = identity.parse::<FullJid>() else {
        // Permanent: retrying cannot make this parse.
        warn!(
            room = %room_name,
            "LiveKit participant identity is not a valid full JID; cannot re-assert media grants",
        );
        return ReassertOutcome::Handled;
    };
    // Use the shared classifier: a domain-only id like `c-abc123`
    // parses as a valid `BareJid` without being a room.
    if !is_muc_call(room_name) {
        // 1:1 scoped call id — no MUC roles apply.
        return ReassertOutcome::Handled;
    }
    let Ok(room_jid) = room_name.parse::<BareJid>() else {
        return ReassertOutcome::Handled;
    };
    use crate::server::routes::websocket::muc_call_sfu::{
        enforce_current_voice_grants, GrantEnforcement,
    };
    match enforce_current_voice_grants(state, &room_jid, &full_jid).await {
        GrantEnforcement::Applied
        | GrantEnforcement::EvictedNonOccupant
        | GrantEnforcement::SfuNotConfigured => ReassertOutcome::Handled,
        GrantEnforcement::NoLocalRoomActor => {
            // `GetRoom` only consults THIS process's room map, so a
            // room claimed by another cluster node also answers
            // `Ok(None)`. Webhooks arrive through one load-balanced
            // URL across replicas, so this is the common case, NOT
            // evidence that the participant is unauthorized —
            // evicting here would eject legitimate occupants from
            // roughly every join that lands on a non-owning node.
            // Absence of a local actor means "cannot determine"
            // locally; #1594 routes the re-assert to the room's claim
            // owner instead.
            super::livekit_grant_relay::reassert_on_claim_owner(state, &room_jid, &full_jid).await
        }
        GrantEnforcement::LookupFailed => {
            // The lookup failed transiently, so this node could not
            // resolve the occupant's role. Deliberately NOT an
            // eviction — the participant is probably legitimate — but
            // a stale token goes unenforced on this path, so LiveKit
            // is asked to retry. The shared core already logged the
            // failure WARN with the underlying error; a second line
            // here would double-count the event in log-based alerting.
            ReassertOutcome::RetryableFailure
        }
    }
}

async fn process_participant_left_for_identity(
    state: &WebSocketState,
    room_name: &str,
    identity: &str,
    observed_sids: Option<&ObservedCallSids>,
) -> WebhookEffectOutcome {
    let full_jid = match identity.parse::<FullJid>() {
        Ok(full_jid) => full_jid,
        Err(error) => {
            warn!(
                identity = %identity,
                room = %room_name,
                %error,
                "LiveKit participant identity is not a valid full JID; skipping cleanup",
            );
            return WebhookEffectOutcome::Permanent("invalid_participant_jid");
        }
    };
    if let Ok(room_jid) = room_name.parse::<BareJid>() {
        return clear_muji_presence_for_departure(state, &room_jid, &full_jid, observed_sids).await;
    }

    let call_id = match CallId::new(room_name.to_owned()) {
        Ok(call_id) => call_id,
        Err(error) => {
            warn!(
                room = %room_name,
                identity = %identity,
                %error,
                "LiveKit room name is neither a MUC bare JID nor a valid raw call id; skipping cleanup",
            );
            return WebhookEffectOutcome::Permanent("invalid_call_id");
        }
    };

    debug!(
        room = %room_name,
        identity = %identity,
        "LiveKit room name is not a MUC bare JID; clearing SFU registry by raw call id",
    );
    if matches!(
        note_participant_left_by_call_id(state, &call_id, &full_jid, observed_sids),
        Some(TeardownDisposition::StaleSid)
    ) {
        increment_call_teardown_stale_dropped();
    }
    WebhookEffectOutcome::Completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::handlers::iq::handle_iq;
    use crate::server::routes::websocket::tests::{
        create_test_server_owner_session, create_test_websocket_state_with_calls,
        create_test_websocket_state_with_sfu, snapshot_room, RecordingSfu,
    };
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::io;
    use std::sync::Mutex;
    use waddle_sfu::{
        Identity, MediaCapabilities, ObservedCallSids, ParticipantSid, RoomSid, TeardownDisposition,
    };
    use waddle_xmpp::protocol::ConnectionPhase;

    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    struct FixedSummaryReconciler {
        summary: Mutex<Option<waddle_sfu::ReconcilePassSummary>>,
    }

    impl waddle_sfu::SfuReconciler for FixedSummaryReconciler {
        fn reconcile_active_calls(&self, _: chrono::Duration) -> waddle_sfu::ReconcileFuture<'_> {
            Box::pin(async move {
                self.summary
                    .lock()
                    .expect("summary lock")
                    .take()
                    .unwrap_or_default()
            })
        }

        fn live_participants<'a>(
            &'a self,
            _: &'a CallId,
        ) -> waddle_sfu::LiveParticipantsFuture<'a> {
            Box::pin(async { Some(Vec::new()) })
        }
    }

    fn signed_webhook_headers(secret: &[u8], body: &[u8]) -> HeaderMap {
        let mut hasher = Sha256::new();
        hasher.update(body);
        let claims = json!({
            "sha256": BASE64_STANDARD.encode(hasher.finalize()),
            "exp": (chrono::Utc::now() + chrono::Duration::seconds(60)).timestamp(),
            "iat": chrono::Utc::now().timestamp(),
        });
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret),
        )
        .expect("sign test webhook");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .expect("valid auth header"),
        );
        headers
    }

    fn test_router_state() -> WebhookRouterState {
        WebhookRouterState {
            deliveries: Arc::new(
                super::super::webhook_delivery::InMemoryWebhookDeliveryStore::default(),
            ),
        }
    }

    fn test_router_state_with(deliveries: Arc<dyn WebhookDeliveryStore>) -> WebhookRouterState {
        WebhookRouterState { deliveries }
    }

    /// #1436 acceptance for the webhook family: a pod that has received
    /// no deliveries still exports every reachable (event, outcome)
    /// series at 0, so `LiveKitWebhookRejections` reads 0, not no-data.
    #[tokio::test(flavor = "current_thread")]
    async fn registration_exports_reachable_webhook_series_at_zero() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        register_webhook_counters();

        for (event, outcome) in [
            ("unknown", "signature_failed"),
            ("unknown", "decode_failed"),
            ("participant_joined", "received"),
            ("participant_left", "received"),
            ("participant_connection_aborted", "received"),
            ("room_finished", "received"),
            ("other", "received"),
            ("room_finished", "duplicate"),
            ("participant_left", "retryable_failure"),
            ("participant_left", "permanent_failure"),
        ] {
            assert_eq!(
                metrics.counter_sum(
                    "waddle.call.webhook.events",
                    &[("event", event), ("outcome", outcome)]
                ),
                Some(0),
                "webhook series ({event}, {outcome}) missing from startup zero-registration"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reconcile_pass_summary_emits_duration_and_room_counters() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state_with_sfu(Arc::new(RecordingSfu::default())).await;
        let reconciler = FixedSummaryReconciler {
            summary: Mutex::new(Some(waddle_sfu::ReconcilePassSummary {
                swept: Vec::new(),
                rooms_examined: 3,
                rooms_adopted: 2,
                rooms_swept: 1,
                occupancy_failures: 4,
            })),
        };
        let mut streaks = super::super::sfu_voice_reconcile::NonOccupantStreaks::default();

        reconcile_once(
            state.as_ref(),
            &reconciler,
            chrono::Duration::zero(),
            &mut streaks,
        )
        .await;

        assert_eq!(
            metrics.histogram_count("waddle.call.reconcile.pass_duration", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.reconcile.rooms_examined", &[]),
            Some(3)
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.reconcile.rooms_adopted", &[]),
            Some(2)
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.reconcile.rooms_swept", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.reconcile.occupancy_failures", &[]),
            Some(4)
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.reconcile.pass_duration"),
            Some("s".to_owned())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_webhook_emits_debug_log_and_counter() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let secret = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("call fixture has SFU")
            .webhook_secret()
            .as_bytes()
            .to_vec();
        let body = serde_json::to_vec(&json!({
            "event": "participant_left",
            "id": "EV_duplicate_metric",
            "room": { "name": "alice@example.com::duplicate-call" },
            "participant": { "identity": "alice@example.com/web" }
        }))
        .expect("serialize webhook body");
        let headers = signed_webhook_headers(&secret, &body);
        let router_state = test_router_state();

        let first = livekit_webhook_handler(
            Extension(state.clone()),
            State(router_state.clone()),
            headers.clone(),
            Bytes::from(body.clone()),
        )
        .await
        .into_response();
        let duplicate = livekit_webhook_handler(
            Extension(state),
            State(router_state),
            headers,
            Bytes::from(body),
        )
        .await
        .into_response();

        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(duplicate.status(), StatusCode::OK);
        assert_eq!(
            recorder.note_snapshot().len(),
            1,
            "a completed duplicate must not run participant-left effects twice"
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.webhook.events", &[("outcome", "received")]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.webhook.events", &[("outcome", "duplicate")]),
            Some(1)
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.webhook.events"),
            Some("1".to_string())
        );
        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        assert!(
            logs.contains("LiveKit webhook duplicate; dropping"),
            "{logs}"
        );
        assert!(logs.contains("\"level\":\"DEBUG\""), "{logs}");
        assert!(logs.contains("EV_duplicate_metric"), "{logs}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn room_finished_survivor_sweep_forwards_only_room_sid() {
        let recorder = Arc::new(RecordingSfu::default());
        let survivor_jid: FullJid = "bob@example.com/phone".parse().expect("survivor jid");
        recorder.set_participants(vec![Identity::from_jid(survivor_jid.clone())]);
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let secret = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("call fixture has SFU")
            .webhook_secret()
            .as_bytes()
            .to_vec();
        let body = serde_json::to_vec(&json!({
            "event": "room_finished",
            "id": "EV_room_finished_sid",
            "room": {
                "name": "alice@example.com::room-finished",
                "sid": "RM_finished"
            }
        }))
        .expect("serialize webhook body");
        let headers = signed_webhook_headers(&secret, &body);

        let response = livekit_webhook_handler(
            Extension(state),
            State(test_router_state()),
            headers,
            Bytes::from(body),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let notes = recorder.note_with_sids_snapshot();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].1.as_jid(), &survivor_jid);
        assert_eq!(
            notes[0].2.room_sid,
            Some(RoomSid::new("RM_finished").expect("room sid"))
        );
        assert_eq!(notes[0].2.participant_sid, None);
    }

    #[tokio::test]
    async fn processing_delivery_redelivery_runs_effects_and_completes() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let store =
            Arc::new(super::super::webhook_delivery::InMemoryWebhookDeliveryStore::default());
        assert_eq!(
            store
                .observe("EV_processing_redelivery")
                .await
                .expect("seed processing delivery"),
            WebhookDeliveryObservation::Processing
        );
        let router_state = test_router_state_with(store.clone());
        let body = serde_json::to_vec(&json!({
            "event": "participant_left",
            "id": "EV_processing_redelivery",
            "room": { "name": "alice@example.com::processing-call" },
            "participant": { "identity": "alice@example.com/web" }
        }))
        .expect("serialize webhook body");
        let secret = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("recording SFU")
            .webhook_secret()
            .as_bytes()
            .to_vec();
        let response = livekit_webhook_handler(
            Extension(state),
            State(router_state),
            signed_webhook_headers(&secret, &body),
            Bytes::from(body),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(recorder.note_snapshot().len(), 1);
        assert_eq!(
            store
                .status("EV_processing_redelivery")
                .expect("delivery status"),
            Some(WebhookDeliveryObservation::Done)
        );
        assert_eq!(
            store
                .attempt_count("EV_processing_redelivery")
                .expect("attempt count"),
            Some(2)
        );
    }

    #[tokio::test]
    async fn retryable_participant_left_stays_processing_then_completes_on_retry() {
        let first_recorder = Arc::new(RecordingSfu::default());
        let first_state = create_test_websocket_state_with_sfu(first_recorder).await;
        first_state.deps.protocol.room_registry.kill();
        first_state
            .deps
            .protocol
            .room_registry
            .wait_for_shutdown()
            .await;

        let store =
            Arc::new(super::super::webhook_delivery::InMemoryWebhookDeliveryStore::default());
        let router_state = test_router_state_with(store.clone());
        let body = serde_json::to_vec(&json!({
            "event": "participant_left",
            "id": "EV_retryable_participant_left",
            "room": { "name": "retryable@muc.example.com" },
            "participant": { "identity": "alice@example.com/web" }
        }))
        .expect("serialize webhook body");
        let first_secret = first_state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("recording SFU")
            .webhook_secret()
            .as_bytes()
            .to_vec();
        let first_response = livekit_webhook_handler(
            Extension(first_state),
            State(router_state.clone()),
            signed_webhook_headers(&first_secret, &body),
            Bytes::from(body.clone()),
        )
        .await
        .into_response();

        assert_eq!(first_response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            store
                .status("EV_retryable_participant_left")
                .expect("processing status"),
            Some(WebhookDeliveryObservation::Processing)
        );
        assert_eq!(
            store
                .attempt_count("EV_retryable_participant_left")
                .expect("first attempt count"),
            Some(1)
        );

        let retry_recorder = Arc::new(RecordingSfu::default());
        let retry_state = create_test_websocket_state_with_sfu(retry_recorder.clone()).await;
        let session = crate::server::routes::websocket::tests::create_test_server_owner_session(
            retry_state.as_ref(),
            "alice",
        )
        .await;
        let room_jid: BareJid = "retryable@muc.example.com".parse().expect("room JID");
        let alice: FullJid = "alice@example.com/web".parse().expect("full JID");
        crate::server::routes::websocket::handlers::presence::handle_muc_join(
            retry_state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            &Some(session),
        )
        .await;
        let retry_secret = retry_state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("recording SFU")
            .webhook_secret()
            .as_bytes()
            .to_vec();
        let retry_response = livekit_webhook_handler(
            Extension(retry_state),
            State(router_state),
            signed_webhook_headers(&retry_secret, &body),
            Bytes::from(body),
        )
        .await
        .into_response();

        assert_eq!(retry_response.status(), StatusCode::OK);
        assert_eq!(retry_recorder.note_snapshot().len(), 1);
        assert_eq!(
            store
                .status("EV_retryable_participant_left")
                .expect("done status"),
            Some(WebhookDeliveryObservation::Done)
        );
        assert_eq!(
            store
                .attempt_count("EV_retryable_participant_left")
                .expect("retry attempt count"),
            Some(2)
        );
    }

    #[tokio::test]
    async fn permanent_bad_participant_jid_is_acknowledged_and_completed() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let store =
            Arc::new(super::super::webhook_delivery::InMemoryWebhookDeliveryStore::default());
        let router_state = test_router_state_with(store.clone());
        let body = serde_json::to_vec(&json!({
            "event": "participant_left",
            "id": "EV_permanent_bad_jid",
            "room": { "name": "alice@example.com::bad-jid-call" },
            "participant": { "identity": "not a jid" }
        }))
        .expect("serialize webhook body");
        let secret = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("recording SFU")
            .webhook_secret()
            .as_bytes()
            .to_vec();
        let response = livekit_webhook_handler(
            Extension(state),
            State(router_state),
            signed_webhook_headers(&secret, &body),
            Bytes::from(body),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(recorder.note_snapshot().is_empty());
        assert_eq!(
            store
                .status("EV_permanent_bad_jid")
                .expect("delivery status"),
            Some(WebhookDeliveryObservation::Done)
        );
        assert_eq!(
            store
                .attempt_count("EV_permanent_bad_jid")
                .expect("attempt count"),
            Some(1)
        );
    }

    #[tokio::test]
    async fn signed_undecodable_webhook_records_decode_failure() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state_with_calls().await;
        let secret = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("call fixture has SFU")
            .webhook_secret()
            .as_bytes()
            .to_vec();
        let body = b"{not-json";
        let headers = signed_webhook_headers(&secret, body);

        let response = livekit_webhook_handler(
            Extension(state),
            State(test_router_state()),
            headers,
            Bytes::from_static(body),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.webhook.events",
                &[("outcome", "decode_failed")]
            ),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.webhook.events",
                &[("outcome", "signature_failed")]
            ),
            Some(0)
        );
    }

    /// #1452: every accepted delivery must be countable by *what*
    /// LiveKit sent, not just whether we took it, and must leave one
    /// disposition line carrying the shared call correlation id.
    #[tokio::test(flavor = "current_thread")]
    async fn accepted_webhook_counts_by_event_type_and_logs_one_disposition_line() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let state = create_test_websocket_state_with_calls().await;
        let secret = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("call fixture has SFU")
            .webhook_secret()
            .as_bytes()
            .to_vec();
        let body = serde_json::to_vec(&json!({
            "event": "participant_joined",
            "id": "EV_disposition",
            "room": { "name": "general@muc.example.com" },
            "participant": { "identity": "alice@example.com/web" }
        }))
        .expect("serialize webhook body");
        let headers = signed_webhook_headers(&secret, &body);

        let response = livekit_webhook_handler(
            Extension(state),
            State(test_router_state()),
            headers,
            Bytes::from(body),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.webhook.events",
                &[("event", "participant_joined"), ("outcome", "received")]
            ),
            Some(1)
        );
        // The event dimension is what makes "LiveKit stopped sending
        // participant_left" detectable; a bare outcome count cannot.
        assert_eq!(
            metrics
                .counter_sum(
                    "waddle.call.webhook.events",
                    &[("event", "participant_left")]
                )
                .unwrap_or(0),
            0
        );

        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        let correlation =
            waddle_sfu::CallCorrelationId::for_room_name("general@muc.example.com").to_string();
        assert!(logs.contains("LiveKit webhook accepted"), "{logs}");
        assert!(logs.contains("participant_joined"), "{logs}");
        assert!(logs.contains("EV_disposition"), "{logs}");
        assert!(logs.contains(&correlation), "{logs}");
    }

    /// A delivery that never decoded has no knowable event type, so it
    /// must land on the reserved `unknown` bucket rather than silently
    /// inheriting a neighbour's dimension.
    #[tokio::test(flavor = "current_thread")]
    async fn undecodable_webhook_counts_under_the_unknown_event_type() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state_with_calls().await;
        let secret = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("call fixture has SFU")
            .webhook_secret()
            .as_bytes()
            .to_vec();
        let body = b"{not-json";
        let headers = signed_webhook_headers(&secret, body);

        let response = livekit_webhook_handler(
            Extension(state),
            State(test_router_state()),
            headers,
            Bytes::from_static(body),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.webhook.events",
                &[("event", "unknown"), ("outcome", "decode_failed")]
            ),
            Some(1)
        );
    }

    #[test]
    fn webhook_event_type_maps_every_modelled_payload_kind() {
        let env = ParticipantEnvelope {
            id: None,
            room: waddle_sfu::RoomInfo {
                name: "r".into(),
                sid: None,
            },
            participant: waddle_sfu::ParticipantInfo {
                identity: "alice@example.com/web".into(),
                sid: None,
                state: None,
            },
        };
        assert_eq!(
            webhook_event_type(&LiveKitWebhookEvent::ParticipantJoined(env.clone())).value(),
            "participant_joined"
        );
        assert_eq!(
            webhook_event_type(&LiveKitWebhookEvent::ParticipantLeft(env.clone())).value(),
            "participant_left"
        );
        assert_eq!(
            webhook_event_type(&LiveKitWebhookEvent::ParticipantConnectionAborted(env)).value(),
            "participant_connection_aborted"
        );
        assert_eq!(
            webhook_event_type(&LiveKitWebhookEvent::Other).value(),
            "other"
        );
    }

    #[test]
    fn webhook_room_name_is_none_for_unmodelled_payloads() {
        assert_eq!(webhook_room_name(&LiveKitWebhookEvent::Other), None);
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

    /// Cross-node seam (#1594): the shared enforcement helper must
    /// distinguish "no actor in this process" from every authoritative
    /// answer, because the webhook forwards exactly that case to the
    /// room's claim owner. It must neither push grants nor evict on
    /// absence — absence is "cannot determine", not authorization
    /// evidence.
    #[tokio::test]
    async fn grant_enforcement_reports_missing_local_room_actor_without_side_effects() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room_jid: BareJid = "claimed-elsewhere@muc.example.com"
            .parse()
            .expect("room jid");
        let full_jid: FullJid = "alice@example.com/web".parse().expect("full jid");

        let outcome = crate::server::routes::websocket::muc_call_sfu::enforce_current_voice_grants(
            state.as_ref(),
            &room_jid,
            &full_jid,
        )
        .await;

        assert_eq!(
            outcome,
            crate::server::routes::websocket::muc_call_sfu::GrantEnforcement::NoLocalRoomActor
        );
        assert!(
            recorder.update_snapshot().is_empty(),
            "no grants may be pushed without an authoritative occupant answer"
        );
        assert!(
            recorder.snapshot().is_empty(),
            "an absent local actor must never evict"
        );
    }

    /// The owner-side executor for a relayed re-assert reuses this
    /// helper, so it must push the seated occupant's voice-derived
    /// grants exactly like the same-node webhook path.
    #[tokio::test]
    async fn grant_enforcement_pushes_grants_for_seated_occupant() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let session = crate::server::routes::websocket::tests::create_test_server_owner_session(
            state.as_ref(),
            "alice",
        )
        .await;
        let room_jid: BareJid = "grants-here@muc.example.com".parse().expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("full jid");
        crate::server::routes::websocket::handlers::presence::handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            &Some(session),
        )
        .await;

        let outcome = crate::server::routes::websocket::muc_call_sfu::enforce_current_voice_grants(
            state.as_ref(),
            &room_jid,
            &alice,
        )
        .await;

        assert_eq!(
            outcome,
            crate::server::routes::websocket::muc_call_sfu::GrantEnforcement::Applied
        );
        let updates = recorder.update_snapshot();
        assert_eq!(updates.len(), 1, "exactly one grant push expected");
        assert_eq!(updates[0].0.as_str(), room_jid.to_string());
        assert_eq!(updates[0].1.as_livekit_identity(), alice.to_string());
        assert!(
            !updates[0].2.is_listen_only(),
            "a seated occupant with voice must not be downgraded to listen-only"
        );
        assert!(
            recorder.snapshot().is_empty(),
            "occupants are never evicted"
        );
    }

    /// A LOCAL actor answering "not an occupant" is authoritative, so
    /// the shared helper evicts — same contract as the pre-#1594
    /// webhook path, now also reachable via the relay.
    #[tokio::test]
    async fn grant_enforcement_evicts_non_occupant_when_local_actor_is_authoritative() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let session = crate::server::routes::websocket::tests::create_test_server_owner_session(
            state.as_ref(),
            "alice",
        )
        .await;
        let room_jid: BareJid = "evictions@muc.example.com".parse().expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("full jid");
        crate::server::routes::websocket::handlers::presence::handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            &Some(session),
        )
        .await;
        let bob: FullJid = "bob@example.com/phone".parse().expect("full jid");

        let outcome = crate::server::routes::websocket::muc_call_sfu::enforce_current_voice_grants(
            state.as_ref(),
            &room_jid,
            &bob,
        )
        .await;

        assert_eq!(
            outcome,
            crate::server::routes::websocket::muc_call_sfu::GrantEnforcement::EvictedNonOccupant
        );
        assert!(
            recorder.update_snapshot().is_empty(),
            "a non-occupant gets evicted, never granted"
        );
        let evictions = recorder.snapshot();
        assert_eq!(evictions.len(), 1);
        assert_eq!(evictions[0].1.as_livekit_identity(), bob.to_string());
    }

    /// #1594 decision tests: what `reassert_voice_grants_on_join` does
    /// when this node has no room actor, by claim state. The foreign
    /// fresh-owner happy path needs a second live node and is covered
    /// by the clustering e2e suite.
    #[cfg(feature = "clustering")]
    mod cross_node {
        use super::*;
        use crate::clustering::ClusteringHandles;
        use crate::server::routes::websocket::tests::create_test_websocket_state_with_sfu_and_clustering;
        use tokio_util::sync::CancellationToken;
        use waddle_xmpp::ownership::{
            ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
        };

        fn handles_with(store: Arc<InProcessClaimStore>, me: NodeIdentity) -> ClusteringHandles {
            ClusteringHandles {
                claim_store: Some(store),
                node_identity: Some(SharedNodeIdentity::new(me)),
                stop_token: Some(CancellationToken::new()),
                ..ClusteringHandles::default()
            }
        }

        /// An unclaimed room has no authoritative occupant set anywhere,
        /// so there is no owner to ask: acknowledge and leave it to the
        /// reconciler, exactly the pre-#1594 behavior.
        #[tokio::test]
        async fn unclaimed_room_falls_back_to_reconcile() {
            let recorder = Arc::new(RecordingSfu::default());
            let me = NodeIdentity::new("node-self", "epoch-1");
            let state = create_test_websocket_state_with_sfu_and_clustering(
                recorder.clone(),
                handles_with(Arc::new(InProcessClaimStore::new()), me),
            )
            .await;

            let outcome = reassert_voice_grants_on_join(
                state.as_ref(),
                "unclaimed@muc.example.com",
                "alice@example.com/web",
            )
            .await;

            assert_eq!(outcome, ReassertOutcome::UnenforceableHere);
            assert!(recorder.update_snapshot().is_empty());
            assert!(
                recorder.snapshot().is_empty(),
                "must never evict on absence"
            );
        }

        /// A claim owned by THIS node with no local actor is a spawn or
        /// teardown race; the local reconciliation pass covers it, so
        /// the delivery is acknowledged rather than bounced to a relay
        /// ask against ourselves.
        #[tokio::test]
        async fn self_owned_claim_without_actor_falls_back_to_reconcile() {
            let recorder = Arc::new(RecordingSfu::default());
            let me = NodeIdentity::new("node-self", "epoch-1");
            let store = Arc::new(InProcessClaimStore::new());
            let room_jid: BareJid = "self-claimed@muc.example.com".parse().expect("room jid");
            store
                .acquire(
                    &Entity::new(EntityType::RoomActor, room_jid.to_string()),
                    &me,
                )
                .await
                .expect("acquire room claim");
            let state = create_test_websocket_state_with_sfu_and_clustering(
                recorder.clone(),
                handles_with(store, me),
            )
            .await;

            let outcome = reassert_voice_grants_on_join(
                state.as_ref(),
                "self-claimed@muc.example.com",
                "alice@example.com/web",
            )
            .await;

            assert_eq!(outcome, ReassertOutcome::UnenforceableHere);
            assert!(recorder.update_snapshot().is_empty());
            assert!(
                recorder.snapshot().is_empty(),
                "must never evict on absence"
            );
        }
    }

    #[tokio::test]
    async fn participant_left_scoped_1to1_call_id_cleans_sfu_registry() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let call_id = "alice@example.com::dm-1128";
        let departed = "bob@example.com/phone";

        process_participant_left_for_identity(state.as_ref(), call_id, departed, None).await;

        let notes = recorder.note_snapshot();
        assert_eq!(
            notes.len(),
            1,
            "participant-left must use local-only SFU cleanup"
        );
        assert_eq!(notes[0].0.as_str(), call_id);
        assert_eq!(notes[0].1.as_livekit_identity(), departed);
        assert!(
            recorder.snapshot().is_empty(),
            "participant-left webhook must not call the admin-evict unregister path"
        );
    }

    #[tokio::test]
    async fn participant_left_scoped_1to1_threads_observed_sids_to_sfu() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let call_id = "alice@example.com::dm-observed";
        let departed = "bob@example.com/phone";
        let observed_sids = ObservedCallSids {
            room_sid: Some(RoomSid::new("RM_observed").expect("room sid")),
            participant_sid: Some(ParticipantSid::new("PA_observed").expect("participant sid")),
        };

        process_participant_left_for_identity(
            state.as_ref(),
            call_id,
            departed,
            Some(&observed_sids),
        )
        .await;

        let notes = recorder.note_with_sids_snapshot();
        assert_eq!(notes.len(), 1, "participant-left must note exactly once");
        assert_eq!(notes[0].0.as_str(), call_id);
        assert_eq!(notes[0].1.as_livekit_identity(), departed);
        assert_eq!(notes[0].2, observed_sids);
        assert!(
            recorder.snapshot().is_empty(),
            "participant-left webhook must not call the admin-evict unregister path"
        );
    }

    #[tokio::test]
    async fn participant_joined_reregisters_unknown_participant_with_observed_sids() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let env = ParticipantEnvelope {
            id: Some("EV_joined_observed".to_owned()),
            room: RoomInfo {
                name: "alice@example.com::dm-observed".to_owned(),
                sid: Some(RoomSid::new("RM_joined").expect("room sid")),
            },
            participant: ParticipantInfo {
                identity: "bob@example.com/phone".to_owned(),
                sid: Some(ParticipantSid::new("PA_joined").expect("participant sid")),
                state: None,
            },
        };
        let observed_sids = observed_sids_for_participant(&env.room, &env.participant);

        assert_eq!(
            register_participant_from_join(state.as_ref(), &env, &observed_sids),
            SidObservationDisposition::Applied
        );

        let observations = recorder.registered_with_sids_snapshot();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].0.as_str(), env.room.name);
        assert_eq!(
            observations[0].1.as_livekit_identity(),
            env.participant.identity
        );
        assert_eq!(observations[0].2, observed_sids);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn participant_joined_conflicting_room_sid_is_stale_noop_and_counted() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let recorder = Arc::new(RecordingSfu::default());
        recorder.set_note_disposition(TeardownDisposition::StaleSid);
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let env = ParticipantEnvelope {
            id: Some("EV_joined_stale".to_owned()),
            room: RoomInfo {
                name: "alice@example.com::dm-stale".to_owned(),
                sid: Some(RoomSid::new("RM_old").expect("room sid")),
            },
            participant: ParticipantInfo {
                identity: "bob@example.com/phone".to_owned(),
                sid: Some(ParticipantSid::new("PA_old").expect("participant sid")),
                state: None,
            },
        };
        let observed_sids = observed_sids_for_participant(&env.room, &env.participant);

        assert_eq!(
            register_participant_from_join(state.as_ref(), &env, &observed_sids),
            SidObservationDisposition::StaleSid
        );
        assert!(
            recorder.registered_with_sids_snapshot().is_empty(),
            "a stale join must not recreate registry membership"
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.teardown.stale_dropped", &[]),
            Some(1)
        );
    }

    #[tokio::test]
    async fn stale_sid_drop_skips_muc_clear_and_counts_metric() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let recorder = Arc::new(RecordingSfu::default());
        recorder.set_note_disposition(TeardownDisposition::StaleSid);
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "stale-sid@muc.example.com".parse().expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("full jid");
        crate::server::routes::websocket::handlers::presence::handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            &Some(session),
        )
        .await;
        let observed_sids = ObservedCallSids {
            room_sid: Some(RoomSid::new("RM_stale").expect("room sid")),
            participant_sid: Some(ParticipantSid::new("PA_stale").expect("participant sid")),
        };
        let room_name = room_jid.to_string();

        let outcome = process_participant_left_for_identity(
            state.as_ref(),
            &room_name,
            alice.as_str(),
            Some(&observed_sids),
        )
        .await;

        assert_eq!(outcome, WebhookEffectOutcome::Completed);
        let room = snapshot_room(state.as_ref(), &room_jid).await.room;
        assert_eq!(
            room.find_nick_by_real_jid(&alice),
            Some("alice"),
            "stale SID must not clear the current occupant from the room actor"
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.teardown.stale_dropped", &[] as &[(&str, &str)]),
            Some(1)
        );
        let observed = recorder.observed_with_sids_snapshot();
        assert_eq!(
            observed.len(),
            1,
            "stale guard still inspects the webhook once"
        );
        assert_eq!(observed[0].2, observed_sids);
        assert!(
            recorder.note_with_sids_snapshot().is_empty(),
            "stale preflight must not destructively note the participant"
        );
        assert!(
            recorder.snapshot().is_empty(),
            "webhook stale drop must not trigger the admin-evict path"
        );
    }

    #[tokio::test]
    async fn participant_left_scoped_1to1_revokes_jti_and_survivor_terminate_succeeds() {
        let state = create_test_websocket_state_with_calls().await;
        let sfu = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("call-enabled fixture has SFU")
            .clone();
        let alice: FullJid = "alice@example.com/web".parse().expect("alice full jid");
        let bob: FullJid = "bob@example.com/phone".parse().expect("bob full jid");
        let call_id = CallId::new("alice@example.com::dm-1128").expect("scoped call id");
        let alice_identity = Identity::from_jid(alice.clone());
        let bob_identity = Identity::from_jid(bob.clone());
        let bob_token = sfu
            .issue_join_token(
                &call_id,
                &bob_identity,
                MediaCapabilities::direct_call_peer(),
            )
            .expect("bob token");
        sfu.register_call_participant(&call_id, &alice_identity);
        sfu.register_call_participant(&call_id, &bob_identity);

        process_participant_left_for_identity(state.as_ref(), call_id.as_str(), bob.as_str(), None)
            .await;

        assert!(
            !sfu.has_call_participant(&call_id, &bob_identity),
            "participant-left must remove the crashed peer from the 1:1 call registry"
        );
        assert!(
            sfu.is_revoked(&bob_token.jti),
            "participant-left must revoke the crashed peer's issued join token"
        );
        assert!(
            sfu.has_call_participant(&call_id, &alice_identity),
            "survivor must remain registered until their own terminate"
        );

        let terminate = r#"<iq xmlns='jabber:client' id='term-1128' type='set' to='bob@example.com/phone'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate' sid='dm-1128'>
                <reason><success/></reason>
            </jingle>
        </iq>"#;
        let responses = handle_iq(
            terminate,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ConnectionPhase::ready(alice.clone(), false),
        )
        .await;

        assert_eq!(
            responses.len(),
            1,
            "survivor terminate should be acked: {responses:?}"
        );
        let reply = responses[0].as_str();
        assert!(
            reply.contains(r#"type='result'"#) && reply.contains(r#"id='term-1128'"#),
            "survivor terminate should return an empty IQ result, got: {reply}"
        );
        assert!(
            !sfu.has_call_participant(&call_id, &alice_identity),
            "survivor terminate must unregister the remaining participant"
        );
    }
}
