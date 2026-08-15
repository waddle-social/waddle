//! Leased delivery of durable room mutation effects.
//!
//! Lifecycle rule: an effect may execute only while its exact lifecycle is
//! the room's live lifecycle; TERMINAL effects alone may additionally execute
//! while that exact lifecycle is tombstoned.
//! The latter preserves the wipe-first/destroy-presence contract; a recreated
//! room cannot commit while its predecessor's terminal effect exists.

use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jid::FullJid;
use kameo::actor::ActorRef;
use tokio::sync::oneshot;
use waddle_xmpp::muc::room_registry_actor::{LocalRoomJids, RoomRegistryActor};
use waddle_xmpp::muc::RoomEffectReservation;
use waddle_xmpp::ownership::{Entity, EntityType};
use waddle_xmpp::registry::{OutboundWriteAcceptance, SendResult};
use waddle_xmpp::Stanza;

use super::render::{effect_removed_sessions, effect_voice_changes, rebuild_effect};
use super::{
    ClaimedRoomEffect, RoomEffectKey, RoomEffectLastError, RoomEffectLeaseToken,
    RoomEffectOutboxError,
};
use crate::server::routes::websocket::handlers::presence::{
    registered_remote_resource_delivery, RegisteredRemoteDelivery,
};
use crate::server::routes::websocket::WebSocketState;

const OWNERSHIP_LOOKUP_TIMEOUT: Duration = waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT;
const LOCAL_ACCEPTANCE_TIMEOUT: Duration = Duration::from_secs(5);
const OWNERSHIP_RETRY_DELAY_MS: i64 = 15_000;
const OWNERSHIP_DEAD_LETTER_MS: i64 = 24 * 60 * 60 * 1_000;

enum ClaimPresence {
    Claimed,
    Unclaimed,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoomEffectDrainSummary {
    pub drained: u64,
    pub requeued: u64,
    pub stale: u64,
    pub dead_lettered: u64,
}

/// Completion retained for initiator-stream frames.  The handler must call
/// [`complete_after_write`] only after its response batch enters the
/// connection writer; keeping this token leased across the response-vector
/// handoff is intentional at-least-once behavior.
pub struct RoomEffectCompletion {
    pub key: RoomEffectKey,
    pub lease: RoomEffectLeaseToken,
    pending_local_acceptances: Arc<Mutex<Option<Vec<oneshot::Receiver<()>>>>>,
}

impl std::fmt::Debug for RoomEffectCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RoomEffectCompletion")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Clone for RoomEffectCompletion {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            lease: self.lease.clone(),
            pending_local_acceptances: Arc::clone(&self.pending_local_acceptances),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InlineRoomEffectFrame {
    pub stanza: Stanza,
    pub completion: RoomEffectCompletion,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InlineRoomEffectDrainSummary {
    pub inline: u64,
    pub completed: u64,
    pub requeued: u64,
    pub stale: u64,
    pub dead_lettered: u64,
    pub blocked: u64,
    pub leased: u64,
}

#[derive(Debug, Default, Clone)]
pub struct InlineRoomEffectDrain {
    pub frames: Vec<InlineRoomEffectFrame>,
    pub summary: InlineRoomEffectDrainSummary,
}

impl Deref for InlineRoomEffectDrain {
    type Target = Vec<InlineRoomEffectFrame>;

    fn deref(&self) -> &Self::Target {
        &self.frames
    }
}

impl DerefMut for InlineRoomEffectDrain {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.frames
    }
}

pub async fn complete_after_write(
    state: &WebSocketState,
    completion: &RoomEffectCompletion,
) -> Result<bool, RoomEffectOutboxError> {
    let pending = completion
        .pending_local_acceptances
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .unwrap_or_default();
    if !await_acks(pending).await {
        return Ok(false);
    }
    state
        .deps
        .protocol
        .room_effect_outbox
        .complete(&completion.key, &completion.lease)
        .await
}

pub async fn drain_due_effects(
    state: &WebSocketState,
    now_ms: i64,
    batch: usize,
) -> Result<RoomEffectDrainSummary, RoomEffectOutboxError> {
    let store = &state.deps.protocol.room_effect_outbox;
    let claimed = store.claim_due_head(now_ms, batch).await?;
    let needs_ownership = claimed
        .iter()
        .any(|effect| !effect.row.effect.is_terminal());
    let local_rooms = if needs_ownership {
        local_room_jids(&state.deps.protocol.room_registry).await
    } else {
        Ok(HashSet::new())
    };
    let mut summary = RoomEffectDrainSummary::default();
    for claimed in claimed {
        match drain_claimed(state, claimed, now_ms, local_rooms.as_ref(), None, false).await? {
            ClaimDisposition::Completed => summary.drained += 1,
            ClaimDisposition::Requeued => summary.requeued += 1,
            ClaimDisposition::Stale => summary.stale += 1,
            ClaimDisposition::DeadLettered => summary.dead_lettered += 1,
            ClaimDisposition::Leased | ClaimDisposition::Inline(_) => {}
        }
    }
    Ok(summary)
}

/// Exact, FIFO-respecting inline drain for a producer's reservation.  It is
/// safe when the janitor won the race: an absent or unavailable exact row is
/// simply omitted.  No current producer calls this yet; C2–C4 own response
/// vector placement and must retain every returned completion until batch
/// write acceptance.
pub async fn drain_reservation_inline(
    state: &WebSocketState,
    reservation: &RoomEffectReservation,
    initiator: Option<&FullJid>,
) -> Result<InlineRoomEffectDrain, RoomEffectOutboxError> {
    drain_reservation(state, reservation, initiator, false).await
}

/// Drain a reservation produced by a command that has just committed through
/// its local room actor. The actor acquisition is the ownership proof, so the
/// background handoff must not reclassify the fresh row as non-local while the
/// registry is between reconciliation steps.
pub async fn drain_local_reservation_after_commit(
    state: &WebSocketState,
    reservation: &RoomEffectReservation,
) -> Result<InlineRoomEffectDrain, RoomEffectOutboxError> {
    drain_reservation(state, reservation, None, true).await
}

async fn drain_reservation(
    state: &WebSocketState,
    reservation: &RoomEffectReservation,
    initiator: Option<&FullJid>,
    committed_locally: bool,
) -> Result<InlineRoomEffectDrain, RoomEffectOutboxError> {
    let now_ms = crate::time::now_ms();
    let mut drain = InlineRoomEffectDrain::default();
    let mut owned_leases = HashSet::new();
    // Inline draining still observes the same ownership fence as the janitor;
    // it merely changes the destination of the initiating session's frame.
    let inline_owned_rooms = local_room_jids(&state.deps.protocol.room_registry).await;
    for ordinal in &reservation.ordinals {
        let key = RoomEffectKey {
            lifecycle: reservation.lifecycle,
            revision: reservation.revision,
            ordinal: *ordinal,
        };
        let Some(claimed) = state
            .deps
            .protocol
            .room_effect_outbox
            .claim_exact_with_owned_leases(&key, now_ms, &owned_leases)
            .await?
        else {
            match classify_inline_claim_miss(state, &key, now_ms).await? {
                InlineClaimMiss::Absent => {}
                InlineClaimMiss::Blocked => drain.summary.blocked += 1,
                InlineClaimMiss::Leased => drain.summary.leased += 1,
                InlineClaimMiss::Stale => drain.summary.stale += 1,
            }
            continue;
        };
        match drain_claimed(
            state,
            claimed,
            now_ms,
            inline_owned_rooms.as_ref(),
            initiator,
            committed_locally,
        )
        .await?
        {
            ClaimDisposition::Completed => drain.summary.completed += 1,
            ClaimDisposition::Requeued => drain.summary.requeued += 1,
            ClaimDisposition::Stale => drain.summary.stale += 1,
            ClaimDisposition::DeadLettered => drain.summary.dead_lettered += 1,
            ClaimDisposition::Leased => drain.summary.leased += 1,
            ClaimDisposition::Inline(mut returned) => {
                if let Some(frame) = returned.first() {
                    owned_leases.insert(frame.completion.lease.clone());
                }
                drain.summary.inline += 1;
                drain.frames.append(&mut returned);
            }
        }
    }
    Ok(drain)
}

enum InlineClaimMiss {
    Absent,
    Blocked,
    Leased,
    Stale,
}

async fn classify_inline_claim_miss(
    state: &WebSocketState,
    key: &RoomEffectKey,
    now_ms: i64,
) -> Result<InlineClaimMiss, RoomEffectOutboxError> {
    let store = &state.deps.protocol.room_effect_outbox;
    let Some(row) = store.find(key).await? else {
        return Ok(InlineClaimMiss::Absent);
    };
    if row.superseded || !store.lifecycle_is_executable(&row).await? {
        return Ok(InlineClaimMiss::Stale);
    }
    let stale_lease = now_ms.saturating_sub(super::store::CLAIM_TIMEOUT_MS);
    if row
        .leased_at_ms
        .is_some_and(|leased_at_ms| leased_at_ms > stale_lease)
    {
        return Ok(InlineClaimMiss::Leased);
    }
    let lifecycle_rows = store.list_for_lifecycle(key.lifecycle).await?;
    let blocked_by_earlier = lifecycle_rows.iter().any(|candidate| {
        candidate.key.lifecycle == key.lifecycle
            && (candidate.key.revision < key.revision
                || (candidate.key.revision == key.revision && candidate.key.ordinal < key.ordinal))
    });
    if blocked_by_earlier {
        return Ok(InlineClaimMiss::Blocked);
    }
    let blocked_terminal_lease = row.effect.is_terminal()
        && lifecycle_rows.iter().any(|candidate| {
            candidate.key.lifecycle == key.lifecycle
                && candidate.key != *key
                && candidate
                    .leased_at_ms
                    .is_some_and(|leased_at_ms| leased_at_ms > stale_lease)
        });
    if blocked_terminal_lease {
        return Ok(InlineClaimMiss::Blocked);
    }
    Ok(InlineClaimMiss::Leased)
}

enum ClaimDisposition {
    Completed,
    Requeued,
    Stale,
    DeadLettered,
    Leased,
    Inline(Vec<InlineRoomEffectFrame>),
}

enum LocalQueueResult {
    Accepted(oneshot::Receiver<()>),
    NotConnected,
    TimedOut,
}

async fn drain_claimed(
    state: &WebSocketState,
    claimed: ClaimedRoomEffect,
    now_ms: i64,
    local_rooms: Result<&HashSet<jid::BareJid>, &()>,
    initiator: Option<&FullJid>,
    committed_locally: bool,
) -> Result<ClaimDisposition, RoomEffectOutboxError> {
    let store = &state.deps.protocol.room_effect_outbox;
    if !store.lifecycle_is_executable(&claimed.row).await? {
        let _ = store
            .complete(&claimed.row.key, &claimed.lease_token)
            .await?;
        return Ok(ClaimDisposition::Stale);
    }
    if !claimed.row.effect.is_terminal() && !committed_locally {
        match local_rooms {
            Ok(rooms) if rooms.contains(&claimed.row.room_jid) => {
                store.clear_unowned_since(&claimed.row.key).await?;
            }
            Ok(_) => {
                match room_claim_presence(state, &claimed.row.room_jid).await {
                    Ok(ClaimPresence::Unclaimed) => {
                        let unowned_since_ms = store
                            .note_unowned_since_if_absent(
                                &claimed.row.key,
                                &claimed.lease_token,
                                now_ms,
                            )
                            .await?
                            .unwrap_or(now_ms);
                        if now_ms.saturating_sub(unowned_since_ms) > OWNERSHIP_DEAD_LETTER_MS {
                            tracing::warn!(
                                room = %claimed.row.room_jid,
                                lifecycle = %claimed.row.key.lifecycle,
                                revision = claimed.row.key.revision.as_i64(),
                                ordinal = claimed.row.key.ordinal.as_i64(),
                                unowned_age_ms = now_ms.saturating_sub(unowned_since_ms),
                                "room effect outbox row remained globally unowned for 24h; dead-lettering"
                            );
                            let _ = store
                                .complete(&claimed.row.key, &claimed.lease_token)
                                .await?;
                            return Ok(ClaimDisposition::DeadLettered);
                        }
                    }
                    Ok(ClaimPresence::Claimed) => {
                        store.clear_unowned_since(&claimed.row.key).await?;
                    }
                    Err(()) => {}
                }
                let release_now_ms = actual_release_base_ms(now_ms);
                store
                    .release_unattempted(
                        &claimed.row.key,
                        &claimed.lease_token,
                        release_now_ms,
                        OWNERSHIP_RETRY_DELAY_MS,
                    )
                    .await?;
                return Ok(ClaimDisposition::Requeued);
            }
            Err(()) => {
                let release_now_ms = actual_release_base_ms(now_ms);
                store
                    .release_unattempted(
                        &claimed.row.key,
                        &claimed.lease_token,
                        release_now_ms,
                        OWNERSHIP_RETRY_DELAY_MS,
                    )
                    .await?;
                return Ok(ClaimDisposition::Requeued);
            }
        }
    } else if !claimed.row.effect.is_terminal() {
        store.clear_unowned_since(&claimed.row.key).await?;
    }
    if !renew_claim_lease(state, &claimed.row.key, &claimed.lease_token).await? {
        // A stale, stolen, or destroy-superseded preclaimed row must not
        // execute; complete() only deletes the still-owned superseded case.
        let _ = store
            .complete(&claimed.row.key, &claimed.lease_token)
            .await?;
        return Ok(ClaimDisposition::Stale);
    }
    // Voice capability changes are typed side effects of the same durable
    // admin mutation.  They have no XMPP wire stanza of their own, but must
    // converge on the node that owns the room before the lease completes.
    let removed_sessions = effect_removed_sessions(&claimed.row.effect);
    let voice_changes: Vec<_> = effect_voice_changes(&claimed.row.effect)
        .iter()
        .map(|change| (change.session.clone(), change.voice))
        .collect();
    crate::server::routes::websocket::muc_call_sfu::converge_moderation_deltas_via_sfu(
        state.deps.protocol.sfu.as_ref(),
        &claimed.row.room_jid,
        removed_sessions,
        &voice_changes,
    );
    let rendered = rebuild_effect(
        &claimed.row.room_jid,
        &claimed.row.effect,
        &state.deps.occupant_id_secret,
    );
    let mut acks = Vec::new();
    let mut inline = Vec::new();
    let mut remote_retry_needed = false;
    let mut local_retry_needed = false;
    for (recipient, stanza) in rendered {
        if !renew_claim_lease(state, &claimed.row.key, &claimed.lease_token).await? {
            tracing::warn!(
                room = %claimed.row.room_jid,
                lifecycle = %claimed.row.key.lifecycle,
                revision = claimed.row.key.revision.as_i64(),
                ordinal = claimed.row.key.ordinal.as_i64(),
                "room effect outbox lost its lease mid-roster; aborting the remaining recipients"
            );
            // Same condition as a failed revalidate: another holder owns
            // delivery now — report it identically so sweep telemetry (and
            // tests) see one disposition for a lost lease regardless of
            // which checkpoint detected it.
            return Ok(ClaimDisposition::Stale);
        }
        if initiator == Some(&recipient) {
            inline.push(InlineRoomEffectFrame {
                stanza,
                completion: RoomEffectCompletion {
                    key: claimed.row.key.clone(),
                    lease: claimed.lease_token.clone(),
                    pending_local_acceptances: Arc::new(Mutex::new(Some(Vec::new()))),
                },
            });
            continue;
        }
        match queue_local(state, &recipient, stanza.clone()).await {
            LocalQueueResult::Accepted(ack) => {
                acks.push(ack);
                continue;
            }
            LocalQueueResult::NotConnected => {}
            LocalQueueResult::TimedOut => {
                // Keep attempting the rest of the roster before releasing:
                // aborting here would re-spam the already-queued head on every
                // retry while starving the tail behind one backpressured
                // recipient (same discipline as the remote Retryable path).
                tracing::warn!(
                    recipient = %recipient,
                    room = %claimed.row.room_jid,
                    "room effect outbox local enqueue timed out; will release after remaining recipients"
                );
                local_retry_needed = true;
                continue;
            }
        }
        // A resource absent from this node may be registered on a peer.  A
        // successful DirectFrame bridge handoff is the remote completion
        // boundary; absent resources retain today's intentional silent drop.
        if registered_remote_resource_delivery(state, &recipient, &stanza).await
            == RegisteredRemoteDelivery::Retryable
        {
            remote_retry_needed = true;
        }
    }
    if remote_retry_needed || local_retry_needed {
        let release_now_ms = actual_release_base_ms(now_ms);
        let _ = store
            .release(
                &claimed.row.key,
                &claimed.lease_token,
                release_now_ms,
                RoomEffectLastError::InfrastructureTransient,
            )
            .await?;
        if !inline.is_empty() {
            let completion = RoomEffectCompletion {
                key: claimed.row.key.clone(),
                lease: claimed.lease_token.clone(),
                pending_local_acceptances: Arc::new(Mutex::new(Some(acks))),
            };
            for frame in &mut inline {
                frame.completion = completion.clone();
            }
            return Ok(ClaimDisposition::Inline(inline));
        }
        return Ok(ClaimDisposition::Requeued);
    }
    if !inline.is_empty() {
        let completion = RoomEffectCompletion {
            key: claimed.row.key.clone(),
            lease: claimed.lease_token.clone(),
            pending_local_acceptances: Arc::new(Mutex::new(Some(acks))),
        };
        for frame in &mut inline {
            frame.completion = completion.clone();
        }
        return Ok(ClaimDisposition::Inline(inline));
    }
    if !await_acks(acks).await {
        // Keep the lease rather than completing or releasing it: expiry
        // intentionally drives an at-least-once retry after a stalled writer.
        return Ok(ClaimDisposition::Leased);
    }
    if store
        .complete(&claimed.row.key, &claimed.lease_token)
        .await?
    {
        Ok(ClaimDisposition::Completed)
    } else {
        Ok(ClaimDisposition::Leased)
    }
}

async fn queue_local(
    state: &WebSocketState,
    recipient: &FullJid,
    stanza: Stanza,
) -> LocalQueueResult {
    let (acceptance, receiver) = OutboundWriteAcceptance::new();
    match tokio::time::timeout(
        LOCAL_ACCEPTANCE_TIMEOUT,
        state
            .deps
            .protocol
            .connection_registry
            .send_to_with_write_acceptance(recipient, stanza, acceptance),
    )
    .await
    {
        Ok(SendResult::Sent) => LocalQueueResult::Accepted(receiver),
        Ok(SendResult::NotConnected | SendResult::ChannelClosed) => LocalQueueResult::NotConnected,
        Err(_) => LocalQueueResult::TimedOut,
    }
}

async fn await_acks(acks: Vec<oneshot::Receiver<()>>) -> bool {
    let wait = async {
        for ack in acks {
            if ack.await.is_err() {
                return false;
            }
        }
        true
    };
    tokio::time::timeout(LOCAL_ACCEPTANCE_TIMEOUT, wait)
        .await
        .unwrap_or(false)
}

async fn renew_claim_lease(
    state: &WebSocketState,
    key: &RoomEffectKey,
    token: &RoomEffectLeaseToken,
) -> Result<bool, RoomEffectOutboxError> {
    state
        .deps
        .protocol
        .room_effect_outbox
        .renew_lease(key, token, crate::time::now_ms())
        .await
}

fn actual_release_base_ms(claimed_at_ms: i64) -> i64 {
    crate::time::now_ms().max(claimed_at_ms)
}

async fn local_room_jids(
    room_registry: &ActorRef<RoomRegistryActor>,
) -> Result<HashSet<jid::BareJid>, ()> {
    room_registry
        .ask(LocalRoomJids)
        .reply_timeout(OWNERSHIP_LOOKUP_TIMEOUT)
        .await
        .map(|rooms| rooms.into_iter().collect())
        .map_err(|error| {
            tracing::warn!(
                ?error,
                "room effect outbox could not resolve locally owned rooms"
            );
        })
}

async fn room_claim_presence(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
) -> Result<ClaimPresence, ()> {
    let Some(claim_store) = state.deps.app_state.clustering_claims.claim_store.as_ref() else {
        return Ok(ClaimPresence::Unclaimed);
    };
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    match tokio::time::timeout(OWNERSHIP_LOOKUP_TIMEOUT, claim_store.current_claim(&entity)).await {
        Ok(Ok(claim)) => Ok(if claim.is_none() {
            ClaimPresence::Unclaimed
        } else {
            ClaimPresence::Claimed
        }),
        Ok(Err(error)) => {
            tracing::warn!(
                room = %room_jid,
                %error,
                "room effect outbox could not confirm room claim absence; retaining row"
            );
            Err(())
        }
        Err(_) => {
            tracing::warn!(
                room = %room_jid,
                timeout_ms = OWNERSHIP_LOOKUP_TIMEOUT.as_millis() as u64,
                "room effect outbox room claim lookup timed out; retaining row"
            );
            Err(())
        }
    }
}
