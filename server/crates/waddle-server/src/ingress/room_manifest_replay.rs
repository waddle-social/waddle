//! A sealed room child may reach only its frozen endpoint and occupancy.
use std::time::Duration;

use jid::FullJid;
use kameo::actor::ActorRef;
use waddle_xmpp::{
    ingress::IngressEffectIntent,
    muc::{
        durable::RoomClaimFenceContext,
        room_actor::{
            GetRoomSnapshot, OccupancyReplayGuardOutcome, RoomActor, RoomChainSnapshot,
            RunOccupancyReplayGuardedAsync, RunSupersededOccupancyGuardedAsync,
        },
        room_registry_actor::{GetRoom, RoomRegistryActor},
    },
    registry::BroadcastOutcome,
    Stanza,
};

use crate::{
    ingress_substrate::MessageEnvelope,
    ingress_uow::{CarbonReceiptRepository, IngressUnitOfWork, IngressUowError},
    server::routes::interpret::SmIngressAppendContext,
};

use super::{EffectReceiptKey, FrozenRoomManifest};

const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

fn validate_current_snapshot(
    manifest: &FrozenRoomManifest,
    snapshot: &RoomChainSnapshot,
) -> Result<(), IngressUowError> {
    if snapshot.occupancy_incarnation != manifest.occupancy_incarnation
        || manifest.coordinates.is_some_and(|sealed| {
            snapshot
                .durable_coordinates
                .is_none_or(|current| current.lifecycle != sealed.lifecycle)
        })
        || manifest.claim_fence.is_some() && snapshot.claim_fence.is_none()
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    if snapshot.claim_fence.as_ref().is_some_and(|fence| {
        fence.entity
            != waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                manifest.room.to_string(),
            )
    }) {
        return Err(IngressUowError::ClaimFenceMissing);
    }
    Ok(())
}

async fn current_actor(
    manifest: &FrozenRoomManifest,
    registry: Option<&ActorRef<RoomRegistryActor>>,
    target: &FullJid,
) -> Result<(ActorRef<RoomActor>, Option<RoomClaimFenceContext>), IngressUowError> {
    let actor = registry
        .ok_or(IngressUowError::EffectIntentConflict)?
        .ask(GetRoom {
            room_jid: manifest.room.clone(),
        })
        .mailbox_timeout(PROBE_TIMEOUT)
        .reply_timeout(PROBE_TIMEOUT)
        .await
        .map_err(|_| IngressUowError::EffectIntentConflict)?
        .ok_or(IngressUowError::EffectIntentConflict)?;
    let snapshot = actor
        .ask(GetRoomSnapshot {
            sender_jid: target.clone(),
        })
        .mailbox_timeout(PROBE_TIMEOUT)
        .reply_timeout(PROBE_TIMEOUT)
        .await
        .map_err(|_| IngressUowError::EffectIntentConflict)?;
    validate_current_snapshot(manifest, &snapshot)?;
    Ok((actor, snapshot.claim_fence))
}

/// The actor action owns this transaction so an asking task's timeout cannot
/// release the exact claim lock while detached SM acceptance still commits.
/// This transaction reads epoch/claim only; the inner sink owns canonical rows.
async fn under_current_claim<R>(
    uow: &IngressUnitOfWork,
    room: &jid::BareJid,
    fence: Option<&RoomClaimFenceContext>,
    action: impl std::future::Future<Output = R>,
) -> Result<R, IngressUowError> {
    if fence.is_some_and(|fence| {
        fence.entity
            != waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room.to_string(),
            )
    }) {
        return Err(IngressUowError::ClaimFenceMissing);
    }
    match uow.fencing() {
        crate::ingress_uow::IngressFencing::SingleNode if fence.is_none() => Ok(action.await),
        crate::ingress_uow::IngressFencing::SingleNode => Err(IngressUowError::ClaimFenceMissing),
        #[cfg(feature = "clustering")]
        crate::ingress_uow::IngressFencing::Clustered(_) => {
            let fence = fence.ok_or(IngressUowError::ClaimFenceMissing)?;
            let mut transaction = uow
                .begin_with_timeouts(Duration::from_millis(100), PROBE_TIMEOUT)
                .await?;
            crate::ingress_uow::CanonicalMessageRepository::protect_room_acceptance(
                &mut transaction,
                room,
                fence,
            )
            .await?;
            let outcome = action.await;
            transaction.commit().await?;
            Ok(outcome)
        }
    }
}

/// The actor action owns this short publication guard. In particular, a
/// caller timeout must not release node authority before the socket callback.
async fn enqueue_under_current_node(
    uow: &IngressUnitOfWork,
    fence: Option<&RoomClaimFenceContext>,
    action: impl FnOnce() -> BroadcastOutcome,
) -> Result<BroadcastOutcome, IngressUowError> {
    match uow.fencing() {
        crate::ingress_uow::IngressFencing::SingleNode if fence.is_none() => Ok(action()),
        crate::ingress_uow::IngressFencing::SingleNode => Err(IngressUowError::ClaimFenceMissing),
        #[cfg(feature = "clustering")]
        crate::ingress_uow::IngressFencing::Clustered(identity) => {
            let expected = &fence.ok_or(IngressUowError::ClaimFenceMissing)?.owner;
            let _publication = identity
                .guard_if_current(expected)
                .await
                .ok_or(IngressUowError::AuthorityStopped)?;
            // No await separates the active exact-owner proof and the sink.
            Ok(action())
        }
    }
}

/// Validate the payload before any endpoint decision. Unknown authority is an
/// error, never permission to fall back to today's roster or an unkeyed send.
pub(super) fn validate_child(
    envelope: &MessageEnvelope,
    receipt: &EffectReceiptKey,
    target: &FullJid,
    stanza: &Stanza,
) -> Result<(), IngressUowError> {
    let Some(manifest) = envelope.room_manifest() else {
        return Ok(());
    };
    if !manifest.contains(receipt, Some(target)) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let Stanza::Message(offered) = stanza else {
        return Err(IngressUowError::EffectIntentConflict);
    };
    let intent = manifest
        .recorded_intents()
        .iter()
        .find(|intent| super::receipt_key(intent).ok().as_ref() == Some(receipt))
        .ok_or(IngressUowError::EffectIntentConflict)?;
    let source_intent = match intent {
        IngressEffectIntent::RouteMucGroupchat { .. }
        | IngressEffectIntent::RouteMucSystemBroadcast { .. } => intent,
        IngressEffectIntent::RouteDirect { .. } => manifest
            .recorded_intents()
            .iter()
            .find(|source| {
                super::reflection_dispatch::original_intent(source).as_ref() == Some(intent)
            })
            .ok_or(IngressUowError::EffectIntentConflict)?,
        _ => return Err(IngressUowError::EffectIntentConflict),
    };
    let source = super::room_canonical::source(envelope, source_intent)
        .map_err(|_| IngressUowError::EffectIntentConflict)?;
    let expected = super::room_canonical::occupant_copy_message(source, target);
    if !super::append_authority::same_message_content(&expected, offered) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    Ok(())
}

pub(super) async fn load(
    uow: &IngressUnitOfWork,
    context: &SmIngressAppendContext,
    target: &FullJid,
    stanza: &Stanza,
) -> Result<Option<FrozenRoomManifest>, IngressUowError> {
    let (envelope, _) =
        CarbonReceiptRepository::load_authority(uow.database(), context.message_key).await?;
    validate_child(&envelope, &context.receipt, target, stanza)?;
    if envelope
        .room_manifest()
        .is_some_and(|manifest| manifest.message_key() != context.message_key)
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    Ok(envelope.room_manifest().cloned())
}

/// A strictly newer occupancy in the same inherited roster cannot take the
/// old child. Settle that child's policy progress, never a transport send.
pub(super) async fn settle_superseded(
    uow: &IngressUnitOfWork,
    manifest: &FrozenRoomManifest,
    registry: Option<&ActorRef<RoomRegistryActor>>,
    context: &SmIngressAppendContext,
    target: &FullJid,
) -> Result<Option<Vec<IngressEffectIntent>>, IngressUowError> {
    if manifest.message_key() != context.message_key {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let child = manifest
        .child(&context.receipt, Some(target))
        .ok_or(IngressUowError::EffectIntentConflict)?;
    if child.disposition != super::RoomManifestChildDisposition::Delivery {
        return Ok(None);
    }
    let watermark = child
        .occupancy
        .ok_or(IngressUowError::EffectIntentConflict)?;
    let intent = manifest
        .recorded_intents()
        .iter()
        .find(|intent| super::receipt_key(intent).ok().as_ref() == Some(&context.receipt))
        .ok_or(IngressUowError::EffectIntentConflict)?;
    if child.delivery_key
        != waddle_xmpp::ingress::DeliveryKey::resource(
            context.message_key,
            &intent.semantic_key(),
            target,
        )
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let mut progress =
        super::recorded::RouteProgress::from_intent(intent, context.received_at, Vec::new())?
            .ok_or(IngressUowError::EffectIntentConflict)?;
    super::reflection_dispatch::classify_progress(&mut progress, manifest.recorded_intents());
    if !progress.fanout.contains(target) || progress.room() != Some(&manifest.room) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let (actor, fence) = match current_actor(manifest, registry, target).await {
        Ok(proof) => proof,
        Err(error) => {
            // A read-only probe cannot prove retirement. The local sink still
            // validates delivery separately; a legacy remote wire has no new
            // manifest acceptance authority to complete.
            tracing::debug!(%error, "superseded occupancy proof unavailable");
            return Ok(None);
        }
    };
    let uow = uow.clone();
    let room = manifest.room.clone();
    let target = target.clone();
    let key = context.message_key;
    let result = actor
        .ask(RunSupersededOccupancyGuardedAsync {
            jid: target.clone(),
            occupancy_incarnation: manifest.occupancy_incarnation,
            watermark,
            action: Box::new(move || {
                Box::pin(async move {
                    under_current_claim(&uow, &room, fence.as_ref(), async {
                        #[cfg(feature = "clustering")]
                        let _publication = match uow.fencing() {
                            crate::ingress_uow::IngressFencing::Clustered(identity) => Some(
                                identity
                                    .guard_if_current(
                                        &fence
                                            .as_ref()
                                            .ok_or(IngressUowError::ClaimFenceMissing)?
                                            .owner,
                                    )
                                    .await
                                    .ok_or(IngressUowError::AuthorityStopped)?,
                            ),
                            crate::ingress_uow::IngressFencing::SingleNode => None,
                        };
                        crate::ingress_uow::run_with_retry(5, || {
                            super::execute_uow::record_delivery_progress(
                                &uow,
                                key,
                                &progress,
                                std::slice::from_ref(&target),
                                None,
                            )
                        })
                        .await
                        .map_err(|failure| failure.last_error)
                    })
                    .await?
                })
            }),
        })
        .mailbox_timeout(PROBE_TIMEOUT)
        .reply_timeout(PROBE_TIMEOUT)
        .await
        .map_err(|_| IngressUowError::EffectIntentConflict)?;
    match result {
        OccupancyReplayGuardOutcome::Executed(settled) => settled.map(Some),
        OccupancyReplayGuardOutcome::Superseded | OccupancyReplayGuardOutcome::Unproven => Ok(None),
    }
}

/// The actor validates lifecycle and monotonic occupancy immediately before the
/// synchronous socket enqueue, with no roster mutation between proof and sink.
pub(super) async fn enqueue(
    manifest: &FrozenRoomManifest,
    uow: &IngressUnitOfWork,
    registry: Option<&ActorRef<RoomRegistryActor>>,
    receipt: &EffectReceiptKey,
    target: &FullJid,
    action: impl FnOnce() -> BroadcastOutcome + Send + 'static,
) -> Result<BroadcastOutcome, IngressUowError> {
    let child = manifest
        .child(receipt, Some(target))
        .ok_or(IngressUowError::EffectIntentConflict)?;
    let watermark = child
        .occupancy
        .ok_or(IngressUowError::EffectIntentConflict)?;
    let (actor, fence) = current_actor(manifest, registry, target).await?;
    let uow = uow.clone();
    let room = manifest.room.clone();
    let result = actor
        .ask(RunOccupancyReplayGuardedAsync {
            jid: target.clone(),
            occupancy_incarnation: manifest.occupancy_incarnation,
            watermark,
            action: Box::new(move || {
                Box::pin(async move {
                    under_current_claim(&uow, &room, fence.as_ref(), async {
                        enqueue_under_current_node(&uow, fence.as_ref(), action).await
                    })
                    .await?
                })
            }),
        })
        .mailbox_timeout(PROBE_TIMEOUT)
        .reply_timeout(PROBE_TIMEOUT)
        .await
        .map_err(|_| IngressUowError::EffectIntentConflict)?;
    Ok(match result {
        OccupancyReplayGuardOutcome::Executed(outcome) => outcome?,
        OccupancyReplayGuardOutcome::Superseded => BroadcastOutcome::NotConnected,
        OccupancyReplayGuardOutcome::Unproven => return Err(IngressUowError::EffectIntentConflict),
    })
}

/// Release one policy-settled initial reflection into its original sender-stream
/// frame queue. This never allocates keyed custody or an ingress delivery retry.
pub(super) async fn release_transient_reflection(
    uow: &IngressUnitOfWork,
    decision: &super::IngressDecision,
    index: usize,
    stanza: &Stanza,
    deps: &crate::server::routes::interpret::Deps<'_>,
) -> Result<Option<Stanza>, IngressUowError> {
    use crate::ingress_uow::{
        CanonicalMessageRepository, EffectReceiptRepository, PolicyDiscardReason,
    };
    use crate::server::routes::interpret::DeliveryExecutionContext;
    let Stanza::Message(message) = stanza else {
        return Ok(None);
    };
    if message.type_ != xmpp_parsers::message::MessageType::Groupchat
        || !waddle_xmpp::xep::xep0334::should_skip_storage(message)
    {
        return Ok(None);
    }
    let key = decision
        .message_key
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    let mut tx = uow
        .begin_with_timeouts(PROBE_TIMEOUT, PROBE_TIMEOUT)
        .await?;
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    let Some(manifest) = envelope.room_manifest() else {
        tx.commit().await?;
        return Ok(None);
    };
    if !matches!(
        decision.class,
        super::IngressDecisionClass::Accepted | super::IngressDecisionClass::OwnerFirstAcceptance
    ) || deps.delivery_execution_context != DeliveryExecutionContext::Live
        || !decision
            .external_receipts
            .get(index)
            .is_some_and(Vec::is_empty)
        || manifest.message_key() != key
        || !waddle_xmpp::xep::xep0334::should_skip_storage(envelope.message())
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let target = message
        .to
        .as_ref()
        .and_then(|jid| jid.try_as_full().ok())
        .ok_or(IngressUowError::EffectIntentConflict)?;
    let reflection = manifest
        .recorded_intents()
        .iter()
        .filter_map(super::reflection_dispatch::original_intent)
        .find(|intent| {
            matches!(intent, IngressEffectIntent::RouteDirect { fanout, .. }
            if fanout.as_slice() == [target.clone()])
        })
        .ok_or(IngressUowError::EffectIntentConflict)?;
    let receipt = super::receipt_key(&reflection)?;
    validate_child(&envelope, &receipt, target, stanza)?;
    if !EffectReceiptRepository::contains_policy_discard(
        &mut tx,
        key,
        &receipt,
        PolicyDiscardReason::StorageHintForbidsHandoff,
    )
    .await?
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    tx.commit().await?;
    let frame = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sender_stream = frame.clone();
    let owned_stanza = stanza.clone();
    let stop = deps.ingress_delivery_stop.clone().or_else(|| {
        deps.web_socket_state
            .map(|state| state.deps.protocol.ingress.delivery_stop_token())
    });
    let outcome = enqueue(
        manifest,
        uow,
        deps.room_registry,
        &receipt,
        target,
        move || {
            if stop
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
            {
                return BroadcastOutcome::DroppedClosed;
            }
            match sender_stream.lock() {
                Ok(mut slot) if slot.is_none() => {
                    *slot = Some(owned_stanza);
                    BroadcastOutcome::Delivered
                }
                Ok(_) => BroadcastOutcome::DroppedClosed,
                Err(_) => BroadcastOutcome::DroppedClosed,
            }
        },
    )
    .await?;
    if outcome != BroadcastOutcome::Delivered {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let released = frame
        .lock()
        .map_err(|_| IngressUowError::EffectIntentConflict)?
        .take()
        .ok_or(IngressUowError::EffectIntentConflict)?;
    Ok(Some(released))
}

/// Keep a new detached allocation in the same serialized occupancy turn as its
/// proof. The existing SM key remains the only durable acceptance ledger.
pub(super) async fn queue_detached(
    manifest: &FrozenRoomManifest,
    uow: &IngressUnitOfWork,
    deps: &crate::server::routes::interpret::Deps<'_>,
    context: &SmIngressAppendContext,
    target: &FullJid,
    stanza: &Stanza,
) -> Result<crate::server::routes::interpret::FullJidDeliveryOutcome, IngressUowError> {
    use crate::server::routes::interpret::FullJidDeliveryOutcome;
    if context.dispatch_stream.is_some() {
        return Ok(FullJidDeliveryOutcome::Unavailable);
    }
    let Some(sm) = deps.sm_session_registry.cloned() else {
        return Ok(FullJidDeliveryOutcome::Unavailable);
    };
    let watermark = manifest
        .child(&context.receipt, Some(target))
        .and_then(|child| child.occupancy)
        .ok_or(IngressUowError::EffectIntentConflict)?;
    let (actor, fence) = current_actor(manifest, deps.room_registry, target).await?;
    let uow = uow.clone();
    let room = manifest.room.clone();
    let key = waddle_xmpp::stream_management::SmIngressAppendKey {
        message_key: context.message_key,
        kind: waddle_xmpp::stream_management::SmIngressReceiptKind::from_storage(
            context.receipt.kind.to_storage(),
        ),
        semantic_identity_hash: context.receipt.semantic_identity_hash,
        resource: target.clone(),
    };
    let target_owned = target.clone();
    let stanza = stanza.clone();
    let received_at = context.received_at.unwrap_or_else(chrono::Utc::now);
    let result = actor
        .ask(RunOccupancyReplayGuardedAsync {
            jid: target.clone(),
            occupancy_incarnation: manifest.occupancy_incarnation,
            watermark,
            action: Box::new(move || {
                Box::pin(async move {
                    under_current_claim(&uow, &room, fence.as_ref(), async move {
                        sm.record_keyed_stanza_for_detached_bound_resource(
                            &target_owned,
                            &stanza,
                            received_at,
                            key,
                        )
                        .await
                    })
                    .await
                })
            }),
        })
        .mailbox_timeout(PROBE_TIMEOUT)
        .reply_timeout(PROBE_TIMEOUT)
        .await
        .map_err(|_| IngressUowError::EffectIntentConflict)?;
    match result {
        OccupancyReplayGuardOutcome::Unproven => Err(IngressUowError::EffectIntentConflict),
        OccupancyReplayGuardOutcome::Superseded => Ok(FullJidDeliveryOutcome::Unavailable),
        OccupancyReplayGuardOutcome::Executed(Ok(Ok(outcome)))
            if outcome.is_allocated()
                || matches!(
                    outcome,
                    waddle_xmpp::stream_management::SmKeyedAppendOutcome::Suppressed
                ) =>
        {
            Ok(FullJidDeliveryOutcome::QueuedDetached)
        }
        OccupancyReplayGuardOutcome::Executed(Ok(Ok(_))) => Ok(FullJidDeliveryOutcome::Unavailable),
        OccupancyReplayGuardOutcome::Executed(Ok(Err(_)))
        | OccupancyReplayGuardOutcome::Executed(Err(_)) => {
            Err(IngressUowError::EffectIntentConflict)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::{
        ingress::{EffectMessageIdentity, EntityGeneration, MessageKey},
        muc::{durable::AdminMutationId, room_actor::OccupancyWatermark},
    };
    use xmpp_parsers::message::{Message, MessageType};

    #[tokio::test]
    async fn xep0045_newer_same_incarnation_settles_old_child_without_delivering_to_rejoined_resource(
    ) {
        use crate::ingress::{
            commit::commit_submission, execute::execute_effects, test_support::IngressFixture,
            xep0045_room_manifest_tests::room_submission, RoomManifestSnapshot,
        };
        use crate::ingress_uow::{
            CanonicalMessageRepository, DeliveryProgressRepository, EffectReceiptRepository,
            SendAttemptRepository, SendObligation,
        };
        use crate::server::routes::interpret::{
            effects::{
                delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
                Effect, ExternalEffect, ImmediateSink, PlannedEffect, RoomExecutionPath,
            },
            Deps,
        };
        use waddle_xmpp::muc::{
            room_actor::{Join, LeaveAttemptId, LeaveByRealJid, LeaveOrigin, LeaveSessionSelector},
            room_registry_actor::CreateRoom,
        };
        let fixture = IngressFixture::sqlite().await;
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let mut submission = room_submission(&fixture);
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let replaced: FullJid = "alice@example.com/phone".parse().expect("replaced");
        let other: FullJid = "other@example.com/phone".parse().expect("other human");
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "superseded-copy".into(),
                channel_id: "superseded-copy".into(),
                config: Default::default(),
            })
            .await
            .expect("room actor");
        for (target, nick) in [
            (submission.sender.clone(), "sender"),
            (replaced.clone(), "alice"),
            (other.clone(), "other"),
        ] {
            actor
                .ask(Join {
                    session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                    nick: nick.into(),
                    real_jid: target,
                    role: waddle_xmpp::Role::Participant,
                    affiliation: waddle_xmpp::Affiliation::Member,
                })
                .await
                .expect("join");
        }
        let snapshot = actor
            .ask(GetRoomSnapshot {
                sender_jid: submission.sender.clone(),
            })
            .await
            .expect("snapshot");
        if let RoomExecutionPath::Local {
            manifest_snapshot,
            snapshot_generation,
            ..
        } = &mut submission.plan.room_execution
        {
            *manifest_snapshot = Some(RoomManifestSnapshot::from(&snapshot));
            *snapshot_generation = snapshot.admission_revision;
        }
        let route = submission
            .plan
            .intents
            .iter_mut()
            .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
            .expect("route");
        if let IngressEffectIntent::RouteMucGroupchat { occupants, .. } = route {
            occupants.push(other.clone());
        }
        let receipt = super::super::receipt_key(route).expect("route receipt");
        let mut offered = submission
            .plan
            .room_canonical_message
            .as_ref()
            .expect("canonical")
            .as_ref()
            .clone();
        offered.to = Some(replaced.clone().into());
        submission
            .plan
            .plan
            .push(PlannedEffect::new(Effect::External(
                ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                    route_identity: None,
                    jid: replaced.clone(),
                    stanza: Box::new(Stanza::Message(offered)),
                    kind: PeerDeliveryKind::PeerStanza,
                    call_setup: None,
                }),
            )));
        let (sender, mut receiver) = tokio::sync::mpsc::channel(16);
        crate::server::routes::websocket::tests::register_test_connection(
            &state, &replaced, sender,
        )
        .await;
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("accept original occupancy");
        let key = decision.message_key.expect("key");
        let mut tx = fixture.uow.begin().await.expect("read seal");
        let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("envelope")
            .expect("canonical row");
        let manifest = envelope.room_manifest().expect("manifest").clone();
        tx.commit().await.expect("seal read commit");
        let context = SmIngressAppendContext {
            message_key: key,
            receipt: receipt.clone(),
            received_at: None,
            archive_positions: Vec::new(),
            dispatch_stream: None,
            authority: super::super::append_authority::AppendAuthority::Verified,
        };
        let registry = Some(&state.deps.protocol.room_registry);
        assert!(
            settle_superseded(&fixture.uow, &manifest, registry, &context, &replaced)
                .await
                .expect("current occupancy probe")
                .is_none()
        );
        actor
            .ask(LeaveByRealJid {
                sender_jid: replaced.clone(),
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: LeaveOrigin::Fresh,
            })
            .await
            .expect("leave");
        assert!(
            settle_superseded(&fixture.uow, &manifest, registry, &context, &replaced)
                .await
                .expect("absent occupancy stays pending")
                .is_none()
        );
        actor
            .ask(Join {
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                nick: "replacement-alice".into(),
                real_jid: replaced.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("rejoin");
        let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
        deps.room_registry = registry;
        deps.web_socket_state = Some(&state);
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(2),
        )
        .await;
        assert!(
            receiver.try_recv().is_err(),
            "historical copy must not reach the replacement socket"
        );
        let mut tx = fixture.uow.begin().await.expect("read policy progress");
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("policy progress"),
            vec![replaced.clone()]
        );
        assert!(
            !EffectReceiptRepository::contains(
                &mut tx,
                key,
                receipt.kind,
                &receipt.semantic_identity_hash
            )
            .await
            .expect("aggregate"),
            "other human remains pending"
        );
        assert!(
            SendAttemptRepository::status(
                &mut tx,
                &SendObligation {
                    message: key,
                    receipt: receipt.clone(),
                    recipient: replaced.clone()
                }
            )
            .await
            .expect("send status")
            .is_none(),
            "policy settlement is not transport completion"
        );
        tx.commit().await.expect("progress read commit");
        assert!(
            settle_superseded(&fixture.uow, &manifest, registry, &context, &replaced)
                .await
                .expect("idempotent policy replay")
                .is_some()
        );
        assert_eq!(
            fixture.count("ingress_delivery_receipts").await,
            1,
            "one original target settled once"
        );
        let mut unknown = manifest.clone();
        unknown.occupancy_incarnation = AdminMutationId::generate();
        assert!(
            settle_superseded(&fixture.uow, &unknown, registry, &context, &other)
                .await
                .expect("unknown proof does not settle")
                .is_none(),
            "unknown incarnation remains pending"
        );
        assert_eq!(
            fixture.count("sm_ingress_appends").await,
            0,
            "no stale detached append"
        );
        fixture.close().await;
    }

    #[test]
    fn xep0045_current_room_proof_allows_revision_progress_but_rejects_recreated_lifecycle() {
        use waddle_xmpp::muc::durable::{RoomCommittedCoordinates, RoomLifecycleId, RoomRevision};
        let room: jid::BareJid = "room@muc.example.test".parse().expect("room");
        let coordinates = RoomCommittedCoordinates {
            lifecycle: RoomLifecycleId::generate(),
            revision: RoomRevision::initial(),
        };
        let incarnation = AdminMutationId::generate();
        let manifest = FrozenRoomManifest::build(
            MessageKey::new(),
            room.clone(),
            waddle_xmpp_core::xep0359::StanzaId::new("sealed", room.into()),
            &super::super::RoomManifestSnapshot {
                host_no_transport: Vec::new(),
                coordinates: Some(coordinates),
                occupancy_incarnation: incarnation,
                occupants: Vec::new(),
            },
            None,
            &[],
        )
        .expect("manifest");
        let mut current = RoomChainSnapshot {
            claim_fence: None,
            durable_coordinates: Some(RoomCommittedCoordinates {
                revision: coordinates.revision.next().expect("next"),
                ..coordinates
            }),
            occupancy_incarnation: incarnation,
            occupants: Vec::new(),
            sender_nick: None,
            sender_role: None,
            sender_affiliation: None,
            sender_nickname_generation: None,
            config: Default::default(),
            durable_recipient_bare_jids: Vec::new(),
            admission_revision: 0,
        };
        validate_current_snapshot(&manifest, &current).expect("same lifecycle progressed");
        current.durable_coordinates = Some(RoomCommittedCoordinates {
            lifecycle: RoomLifecycleId::generate(),
            revision: RoomRevision::initial(),
        });
        assert!(
            validate_current_snapshot(&manifest, &current).is_err(),
            "recreated room cannot accept historical work"
        );
        current.durable_coordinates = None;
        assert!(
            validate_current_snapshot(&manifest, &current).is_err(),
            "unknown lifecycle remains pending"
        );
        current.durable_coordinates = Some(coordinates);
        current.occupancy_incarnation = AdminMutationId::generate();
        assert!(
            validate_current_snapshot(&manifest, &current).is_err(),
            "lost live-roster inheritance remains pending"
        );
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn xep0045_cancelled_caller_cannot_publish_after_queued_node_rotation() {
        use waddle_xmpp::ownership::NodeIdentity;
        let Some(mut fixture) =
            super::super::test_support::IngressFixture::postgres("manifest_cancelled_enqueue")
                .await
        else {
            return;
        };
        let room: jid::BareJid = "room@muc.example.test".parse().expect("room");
        let fence = fixture.room_fence(&room).await;
        let crate::ingress_uow::IngressFencing::Clustered(identity) = fixture.uow.fencing() else {
            panic!("clustered fixture");
        };
        let identity = identity.clone();
        let preceding_publication = identity
            .guard_if_current(&fence.owner)
            .await
            .expect("preceding publication");
        let writer_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let writer_signal = writer_started.clone();
        let rotation = tokio::spawn(async move {
            writer_signal.notify_one();
            identity
                .rotate(NodeIdentity::new("replacement", "new-incarnation"))
                .await;
        });
        writer_started.notified().await;
        let sink_waiting = std::sync::Arc::new(tokio::sync::Notify::new());
        let waiting_signal = sink_waiting.clone();
        let invoked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_invoked = invoked.clone();
        let (completion, completed) = tokio::sync::oneshot::channel();
        let uow = fixture.uow.clone();
        let actor_action = tokio::spawn(async move {
            let result = under_current_claim(&uow, &room, Some(&fence), async {
                waiting_signal.notify_one();
                enqueue_under_current_node(&uow, Some(&fence), || {
                    callback_invoked.store(true, std::sync::atomic::Ordering::SeqCst);
                    BroadcastOutcome::Delivered
                })
                .await
            })
            .await
            .and_then(|outcome| outcome);
            completion
                .send(result)
                .expect("test observes actor-owned action");
        });
        let caller = tokio::spawn(async move {
            actor_action.await.expect("actor action join");
        });
        sink_waiting.notified().await;
        caller.abort();
        assert!(caller.await.expect_err("caller cancelled").is_cancelled());
        drop(preceding_publication);
        tokio::time::timeout(Duration::from_secs(1), rotation)
            .await
            .expect("rotation resolves")
            .expect("rotation join");
        let result = tokio::time::timeout(Duration::from_secs(1), completed)
            .await
            .expect("actor action survives caller cancellation")
            .expect("completion");
        assert!(
            matches!(result, Err(IngressUowError::AuthorityStopped)),
            "captured old actor owner cannot publish after rotation"
        );
        assert!(
            !invoked.load(std::sync::atomic::Ordering::SeqCst),
            "stale socket callback was never invoked"
        );
        fixture.close().await;
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn xep0045_room_claim_protection_does_not_deadlock_queued_node_rotation() {
        use waddle_xmpp::ownership::NodeIdentity;
        let Some(mut fixture) =
            super::super::test_support::IngressFixture::postgres("manifest_rotation").await
        else {
            return;
        };
        let room: jid::BareJid = "room@muc.example.test".parse().expect("room");
        let fence = fixture.room_fence(&room).await;
        let crate::ingress_uow::IngressFencing::Clustered(identity) = fixture.uow.fencing() else {
            panic!("clustered fixture");
        };
        let identity = identity.clone();
        let preceding_publication = identity
            .guard_if_current(&fence.owner)
            .await
            .expect("preceding publication");
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let enter_sink = std::sync::Arc::new(tokio::sync::Notify::new());
        let uow = fixture.uow.clone();
        let entered_action = entered.clone();
        let enter_action = enter_sink.clone();
        let sink_identity = identity.clone();
        let sink_fence = fence.clone();
        let child = tokio::spawn(async move {
            under_current_claim(&uow, &room, Some(&fence), async {
                entered_action.notify_one();
                enter_action.notified().await;
                sink_identity
                    .guard_if_current(&sink_fence.owner)
                    .await
                    .is_some()
            })
            .await
        });
        entered.notified().await;
        let rotation_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let started = rotation_started.clone();
        let rotation = tokio::spawn(async move {
            started.notify_one();
            identity
                .rotate(NodeIdentity::new("replacement", "replacement-epoch"))
                .await;
        });
        rotation_started.notified().await;
        drop(preceding_publication);
        tokio::time::timeout(Duration::from_secs(1), rotation)
            .await
            .expect("outer claim row protection does not retain rotation gate")
            .expect("rotation join");
        enter_sink.notify_one();
        let accepted = tokio::time::timeout(Duration::from_secs(1), child)
            .await
            .expect("inner node guard resolves after rotation")
            .expect("child join")
            .expect("protected action completes");
        assert!(!accepted, "inner sink must reject the old node incarnation");
        fixture.close().await;
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn xep0045_current_claim_blocks_takeover_until_child_acceptance_resolves() {
        let Some(mut fixture) =
            super::super::test_support::IngressFixture::postgres("manifest_current_claim").await
        else {
            return;
        };
        let room: jid::BareJid = "room@muc.example.test".parse().expect("room");
        let fence = fixture.room_fence(&room).await;
        let accepted = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let uow = fixture.uow.clone();
        let child_room = room.clone();
        let child_fence = fence.clone();
        let child_started = accepted.clone();
        let child_release = release.clone();
        let child = tokio::spawn(async move {
            under_current_claim(&uow, &child_room, Some(&child_fence), async move {
                child_started.notify_one();
                child_release.notified().await;
                "durable child accepted"
            })
            .await
        });
        accepted.notified().await;
        let db = fixture.db.clone();
        let claim_entity = format!("room_actor:{room}");
        let takeover = tokio::spawn(async move {
            let connection = db.guard().await.expect("claim updater");
            connection
                .execute(
                    "UPDATE clustering_claims SET claim_epoch = claim_epoch + 1 WHERE entity = ? /* manifest_current_claim_takeover */",
                    crate::db_params![claim_entity],
                )
                .await
                .expect("takeover completes");
        });
        let connection = fixture.db.guard().await.expect("lock observer");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut rows = connection.query("SELECT COUNT(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query LIKE '%UPDATE clustering_claims SET claim_epoch%' AND query LIKE '%manifest_current_claim_takeover%'", ()).await.expect("lock waiters");
                let count: i64 = rows.next().await.expect("row read").expect("count row").get(0).expect("count");
                if count > 0 { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("takeover waits on current claim");
        drop(connection);
        assert!(
            !takeover.is_finished(),
            "claim remains held throughout acceptance"
        );
        release.notify_one();
        assert_eq!(
            child
                .await
                .expect("child join")
                .expect("accepted under current claim"),
            "durable child accepted"
        );
        takeover.await.expect("takeover join");
        let invoked = std::sync::atomic::AtomicBool::new(false);
        assert!(
            under_current_claim(&fixture.uow, &room, Some(&fence), async {
                invoked.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .await
            .is_err()
        );
        assert!(
            !invoked.load(std::sync::atomic::Ordering::SeqCst),
            "historical claim cannot enter sink after takeover"
        );
        fixture.close().await;
    }

    #[test]
    fn xep0045_sealed_copy_rejects_a_new_resource_and_changed_canonical_content() {
        let room: jid::BareJid = "room@muc.example.test".parse().expect("room");
        let sender: FullJid = "sender@example.test/web".parse().expect("sender");
        let peer: FullJid = "peer@example.test/phone".parse().expect("peer");
        let sibling: FullJid = "sender@example.test/mobile".parse().expect("sibling");
        let stamp = waddle_xmpp_core::xep0359::StanzaId::new("admitted", room.clone().into());
        let route = IngressEffectIntent::RouteMucGroupchat {
            room: room.clone(),
            occupants: vec![sender.clone(), peer.clone()],
            reflection: sender.clone(),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::stanza(stamp.clone()),
        };
        let reflection =
            super::super::reflection_dispatch::original_intent(&route).expect("reflection");
        let mut message = Message::new(Some(room.clone().into()));
        message.type_ = MessageType::Groupchat;
        message.from = Some(room.with_resource_str("sender").expect("nick").into());
        message
            .bodies
            .insert(Default::default(), "admitted content".into());
        waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &stamp);
        waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
            &mut message,
            &waddle_xmpp::xep::xep0421::OccupantId("sender-occupancy".into()),
        );
        let manifest = FrozenRoomManifest::build(
            MessageKey::new(),
            room,
            stamp,
            &super::super::RoomManifestSnapshot {
                host_no_transport: Vec::new(),
                coordinates: None,
                occupancy_incarnation: AdminMutationId::generate(),
                occupants: vec![
                    (sender.clone(), OccupancyWatermark::from_revision(1)),
                    (peer.clone(), OccupancyWatermark::from_revision(2)),
                ],
            },
            None,
            &[route.clone(), reflection.clone()],
        )
        .expect("manifest");
        let envelope = MessageEnvelope::new(message.clone())
            .with_room_manifest(manifest)
            .expect("envelope");
        for (intent, target) in [(&route, &peer), (&reflection, &sender)] {
            let receipt = super::super::receipt_key(intent).expect("receipt");
            let copy = super::super::room_canonical::occupant_copy_message(&message, target);
            validate_child(&envelope, &receipt, target, &Stanza::Message(copy.clone()))
                .expect("admitted child");
            let mut changed = copy;
            changed
                .bodies
                .insert(Default::default(), "replacement content".into());
            assert!(
                validate_child(&envelope, &receipt, target, &Stanza::Message(changed)).is_err()
            );
            let newcomer = super::super::room_canonical::occupant_copy_message(&message, &sibling);
            assert!(
                validate_child(&envelope, &receipt, &sibling, &Stanza::Message(newcomer)).is_err()
            );
        }
    }
}

#[cfg(test)]
#[path = "room_manifest_replay_transient_tests.rs"]
mod transient_tests;
