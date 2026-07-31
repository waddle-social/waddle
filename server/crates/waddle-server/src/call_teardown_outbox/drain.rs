//! Execution of claimed teardown intents against the node-local SFU and
//! XMPP room actor graph.

use std::collections::HashSet;

use jid::BareJid;
use kameo::actor::ActorRef;
use waddle_sfu::{
    CallTeardownIntentLite, Identity, LiveKitTeardownExecutor, SfuService, TeardownExecution,
    TeardownTargetLite,
};
use waddle_xmpp::muc::room_registry_actor::{LocalRoomJids, RoomRegistryActor};
use waddle_xmpp::ownership::{ClaimSnapshot, Entity, EntityType};

use super::{
    CallTeardownIntent, CallTeardownOutboxError, CallTeardownRetryOutcome, TeardownTarget,
};
use crate::server::routes::muc_muji_clear::WebhookEffectOutcome;
use crate::server::routes::websocket::WebSocketState;

const ROOM_OWNERSHIP_LOOKUP_TIMEOUT: std::time::Duration =
    waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT;
const OWNERSHIP_DEAD_LETTER_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CallTeardownDrainSummary {
    pub drained: u64,
    pub requeued: u64,
    pub failed: u64,
}

/// Drain a bounded batch. Muji call IDs are room JIDs and therefore run only
/// on the node whose room registry currently owns that room. A raw 1:1 call
/// ID is process-local and may be drained by any node.
pub(crate) async fn drain_due(
    state: &WebSocketState,
    batch_size: usize,
) -> Result<CallTeardownDrainSummary, CallTeardownOutboxError> {
    drain_due_at(state, batch_size, crate::time::now_ms()).await
}

pub(super) async fn drain_due_at(
    state: &WebSocketState,
    batch_size: usize,
    now_ms: i64,
) -> Result<CallTeardownDrainSummary, CallTeardownOutboxError> {
    let store = &state.deps.protocol.call_teardown_outbox;
    let jobs = store.claim_due_at(batch_size, now_ms).await?;
    let needs_room_ownership = jobs.iter().any(|job| room_scope(&job.intent).is_some());
    let local_rooms = if needs_room_ownership {
        local_room_jids(&state.deps.protocol.room_registry).await
    } else {
        Ok(HashSet::new())
    };
    let mut summary = CallTeardownDrainSummary::default();

    for job in jobs {
        if let Some(room_jid) = room_scope(&job.intent) {
            let owned = match &local_rooms {
                Ok(rooms) => rooms.contains(&room_jid),
                Err(()) => {
                    // A registry timeout or stopped mailbox says nothing
                    // about ownership. Preserve the row for a later healthy
                    // lookup instead of converting a transient control-plane
                    // failure into a permanent dead letter.
                    store.release_claim_at(&job, now_ms).await?;
                    continue;
                }
            };
            if !owned {
                let old_enough =
                    now_ms.saturating_sub(job.created_at_ms) >= OWNERSHIP_DEAD_LETTER_MS;
                let globally_unclaimed = old_enough
                    && room_is_globally_unclaimed(state, &room_jid)
                        .await
                        .unwrap_or(false);
                if globally_unclaimed {
                    tracing::warn!(
                        call_id = %job.intent.call_id,
                        intent_id = %job.intent_id.as_str(),
                        room = %room_jid,
                        age_ms = now_ms.saturating_sub(job.created_at_ms),
                        "call teardown room-scoped intent never reached an owning node; dead-lettering"
                    );
                    if store
                        .fail_claim_at(&job, "room_never_owned".to_owned(), now_ms)
                        .await?
                    {
                        summary.failed += 1;
                    }
                    continue;
                }
                // Ownership misses and lookup failures are not attempts. The
                // claim is CAS-released immediately so the owning node need
                // not wait for stale-claim recovery.
                store.release_claim_at(&job, now_ms).await?;
                continue;
            }
        }

        if let TeardownTarget::Participant { identity, .. } = &job.intent.target {
            if room_scope(&job.intent).is_some()
                && store
                    .has_pending_muji_presence_clear(&job.intent.call_id, identity)
                    .await?
            {
                store.release_claim_at(&job, now_ms).await?;
                continue;
            }
        }

        if stale_superseded_by_live_participant(
            state.deps.protocol.sfu.as_deref(),
            &job.intent,
            job.created_at_ms,
        ) {
            tracing::info!(
                call_id = %job.intent.call_id,
                intent_id = %job.intent_id.as_str(),
                "call teardown intent was superseded by a rejoin registered after the \
                 intent was created; skipping stale drain item"
            );
            if store.mark_done(&job).await? {
                summary.drained += 1;
                waddle_xmpp::telemetry::call::increment_call_teardown_stale_dropped();
            }
            continue;
        }

        match execute_intent(
            state,
            state.deps.protocol.call_teardown_executor.as_ref(),
            &job.intent,
        )
        .await
        {
            IntentExecution::Done => {
                if store.mark_done(&job).await? {
                    summary.drained += 1;
                }
            }
            IntentExecution::Stale => {
                if store.mark_done(&job).await? {
                    summary.drained += 1;
                    waddle_xmpp::telemetry::call::increment_call_teardown_stale_dropped();
                }
            }
            IntentExecution::Retryable(reason) => {
                match store.retry_or_fail(&job, reason.as_db_value()).await? {
                    CallTeardownRetryOutcome::Requeued { .. } => summary.requeued += 1,
                    CallTeardownRetryOutcome::Failed { .. } => summary.failed += 1,
                    CallTeardownRetryOutcome::ClaimLost => {}
                }
            }
        }
    }
    Ok(summary)
}

/// A live registration alone does NOT prove a rejoin: on the room-claim
/// owner the registration legitimately survives exactly because the
/// departure this intent represents was never applied there (that is
/// why the intent exists). Only a registration that POSTDATES the
/// intent's creation is a rejoin; anything else (equal, earlier, or an
/// implementation that does not track times) must let the intent
/// execute (#1449 review N1).
fn stale_superseded_by_live_participant(
    sfu: Option<&dyn SfuService>,
    intent: &CallTeardownIntent,
    intent_created_at_ms: i64,
) -> bool {
    let Some(sfu) = sfu else {
        return false;
    };
    let participant = match &intent.target {
        TeardownTarget::Participant { identity, .. } => identity,
        TeardownTarget::MujiPresenceClear { departed, .. } => departed,
        TeardownTarget::Room => return false,
    };
    sfu.participant_registered_at(&intent.call_id, &Identity::from_jid(participant.clone()))
        .is_some_and(|registered_at| registered_at.timestamp_millis() > intent_created_at_ms)
}

async fn room_is_globally_unclaimed(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<bool, ()> {
    let Some(claim_store) = state.deps.app_state.clustering_claims.claim_store.as_ref() else {
        // Without clustering, the successful local-room snapshot above is
        // the complete ownership view for this process.
        return Ok(true);
    };
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    match tokio::time::timeout(
        ROOM_OWNERSHIP_LOOKUP_TIMEOUT,
        claim_store.current_claim(&entity),
    )
    .await
    {
        Ok(Ok(claim)) => Ok(claim_permits_dead_letter(claim.as_ref())),
        Ok(Err(error)) => {
            tracing::warn!(
                room = %room_jid,
                %error,
                "call teardown outbox could not confirm global room ownership; retaining intent"
            );
            Err(())
        }
        Err(_) => {
            tracing::warn!(
                room = %room_jid,
                "call teardown outbox global room-ownership lookup timed out; retaining intent"
            );
            Err(())
        }
    }
}

/// A stale lease also occurs transiently between an owner's lease
/// expiring and its successor claiming (failover blip). Dead-lettering
/// during that blip is accepted (#1449 review N4): it additionally
/// requires the intent to be 24h old AND this node's local ownership
/// check to have already missed, and a presence-clear that old has
/// near-zero remaining value — the reconciler converged the room long
/// ago.
fn claim_permits_dead_letter(claim: Option<&ClaimSnapshot>) -> bool {
    claim.is_none_or(|snapshot| !snapshot.owner_lease_fresh)
}

#[cfg(test)]
mod ownership_tests {
    use super::claim_permits_dead_letter;
    use waddle_xmpp::ownership::{ClaimEpoch, ClaimSnapshot, NodeIdentity};

    #[test]
    fn global_claim_dead_letter_requires_no_fresh_owner_lease() {
        let fresh = ClaimSnapshot {
            owner: NodeIdentity::new("fresh-node", "epoch-1"),
            claim_epoch: ClaimEpoch(1),
            owner_lease_fresh: true,
        };
        let stale = ClaimSnapshot {
            owner: NodeIdentity::new("stale-node", "epoch-2"),
            claim_epoch: ClaimEpoch(2),
            owner_lease_fresh: false,
        };

        assert!(claim_permits_dead_letter(None));
        assert!(!claim_permits_dead_letter(Some(&fresh)));
        assert!(claim_permits_dead_letter(Some(&stale)));
    }
}

async fn local_room_jids(
    room_registry: &ActorRef<RoomRegistryActor>,
) -> Result<HashSet<BareJid>, ()> {
    room_registry
        .ask(LocalRoomJids)
        .reply_timeout(ROOM_OWNERSHIP_LOOKUP_TIMEOUT)
        .await
        .map(|rooms| rooms.into_iter().collect())
        .map_err(|error| {
            tracing::warn!(
                error = ?error,
                "call teardown outbox could not resolve locally owned rooms; scoped intents remain queued"
            );
        })
}

fn room_scope(intent: &CallTeardownIntent) -> Option<BareJid> {
    match &intent.target {
        TeardownTarget::MujiPresenceClear { room_jid, .. } => Some(room_jid.clone()),
        TeardownTarget::Participant { .. } | TeardownTarget::Room => {
            // Muji uses the bare room JID verbatim as CallId. Raw 1:1 call
            // IDs are scoped opaque identifiers and do not parse as JIDs;
            // their registries are node-local, so any node may retry them.
            intent.call_id.as_str().parse().ok()
        }
    }
}

enum IntentExecution {
    Done,
    Stale,
    Retryable(CallTeardownRetryReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallTeardownRetryReason {
    MujiPresenceClear,
    LiveKitExecutorUnavailable,
    LiveKitAdmin,
    LiveKitOccupied,
}

impl CallTeardownRetryReason {
    const fn as_db_value(self) -> &'static str {
        match self {
            Self::MujiPresenceClear => "muji_presence_clear_retryable",
            Self::LiveKitExecutorUnavailable => "livekit_teardown_executor_unavailable",
            Self::LiveKitAdmin => "livekit_admin_retryable",
            Self::LiveKitOccupied => "livekit_room_occupied",
        }
    }
}

async fn execute_intent(
    state: &WebSocketState,
    executor: Option<&LiveKitTeardownExecutor>,
    intent: &CallTeardownIntent,
) -> IntentExecution {
    if let TeardownTarget::MujiPresenceClear { room_jid, departed } = &intent.target {
        return match tokio::time::timeout(
            ROOM_OWNERSHIP_LOOKUP_TIMEOUT,
            crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
                state, room_jid, departed, None,
            ),
        )
        .await
        {
            Err(_) => IntentExecution::Retryable(CallTeardownRetryReason::MujiPresenceClear),
            Ok(outcome) => match outcome {
                WebhookEffectOutcome::Completed | WebhookEffectOutcome::Permanent(_) => {
                    IntentExecution::Done
                }
                WebhookEffectOutcome::Retryable(_) => {
                    IntentExecution::Retryable(CallTeardownRetryReason::MujiPresenceClear)
                }
            },
        };
    }

    let Some(executor) = executor else {
        return IntentExecution::Retryable(CallTeardownRetryReason::LiveKitExecutorUnavailable);
    };
    let target = match &intent.target {
        TeardownTarget::Participant {
            identity,
            participant_sid,
        } => TeardownTargetLite::Participant {
            identity: Identity::from_jid(identity.clone()),
            participant_sid: participant_sid.clone(),
        },
        TeardownTarget::Room => TeardownTargetLite::Room,
        TeardownTarget::MujiPresenceClear { .. } => return IntentExecution::Done,
    };
    let lite = CallTeardownIntentLite {
        call_id: intent.call_id.clone(),
        target,
        generation: intent.generation,
        room_sid: intent.room_sid.clone(),
    };
    match executor.execute(&lite).await {
        Ok(TeardownExecution::Executed) => IntentExecution::Done,
        Ok(TeardownExecution::StaleGeneration) => IntentExecution::Stale,
        Ok(TeardownExecution::Occupied) => {
            IntentExecution::Retryable(CallTeardownRetryReason::LiveKitOccupied)
        }
        Err(error) => {
            tracing::warn!(
                call_id = %intent.call_id,
                %error,
                "durable LiveKit teardown attempt failed; scheduling retry"
            );
            IntentExecution::Retryable(CallTeardownRetryReason::LiveKitAdmin)
        }
    }
}
