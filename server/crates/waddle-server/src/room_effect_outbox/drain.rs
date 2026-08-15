//! Leased delivery of durable room mutation effects.
//!
//! Lifecycle rule: an effect may execute only while its exact lifecycle is
//! the room's live lifecycle, or while that exact lifecycle is tombstoned.
//! The latter preserves the wipe-first/destroy-presence contract; a recreated
//! room cannot commit while its predecessor's terminal effect exists.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jid::FullJid;
use kameo::actor::ActorRef;
use tokio::sync::oneshot;
use waddle_xmpp::muc::room_registry_actor::{LocalRoomJids, RoomRegistryActor};
use waddle_xmpp::muc::RoomEffectReservation;
use waddle_xmpp::registry::{OutboundWriteAcceptance, SendResult};
use waddle_xmpp::Stanza;

use super::render::{effect_voice_changes, rebuild_effect};
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
const NOT_OWNER_DELAY_MS: i64 = 1_000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoomEffectDrainSummary {
    pub drained: u64,
    pub requeued: u64,
    pub stale: u64,
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
        match drain_claimed(state, claimed, now_ms, local_rooms.as_ref(), None).await? {
            ClaimDisposition::Completed => summary.drained += 1,
            ClaimDisposition::Requeued => summary.requeued += 1,
            ClaimDisposition::Stale => summary.stale += 1,
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
) -> Result<Vec<InlineRoomEffectFrame>, RoomEffectOutboxError> {
    let now_ms = crate::time::now_ms();
    let mut frames = Vec::new();
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
            .claim_exact(&key, now_ms)
            .await?
        else {
            continue;
        };
        match drain_claimed(
            state,
            claimed,
            now_ms,
            inline_owned_rooms.as_ref(),
            initiator,
        )
        .await?
        {
            ClaimDisposition::Inline(mut returned) => frames.append(&mut returned),
            ClaimDisposition::Completed
            | ClaimDisposition::Requeued
            | ClaimDisposition::Stale
            | ClaimDisposition::Leased => {}
        }
    }
    Ok(frames)
}

enum ClaimDisposition {
    Completed,
    Requeued,
    Stale,
    Leased,
    Inline(Vec<InlineRoomEffectFrame>),
}

async fn drain_claimed(
    state: &WebSocketState,
    claimed: ClaimedRoomEffect,
    now_ms: i64,
    local_rooms: Result<&HashSet<jid::BareJid>, &()>,
    initiator: Option<&FullJid>,
) -> Result<ClaimDisposition, RoomEffectOutboxError> {
    let store = &state.deps.protocol.room_effect_outbox;
    if !store.lifecycle_is_executable(&claimed.row).await? {
        let _ = store
            .complete(&claimed.row.key, &claimed.lease_token)
            .await?;
        return Ok(ClaimDisposition::Stale);
    }
    if !claimed.row.effect.is_terminal() {
        match local_rooms {
            Ok(rooms) if rooms.contains(&claimed.row.room_jid) => {}
            Ok(_) => {
                store
                    .release_unattempted(
                        &claimed.row.key,
                        &claimed.lease_token,
                        now_ms,
                        NOT_OWNER_DELAY_MS,
                    )
                    .await?;
                return Ok(ClaimDisposition::Requeued);
            }
            Err(()) => {
                store
                    .release_unattempted(
                        &claimed.row.key,
                        &claimed.lease_token,
                        now_ms,
                        NOT_OWNER_DELAY_MS,
                    )
                    .await?;
                return Ok(ClaimDisposition::Requeued);
            }
        }
    }
    if !store
        .revalidate(&claimed.row.key, &claimed.lease_token)
        .await?
    {
        // A destroy superseded this leased row.  Store semantics permit its
        // holder to delete it, and no wire frame may escape this barrier.
        let _ = store
            .complete(&claimed.row.key, &claimed.lease_token)
            .await?;
        return Ok(ClaimDisposition::Stale);
    }
    // Voice capability changes are typed side effects of the same durable
    // admin mutation.  They have no XMPP wire stanza of their own, but must
    // converge on the node that owns the room before the lease completes.
    for change in effect_voice_changes(&claimed.row.effect) {
        crate::server::routes::websocket::muc_call_sfu::apply_voice_grants_for_room(
            state,
            &claimed.row.room_jid,
            &change.session,
            change.voice,
        );
    }
    let rendered = rebuild_effect(
        &claimed.row.room_jid,
        &claimed.row.effect,
        &state.deps.occupant_id_secret,
    );
    let mut acks = Vec::new();
    let mut inline = Vec::new();
    let mut remote_retry_needed = false;
    for (recipient, stanza) in rendered {
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
        if let Some(ack) = queue_local(state, &recipient, stanza.clone()).await {
            acks.push(ack);
            continue;
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
    if remote_retry_needed {
        let _ = store
            .release(
                &claimed.row.key,
                &claimed.lease_token,
                now_ms,
                RoomEffectLastError::InfrastructureTransient,
            )
            .await?;
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
) -> Option<oneshot::Receiver<()>> {
    let (acceptance, receiver) = OutboundWriteAcceptance::new();
    match state
        .deps
        .protocol
        .connection_registry
        .send_to_with_write_acceptance(recipient, stanza, acceptance)
        .await
    {
        SendResult::Sent => Some(receiver),
        SendResult::NotConnected | SendResult::ChannelClosed => None,
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
