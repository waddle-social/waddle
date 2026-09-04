//! Execution of claimed teardown intents against the node-local SFU and
//! XMPP room actor graph.

use std::collections::HashSet;

use jid::BareJid;
use kameo::actor::ActorRef;
use waddle_sfu::{
    CallTeardownIntentLite, Identity, LiveKitTeardownExecutor, ObservedCallSids, SfuService,
    TeardownExecution, TeardownTargetLite,
};
use waddle_xmpp::muc::room_actor::GetActiveMujiSessions;
use waddle_xmpp::muc::room_registry_actor::{LocalRoomJids, RoomRegistryActor};
use waddle_xmpp::ownership::{ClaimSnapshot, Entity, EntityType};

use super::{
    CallTeardownIntent, CallTeardownLastError, CallTeardownOutboxError, CallTeardownRetryOutcome,
    CallTeardownRetryReason, TeardownTarget,
};
use crate::server::routes::muc_muji_clear::WebhookEffectOutcome;
use crate::server::routes::websocket::{get_room_actor_result, WebSocketState};

const ROOM_OWNERSHIP_LOOKUP_TIMEOUT: std::time::Duration =
    waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT;
const OWNERSHIP_DEAD_LETTER_MS: i64 = 24 * 60 * 60 * 1_000;
const CLOCK_SKEW_MARGIN_MS: i64 = 2_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CallTeardownDrainSummary {
    pub drained: u64,
    pub requeued: u64,
    pub failed: u64,
}

/// Drain a bounded batch. Muji call IDs are room JIDs and therefore run only
/// on the node whose room registry currently owns that room. A raw 1:1 call
/// ID is process-local and runs only on the exact process incarnation that
/// produced it.
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
    let needs_room_ownership = jobs.iter().any(|job| job.intent.room_scope().is_some());
    let local_rooms = if needs_room_ownership {
        local_room_jids(&state.deps.protocol.room_registry).await
    } else {
        Ok(HashSet::new())
    };
    let mut summary = CallTeardownDrainSummary::default();

    for job in jobs {
        let room_scope = job.intent.room_scope();
        let _producing_node_guard = if room_scope.is_none() {
            let guard = match job.producing_node.as_ref() {
                Some(producer) => store.guard_if_current_producer(producer).await,
                None => None,
            };
            // A sid fence travels with the intent, not with the process:
            // the executor refuses fenced work until an authoritative
            // reconcile pass and resolves missing-registry fences against
            // LiveKit live state, so a fenced row is safe to execute on a
            // replacement process. Requiring the exact producer epoch
            // would strand cleanup forever after a crash between persist
            // and admin success (#1612 review round 9). Only UNFENCED
            // rows still need their producer's process-local registry.
            let sid_fenced = job.intent.room_sid.is_some()
                || matches!(
                    &job.intent.target,
                    TeardownTarget::Participant {
                        participant_sid: Some(_),
                        ..
                    }
                );
            match guard {
                Some(guard) => Some(guard),
                None if sid_fenced => {
                    tracing::info!(
                        call_id = %job.intent.call_id,
                        intent_id = %job.intent_id.as_str(),
                        "call teardown 1:1 producer is gone; executing under sid fences"
                    );
                    None
                }
                None => {
                    let old_enough =
                        now_ms.saturating_sub(job.created_at_ms) >= OWNERSHIP_DEAD_LETTER_MS;
                    if old_enough {
                        tracing::warn!(
                            call_id = %job.intent.call_id,
                            intent_id = %job.intent_id.as_str(),
                            age_ms = now_ms.saturating_sub(job.created_at_ms),
                            "call teardown 1:1 intent never reached its producing node; dead-lettering"
                        );
                        if store
                            .fail_claim_at(
                                &job,
                                CallTeardownLastError::ProducerNeverDrained,
                                now_ms,
                            )
                            .await?
                        {
                            summary.failed += 1;
                        }
                    } else {
                        store.release_claim_at(&job, now_ms).await?;
                    }
                    continue;
                }
            }
        } else {
            None
        };

        if let Some(room_jid) = room_scope {
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
                // Some targets need no room actor and may execute here
                // when NOBODY owns the room; when another node owns it,
                // keep releasing so the owner runs it with its live
                // actor. Without this, after a restart a dynamic room
                // nobody rejoined never re-enters LocalRoomJids and the
                // ownership gate would starve the row until dead-letter
                // (#1612 review rounds 12 and 14).
                if executes_without_room_owner(&job.intent)
                    && room_has_no_claim_at_all(state, &room_jid)
                        .await
                        .unwrap_or(false)
                {
                    // fall through to execution below
                } else {
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
                            .fail_claim_at(&job, CallTeardownLastError::RoomNeverOwned, now_ms)
                            .await?
                        {
                            summary.failed += 1;
                        }
                        continue;
                    }
                    // Ownership misses and lookup failures are not attempts.
                    // The claim is CAS-released immediately so the owning
                    // node need not wait for stale-claim recovery.
                    store.release_claim_at(&job, now_ms).await?;
                    continue;
                }
            }
        }

        if let TeardownTarget::Participant { identity, .. } = &job.intent.target {
            if job.intent.room_scope().is_some()
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
            IntentExecution::Retryable(reason) => match store.retry_or_fail(&job, reason).await? {
                CallTeardownRetryOutcome::Requeued { .. } => summary.requeued += 1,
                CallTeardownRetryOutcome::Failed { .. } => summary.failed += 1,
                CallTeardownRetryOutcome::ClaimLost => {}
            },
            IntentExecution::Permanent(error) => {
                if store.fail_claim_at(&job, error, now_ms).await? {
                    summary.failed += 1;
                }
            }
        }
    }
    Ok(summary)
}

/// Late session re-check shared by the destructive execution arms
/// (#1608): `true` when the intent carries session evidence and the
/// participant's live registration is bound to a DIFFERENT session —
/// i.e. a rebind superseded this intent after the drain-loop fence
/// read the binding. Unbound or absent registrations prove nothing
/// and return `false`.
fn session_superseded_by_rebind(
    state: &WebSocketState,
    intent: &CallTeardownIntent,
    participant: &jid::FullJid,
) -> bool {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return false;
    };
    session_binding_mismatch(sfu.as_ref(), intent, participant)
}

/// `true` when the intent carries session evidence and `participant`'s
/// live registration is bound to a DIFFERENT session. Unbound or
/// absent registrations prove nothing and return `false`.
fn session_binding_mismatch(
    sfu: &dyn SfuService,
    intent: &CallTeardownIntent,
    participant: &jid::FullJid,
) -> bool {
    let Some(intent_session) = &intent.session else {
        return false;
    };
    let identity = Identity::from_jid(participant.clone());
    sfu.participant_session_binding(&intent.call_id, &identity)
        .is_some_and(|bound| &bound != intent_session)
}

/// A live registration alone does NOT prove a rejoin: on the room-claim
/// owner the registration legitimately survives exactly because the
/// departure this intent represents was never applied there (that is
/// why the intent exists). Only a registration or later locally minted
/// token that POSTDATES the intent's creation PLUS a small cross-node
/// clock-skew margin proves the participant is current; anything else
/// (equal, earlier, within the skew budget, or an implementation that
/// does not track times) must let the intent execute. NTP should keep
/// nodes comfortably inside this 2s budget, and the bias is
/// intentionally toward EXECUTING the intent: the guarded effects are
/// idempotent/no-op bounded, while silently swallowing a real teardown
/// strands state (#1449 review N1/NN1/H3).
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
        // Completion-only and room-wide targets carry no participant to
        // supersede (CallThreadEndRetry is additionally non-destructive).
        TeardownTarget::Room
        | TeardownTarget::MujiRoomSweep { .. }
        | TeardownTarget::CallThreadEndRetry { .. } => return false,
    };
    // #1608 (PR #1626 review): when the intent records the signaling
    // session whose terminate produced it, a live registration bound
    // to a DIFFERENT session proves supersession directly — no clock
    // comparison, no skew budget. This closes the relay-fallback
    // window where the intent is created AFTER the replacement
    // registration and the timestamp fence alone would execute it.
    // An unbound live registration proves nothing either way and
    // falls through to the timestamp fence.
    if session_binding_mismatch(sfu, intent, participant) {
        return true;
    }
    let identity = Identity::from_jid(participant.clone());
    [
        sfu.participant_registered_at(&intent.call_id, &identity),
        sfu.participant_last_minted_at(&intent.call_id, &identity),
    ]
    .into_iter()
    .flatten()
    .max()
    .is_some_and(|current_at| {
        current_at.timestamp_millis() > intent_created_at_ms.saturating_add(CLOCK_SKEW_MARGIN_MS)
    })
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

/// Targets allowed to execute on a node that does not own the room
/// actor, provided the room has no claim at all. A completion-only
/// retry needs no actor for its durable effect (the inbox summary).
/// Participant/Room LiveKit teardowns are safe exactly when they carry
/// the sid fence the executor resolves against LiveKit's LIVE state
/// once the local entry is missing (occupancy / listing match before
/// the destructive call) — so a restart-orphaned room nobody rejoined
/// still gets its LiveKit cleanup instead of leaving the participant
/// connected until dead-letter (#1612 review round 14). Muji presence
/// effects require the owning room actor and stay gated.
fn executes_without_room_owner(intent: &CallTeardownIntent) -> bool {
    match &intent.target {
        TeardownTarget::CallThreadEndRetry { .. } => true,
        TeardownTarget::Participant {
            participant_sid, ..
        } => participant_sid.is_some(),
        TeardownTarget::Room => intent.room_sid.is_some(),
        TeardownTarget::MujiPresenceClear { .. } | TeardownTarget::MujiRoomSweep { .. } => false,
    }
}

/// Strictly no claim row at all — NOT the stale-lease case, which is a
/// transient failover window where a successor is about to take over.
/// The completion-retry ownership bypass uses this so a non-owner
/// cannot best-effort-complete (and mark done) a row during the blip
/// that the room's next owner would have broadcast properly (#1612
/// review round 13). Dead-lettering keeps the looser predicate.
async fn room_has_no_claim_at_all(state: &WebSocketState, room_jid: &BareJid) -> Result<bool, ()> {
    let Some(claim_store) = state.deps.app_state.clustering_claims.claim_store.as_ref() else {
        return Ok(true);
    };
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    match tokio::time::timeout(
        ROOM_OWNERSHIP_LOOKUP_TIMEOUT,
        claim_store.current_claim(&entity),
    )
    .await
    {
        Ok(Ok(claim)) => Ok(claim.is_none()),
        Ok(Err(error)) => {
            tracing::warn!(
                room = %room_jid,
                %error,
                "call teardown outbox could not confirm room claim absence; retaining intent"
            );
            Err(())
        }
        Err(_) => {
            tracing::warn!(
                room = %room_jid,
                timeout_ms = ROOM_OWNERSHIP_LOOKUP_TIMEOUT.as_millis() as u64,
                "call teardown outbox room claim-absence lookup timed out; retaining intent"
            );
            Err(())
        }
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

enum IntentExecution {
    Done,
    Stale,
    Retryable(CallTeardownRetryReason),
    /// An unrecoverable invariant violation: dead-letter the row as
    /// `failed` so it stays observable (and prunable) instead of
    /// masquerading as a completed effect.
    Permanent(CallTeardownLastError),
}

async fn execute_intent(
    state: &WebSocketState,
    executor: Option<&LiveKitTeardownExecutor>,
    intent: &CallTeardownIntent,
) -> IntentExecution {
    if let TeardownTarget::MujiPresenceClear {
        room_jid,
        departed,
        participant_sid,
    } = &intent.target
    {
        // #1608: re-check the session binding as late as possible — a
        // rebind can land between the drain-loop fence and this
        // execution, and the destructive presence clear must not
        // remove a NEWER session's advertisement.
        if session_superseded_by_rebind(state, intent, departed) {
            return IntentExecution::Stale;
        }
        let observed_sids = ObservedCallSids::new(intent.room_sid.clone(), participant_sid.clone());
        let observed_sids = (observed_sids.room_sid.is_some()
            || observed_sids.participant_sid.is_some())
        .then_some(observed_sids);
        return match tokio::time::timeout(
            ROOM_OWNERSHIP_LOOKUP_TIMEOUT,
            crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
                state,
                room_jid,
                departed,
                observed_sids.as_ref(),
                intent.occupant,
                waddle_sfu::UnboundOccupantPolicy::Keep,
                intent.session.as_ref(),
            ),
        )
        .await
        {
            Err(_) => IntentExecution::Retryable(CallTeardownRetryReason::MujiPresenceClear),
            Ok(outcome) => match outcome {
                WebhookEffectOutcome::Completed | WebhookEffectOutcome::Permanent(_) => {
                    IntentExecution::Done
                }
                WebhookEffectOutcome::Stale => IntentExecution::Stale,
                WebhookEffectOutcome::Retryable(_) => {
                    IntentExecution::Retryable(CallTeardownRetryReason::MujiPresenceClear)
                }
            },
        };
    }

    if let TeardownTarget::CallThreadEndRetry {
        room_jid,
        thread_id,
        anchor_origin_id,
        started,
        ended,
    } = &intent.target
    {
        // Completion-only: never replays the destructive presence clear.
        // The fence carries the failed thread's persisted payload, so a
        // lost (restart) or replaced (newer call) in-memory entry is
        // completed from the row itself rather than acknowledged away.
        return match tokio::time::timeout(
            ROOM_OWNERSHIP_LOOKUP_TIMEOUT,
            crate::server::routes::call_thread_end::maybe_broadcast_call_thread_ended_for(
                state,
                room_jid,
                Some(
                    crate::server::routes::call_thread_end::CallThreadCompletionFence {
                        thread_id,
                        anchor_origin_id,
                        started: *started,
                        ended: *ended,
                    },
                ),
            ),
        )
        .await
        {
            Err(_) => IntentExecution::Retryable(CallTeardownRetryReason::CallThreadEnd),
            Ok(outcome) => match outcome {
                WebhookEffectOutcome::Completed | WebhookEffectOutcome::Permanent(_) => {
                    IntentExecution::Done
                }
                WebhookEffectOutcome::Stale => IntentExecution::Stale,
                WebhookEffectOutcome::Retryable(_) => {
                    IntentExecution::Retryable(CallTeardownRetryReason::CallThreadEnd)
                }
            },
        };
    }

    if let TeardownTarget::MujiRoomSweep { room_jid } = &intent.target {
        let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
            return IntentExecution::Retryable(CallTeardownRetryReason::LiveKitExecutorUnavailable);
        };
        let Some(room_sid) = intent.room_sid.clone() else {
            tracing::warn!(
                call_id = %intent.call_id,
                room = %room_jid,
                "muji room sweep intent is missing the webhook room SID; dead-lettering"
            );
            return IntentExecution::Permanent(CallTeardownLastError::MissingRoomSid);
        };
        let observed_sids = ObservedCallSids::new(Some(room_sid), None);
        let mut departed: HashSet<_> = sfu
            .participants_for_call(&intent.call_id)
            .into_iter()
            .map(|identity| identity.as_jid().clone())
            .collect();
        match get_room_actor_result(state, room_jid).await {
            Ok(Some(actor)) => match actor
                .ask(GetActiveMujiSessions)
                .reply_timeout(ROOM_OWNERSHIP_LOOKUP_TIMEOUT)
                .await
            {
                Ok(actor_sessions) => departed.extend(actor_sessions),
                Err(error) => {
                    tracing::warn!(
                        room = %room_jid,
                        error = ?error,
                        "muji room sweep could not enumerate actor-held advertisements"
                    );
                    return IntentExecution::Retryable(CallTeardownRetryReason::MujiPresenceClear);
                }
            },
            Ok(None) => {
                tracing::warn!(
                    room = %room_jid,
                    "muji room sweep lost its locally owned room actor before enumeration"
                );
                return IntentExecution::Retryable(CallTeardownRetryReason::MujiPresenceClear);
            }
            Err(error) => {
                tracing::warn!(
                    room = %room_jid,
                    %error,
                    "muji room sweep could not resolve the room actor"
                );
                return IntentExecution::Retryable(CallTeardownRetryReason::MujiPresenceClear);
            }
        }
        for departed_jid in departed {
            let outcome = match tokio::time::timeout(
                ROOM_OWNERSHIP_LOOKUP_TIMEOUT,
                crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
                    state,
                    room_jid,
                    &departed_jid,
                    Some(&observed_sids),
                    None,
                    waddle_sfu::UnboundOccupantPolicy::Keep,
                    None,
                ),
            )
            .await
            {
                Err(_) => {
                    return IntentExecution::Retryable(CallTeardownRetryReason::MujiPresenceClear);
                }
                Ok(outcome) => outcome,
            };
            match outcome {
                WebhookEffectOutcome::Completed | WebhookEffectOutcome::Permanent(_) => {}
                WebhookEffectOutcome::Stale => return IntentExecution::Stale,
                WebhookEffectOutcome::Retryable(_) => {
                    return IntentExecution::Retryable(CallTeardownRetryReason::MujiPresenceClear);
                }
            }
        }
        return IntentExecution::Done;
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
        TeardownTarget::MujiPresenceClear { .. }
        | TeardownTarget::MujiRoomSweep { .. }
        | TeardownTarget::CallThreadEndRetry { .. } => {
            return IntentExecution::Done;
        }
    };
    // The executor re-checks `session` against the live binding
    // immediately before the destructive admin call (#1608), so a
    // rebind racing this drain cannot eject the newer session.
    let lite = CallTeardownIntentLite {
        call_id: intent.call_id.clone(),
        target,
        generation: intent.generation,
        room_sid: intent.room_sid.clone(),
        session: intent.session.clone(),
        occupant_session: intent.occupant,
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
