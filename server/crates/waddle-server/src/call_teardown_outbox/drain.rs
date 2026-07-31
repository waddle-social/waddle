//! Execution of claimed teardown intents against the node-local SFU and
//! XMPP room actor graph.

use std::collections::HashSet;

use jid::BareJid;
use kameo::actor::ActorRef;
use waddle_sfu::{
    CallTeardownIntentLite, Identity, LiveKitTeardownExecutor, TeardownExecution,
    TeardownTargetLite,
};
use waddle_xmpp::muc::room_registry_actor::{LocalRoomJids, RoomRegistryActor};

use super::{
    CallTeardownIntent, CallTeardownOutboxError, CallTeardownRetryOutcome, TeardownTarget,
};
use crate::server::routes::muc_muji_clear::WebhookEffectOutcome;
use crate::server::routes::websocket::WebSocketState;

const ROOM_OWNERSHIP_LOOKUP_TIMEOUT: std::time::Duration =
    waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT;

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
    let store = &state.deps.protocol.call_teardown_outbox;
    let jobs = store.claim_due(batch_size).await?;
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
                Err(()) => false,
            };
            if !owned {
                // Ownership misses and lookup failures are not attempts. The
                // claim is CAS-released immediately so the owning node need
                // not wait for stale-claim recovery.
                store.release_claim(&job).await?;
                continue;
            }
        }

        if let TeardownTarget::Participant { identity, .. } = &job.intent.target {
            if room_scope(&job.intent).is_some()
                && store
                    .has_pending_muji_presence_clear(&job.intent.call_id, identity)
                    .await?
            {
                store.release_claim(&job).await?;
                continue;
            }
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
