use super::submission::IngressSubmission;
use crate::ingress_uow::{IngressUowError, IngressUowTransaction};

/// Admission coordinates fence the snapshot once. Later revisions do not revoke
/// the children of an already accepted manifest.
pub(super) async fn assert_snapshot_coordinates(
    tx: &mut IngressUowTransaction<'_>,
    submission: &IngressSubmission,
) -> Result<(), IngressUowError> {
    use crate::server::routes::interpret::effects::{
        room::RoomFenceRequirement, RoomExecutionPath,
    };
    let RoomExecutionPath::Local {
        room,
        fence,
        manifest_snapshot: Some(snapshot),
        ..
    } = &submission.plan.room_execution
    else {
        return Ok(());
    };
    let Some(coordinates) = snapshot.coordinates else {
        return if matches!(fence, RoomFenceRequirement::Guarded(_)) {
            Err(IngressUowError::RoomGenerationStale)
        } else {
            Ok(())
        };
    };
    crate::ingress_uow::CanonicalMessageRepository::assert_room_snapshot(tx, room, coordinates)
        .await
}

pub(super) async fn seal(
    tx: &mut IngressUowTransaction<'_>,
    key: waddle_xmpp::ingress::MessageKey,
    submission: &IngressSubmission,
) -> Result<Option<super::FrozenRoomManifest>, IngressUowError> {
    use crate::ingress_uow::{CanonicalMessageRepository, EffectIntentRepository};
    use crate::server::routes::interpret::effects::{
        room::RoomFenceRequirement, RoomExecutionPath,
    };
    use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};
    let envelope = CanonicalMessageRepository::load_envelope(tx, key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    if let Some(manifest) = envelope.room_manifest() {
        return Ok(Some(manifest.clone()));
    }
    let RoomExecutionPath::Local {
        room,
        fence,
        manifest_snapshot: Some(snapshot),
        ..
    } = &submission.plan.room_execution
    else {
        return Ok(None);
    };
    let intents = EffectIntentRepository::load(tx, key).await?;
    let stamp = intents.iter().find_map(|intent| match intent {
        IngressEffectIntent::RouteMucGroupchat {
            room: saved,
            route_identity: EffectMessageIdentity::StanzaId(stamp),
            ..
        }
        | IngressEffectIntent::RouteMucSystemBroadcast {
            room: saved,
            route_identity: EffectMessageIdentity::StanzaId(stamp),
            ..
        } if saved == room => Some(stamp.clone()),
        _ => None,
    });
    let Some(stamp) = stamp else {
        return Ok(None);
    };
    // Host-owned occupant copies are an intentional no-transport projection.
    // Freeze that original planner disposition rather than rediscovering it on replay.
    let mut snapshot = snapshot.clone();
    snapshot.host_no_transport = submission.plan.plan.iter().filter_map(|planned| {
        match &planned.effect {
            crate::server::routes::interpret::effects::Effect::External(
                crate::server::routes::interpret::effects::ExternalEffect::Delivery(
                    crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect::HostOwnedCopy { target, .. }
                )
            ) => Some(target.clone()),
            _ => None,
        }
    }).collect();
    let manifest = super::FrozenRoomManifest::build(
        key,
        room.clone(),
        stamp,
        &snapshot,
        match fence {
            RoomFenceRequirement::Guarded(fence) => Some(fence.clone()),
            RoomFenceRequirement::Unfenced => None,
        },
        &intents,
    )?;
    let envelope = envelope.with_room_manifest(manifest.clone())?;
    CanonicalMessageRepository::record_room_canonical_envelope(tx, key, &envelope).await?;
    Ok(Some(manifest))
}

/// A sealed reflection is delivered by its ordinary keyed endpoint sink.
pub(super) fn prepare_sealed_plan(
    plan: &mut super::IngressPlan,
    manifest: &super::FrozenRoomManifest,
    initial_transient: &[super::EffectReceiptKey],
) {
    use crate::server::routes::interpret::effects::{
        Effect, ExternalEffect, PlanSuppressionPolicy,
    };
    plan.plan.retain_mut(|planned| {
        if let Effect::External(effect @ ExternalEffect::Frame(_)) = &planned.effect {
            if child_receipts(effect, manifest).iter().any(|receipt| initial_transient.contains(receipt)) {
                planned.suppression = PlanSuppressionPolicy::SenderOnly;
                return true;
            }
        }
        if matches!(&planned.effect, Effect::External(ExternalEffect::Frame(stanza))
            if matches!(stanza.as_ref(), waddle_xmpp::Stanza::Message(message) if message.type_ == xmpp_parsers::message::MessageType::Groupchat)) {
            let Some(delivery) = planned.reflection_delivery.take() else {
                let Some(target) = planned.host_frame_target.as_ref() else { return false; };
                let Effect::External(ExternalEffect::Frame(stanza)) = &planned.effect else { return false; };
                let waddle_xmpp::Stanza::Message(message) = stanza.as_ref() else { return false; };
                if message.to.as_ref().and_then(|jid| jid.try_as_full().ok()) != Some(target) {
                    return false;
                }
                let authorized = manifest.recorded_intents().iter().filter_map(super::reflection_dispatch::original_intent)
                    .any(|intent| {
                        let Ok(receipt) = super::receipt_key(&intent) else { return false; };
                        manifest.contains(&receipt, Some(target))
                            && super::receipts::external_receipts(std::slice::from_ref(&ExternalEffect::Frame(stanza.clone())), std::slice::from_ref(&intent))
                                .is_ok_and(|receipts| receipts[0].contains(&receipt))
                    });
                if authorized { planned.suppression = PlanSuppressionPolicy::SenderOnly; }
                return authorized;
            };
            planned.effect = Effect::External(ExternalEffect::Delivery(*delivery));
        }
        let Effect::External(effect) = &planned.effect else { return true; };
        let receipts = child_receipts(effect, manifest);
        let authorized = if let Some(target) = super::recorded::single_target(effect) {
            receipts.iter().any(|receipt| manifest.contains(receipt, Some(target)))
        } else {
            receipts.iter().any(|receipt| manifest.contains(receipt, None))
        };
        if authorized { planned.suppression = PlanSuppressionPolicy::SenderOnly; }
        authorized
    });
}

/// Aggregate route receipts are assembled from per-resource progress, while
/// mutation receipts use exact matching. Both name existing frozen children.
pub(super) fn child_receipts(
    effect: &super::ExternalEffect,
    manifest: &super::FrozenRoomManifest,
) -> Vec<super::EffectReceiptKey> {
    let mut receipts = super::receipts::external_receipts(
        std::slice::from_ref(effect),
        manifest.recorded_intents(),
    )
    .ok()
    .and_then(|mut receipts| receipts.pop())
    .unwrap_or_default();
    for intent in manifest.recorded_intents() {
        if let Ok(Some(progress)) = super::RouteProgress::from_intent(intent, None, Vec::new()) {
            if progress.matches(effect) && !receipts.contains(&progress.receipt) {
                receipts.push(progress.receipt);
            }
        }
    }
    receipts
}

#[cfg(feature = "clustering")]
pub(super) type RoomProof<'a> = Option<crate::ingress_uow::RoomClaimFence<'a>>;
#[cfg(not(feature = "clustering"))]
pub(super) type RoomProof<'a> = std::marker::PhantomData<&'a ()>;

pub(super) async fn assert_room<'a>(
    tx: &mut IngressUowTransaction<'a>,
    submission: &IngressSubmission,
    validate_local: bool,
) -> Result<RoomProof<'a>, IngressUowError> {
    #[cfg(feature = "clustering")]
    {
        use super::identity::IngressStreamIdentity;
        use crate::server::routes::interpret::effects::{
            room::RoomFenceRequirement, RoomExecutionPath,
        };
        let mut proof = None;
        if let IngressStreamIdentity::Relayed {
            room, room_fence, ..
        } = &submission.identity
        {
            validate_room_context(room, room_fence)?;
            proof = Some(
                crate::ingress_uow::ClaimRepository::assert_room_claim(
                    tx,
                    room,
                    &room_fence.owner,
                    room_fence.epoch,
                )
                .await?,
            );
        }
        if !validate_local {
            return Ok(proof);
        }
        if let RoomExecutionPath::Local { room, fence, .. } = &submission.plan.room_execution {
            if matches!(&submission.identity, IngressStreamIdentity::Relayed { room: expected, .. } if expected != room)
            {
                return Err(IngressUowError::ClaimFenceMissing);
            }
            match fence {
                RoomFenceRequirement::Guarded(context) => {
                    validate_room_context(room, context)?;
                    proof = Some(
                        crate::ingress_uow::ClaimRepository::assert_room_claim(
                            tx,
                            room,
                            &context.owner,
                            context.epoch,
                        )
                        .await
                        .map_err(|error| match error {
                            IngressUowError::ClaimFenceMissing => {
                                IngressUowError::RoomGenerationStale
                            }
                            other => other,
                        })?,
                    );
                }
                RoomFenceRequirement::Unfenced
                    if matches!(
                        tx.fencing(),
                        crate::ingress_uow::IngressFencing::Clustered(_)
                    ) =>
                {
                    return Err(IngressUowError::ClaimFenceMissing)
                }
                RoomFenceRequirement::Unfenced => {}
            }
        }
        Ok(proof)
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (tx, submission, validate_local);
        Ok(std::marker::PhantomData)
    }
}
#[cfg(feature = "clustering")]
fn validate_room_context(
    room: &jid::BareJid,
    context: &waddle_xmpp::muc::RoomClaimFenceContext,
) -> Result<(), IngressUowError> {
    if context.entity
        != waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::RoomActor,
            room.to_string(),
        )
    {
        return Err(IngressUowError::RoomGenerationStale);
    }
    Ok(())
}
