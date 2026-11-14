use super::submission::IngressSubmission;
use crate::ingress_uow::CanonicalMessageRepository;
#[cfg(feature = "clustering")]
use crate::ingress_uow::ClaimRepository;
use crate::ingress_uow::IngressFencing;
use crate::ingress_uow::IngressUnitOfWork;
#[cfg(feature = "clustering")]
use crate::ingress_uow::RoomClaimFence;
use crate::ingress_uow::{IngressUowError, IngressUowTransaction};

/// Single-node rooms always seal. A clustered room seals in a transaction
/// that carries remote room acceptance through the durable mailbox: the
/// owner then seals in the transaction that records its decision, so the
/// manifest is what was accepted. A clustered transaction below the mailbox
/// epoch records no manifest and its copies
/// settle through the keyed relay.
pub(super) fn new_manifests_enabled(
    uow: &IngressUnitOfWork,
    tx: &IngressUowTransaction<'_>,
) -> bool {
    // Without clustering every room is single-node and the epoch is not
    // consulted.
    #[cfg(not(feature = "clustering"))]
    let _ = tx;
    match uow.fencing() {
        IngressFencing::SingleNode => true,
        #[cfg(feature = "clustering")]
        crate::ingress_uow::IngressFencing::Clustered(_) => {
            let sealed = uow.mailbox_acceptance_active(tx);
            // Unit tests seal clustered manifests without a mailbox lane.
            #[cfg(test)]
            let sealed = sealed || super::commit::commit_hooks::allow_clustered_room_manifests();
            sealed
        }
    }
}

/// Admission coordinates fence the snapshot once. Later revisions do not revoke
/// the children of an already accepted manifest.
pub(super) async fn assert_snapshot_coordinates(
    tx: &mut IngressUowTransaction<'_>,
    submission: &IngressSubmission,
) -> Result<(), IngressUowError> {
    use crate::server::routes::interpret::effects::room::RoomFenceRequirement;
    use crate::server::routes::interpret::effects::RoomExecutionPath;
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
    CanonicalMessageRepository::assert_room_snapshot(tx, room, coordinates).await
}

/// How the host-grant authority decided a fresh room acceptance.
pub(super) enum HostGrantAcceptance {
    /// No enrolled scope manages the room; its existing admission applies.
    Unmanaged,
    /// The sender and these recipients are admitted under the scope's
    /// canonical policy state, whose guard this transaction now holds.
    Admitted {
        audience: Vec<jid::BareJid>,
    },
    Refused(HostGrantRefusal),
}

/// Why a managed room's fresh acceptance is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HostGrantRefusal {
    Denied(crate::permissions::host_grant::AdmissionDenial),
    /// The transaction cannot seal a manifest for the room. A managed room
    /// accepts nothing through an acceptance that freezes no children.
    UnsupportedAcceptance,
}

/// Admit a fresh acceptance for a room an enrolled host-grant scope manages.
///
/// The decision is made from PostgreSQL in this transaction, after its
/// ownership claims and before anything it accepts is recorded: the scope's
/// guard is taken shared and held until the transaction ends, so a revoke
/// either committed before this read or waits for this transaction. The
/// sender needs `send_message` and every recipient the plan would freeze
/// needs `read`.
pub(super) async fn admit_host_grants(
    tx: &mut IngressUowTransaction<'_>,
    submission: &IngressSubmission,
    can_seal: bool,
) -> Result<HostGrantAcceptance, IngressUowError> {
    use crate::ingress_uow::HostGrantAdmissionRepository;
    use crate::permissions::host_grant::{HostGrantAction, RoomAcceptance, RoomAdmission};
    use crate::server::routes::interpret::effects::RoomExecutionPath;
    let RoomExecutionPath::Local {
        room,
        manifest_snapshot,
        ..
    } = &submission.plan.room_execution
    else {
        return Ok(HostGrantAcceptance::Unmanaged);
    };
    // An unmanaged room costs one indexed lookup and nothing else.
    let Some(scope) = HostGrantAdmissionRepository::managing_scope(tx, room).await? else {
        return Ok(HostGrantAcceptance::Unmanaged);
    };
    let audience = frozen_recipients(&submission.plan.intents);
    let admission = HostGrantAdmissionRepository::admit_room_acceptance(
        tx,
        &scope,
        &RoomAcceptance {
            room,
            sender: submission.principal.bare_jid(),
            action: HostGrantAction::SendMessage,
            audience: &audience,
        },
    )
    .await?;
    Ok(match admission {
        RoomAdmission::Denied(denial) => {
            HostGrantAcceptance::Refused(HostGrantRefusal::Denied(denial))
        }
        RoomAdmission::Admitted(_) if !can_seal || manifest_snapshot.is_none() => {
            HostGrantAcceptance::Refused(HostGrantRefusal::UnsupportedAcceptance)
        }
        RoomAdmission::Admitted(_) => {
            super::commit::commit_hooks::after_host_grant_admission().await;
            HostGrantAcceptance::Admitted { audience }
        }
    })
}

/// The bare JID of every recipient a manifest sealed from `intents` would
/// freeze a child for.
fn frozen_recipients(intents: &[waddle_xmpp::ingress::IngressEffectIntent]) -> Vec<jid::BareJid> {
    use waddle_xmpp::ingress::IngressEffectIntent;
    let mut seen = std::collections::HashSet::new();
    let mut recipients: Vec<jid::BareJid> = Vec::new();
    let mut add = |target: &jid::FullJid| {
        let bare = target.to_bare();
        if seen.insert(bare.clone()) {
            recipients.push(bare);
        }
    };
    for intent in intents {
        match intent {
            IngressEffectIntent::RouteDirect { fanout, .. } => fanout.iter().for_each(&mut add),
            IngressEffectIntent::RouteMucGroupchat {
                occupants,
                reflection,
                ..
            } => {
                occupants.iter().for_each(&mut add);
                add(reflection);
            }
            IngressEffectIntent::RouteMucSystemBroadcast { occupants, .. } => {
                occupants.iter().for_each(&mut add);
            }
            IngressEffectIntent::Carbons {
                carbon_recipients, ..
            } => carbon_recipients.iter().for_each(&mut add),
            IngressEffectIntent::RouteOccupantPm { recipient, .. }
            | IngressEffectIntent::CallSignal { recipient, .. }
            | IngressEffectIntent::ErrorReply { recipient, .. } => add(recipient),
            _ => {}
        }
    }
    recipients
}

/// Every child a freshly sealed manifest froze must name a recipient that
/// was admitted. A child outside the admitted audience fails the
/// transaction: nothing is accepted for it.
pub(super) fn assert_admitted_children(
    manifest: &super::FrozenRoomManifest,
    audience: &[jid::BareJid],
) -> Result<(), IngressUowError> {
    let admitted = manifest.children.iter().all(|child| {
        child
            .target
            .as_ref()
            .is_none_or(|target| audience.contains(&target.to_bare()))
    });
    if admitted {
        Ok(())
    } else {
        Err(IngressUowError::HostGrantAudienceUnproven)
    }
}

pub(super) async fn seal(
    tx: &mut IngressUowTransaction<'_>,
    key: waddle_xmpp::ingress::MessageKey,
    submission: &IngressSubmission,
) -> Result<Option<super::FrozenRoomManifest>, IngressUowError> {
    use crate::ingress_uow::{CanonicalMessageRepository, EffectIntentRepository};
    use crate::server::routes::interpret::effects::room::RoomFenceRequirement;
    use crate::server::routes::interpret::effects::RoomExecutionPath;
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
        if let Effect::External(effect @ ExternalEffect::Delivery(_)) = &planned.effect {
            if child_receipts(effect, manifest).iter().any(|receipt| initial_transient.contains(receipt)) {
                let stanza = match effect {
                    ExternalEffect::Delivery(
                        super::effects::delivery::ExternalDeliveryEffect::RouteToPeer { stanza, .. }
                        | super::effects::delivery::ExternalDeliveryEffect::RelayFullJid { stanza, .. }
                        | super::effects::delivery::ExternalDeliveryEffect::QueueDetached { stanza, .. }
                        | super::effects::delivery::ExternalDeliveryEffect::HostOwnedCopy { stanza, .. }
                    ) => stanza.clone(),
                    _ => return false,
                };
                // Local planning resolves the sender through its resource sink;
                // relayed planning already returns a Frame. Both initial copies
                // have the same sender-stream lifetime after policy settlement.
                planned.effect = Effect::External(ExternalEffect::Frame(stanza));
                planned.suppression = PlanSuppressionPolicy::SenderOnly;
                return true;
            }
        }
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
                                .is_ok_and(|receipts| receipts.iter().flatten().any(|saved| saved == &receipt))
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
pub(super) type RoomProof<'a> = Option<RoomClaimFence<'a>>;
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
        use crate::server::routes::interpret::effects::room::RoomFenceRequirement;
        use crate::server::routes::interpret::effects::RoomExecutionPath;
        let mut proof = None;
        if let IngressStreamIdentity::Relayed {
            room, room_fence, ..
        } = &submission.identity
        {
            validate_room_context(room, room_fence)?;
            proof = Some(
                ClaimRepository::assert_room_claim(tx, room, &room_fence.owner, room_fence.epoch)
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
                        ClaimRepository::assert_room_claim(tx, room, &context.owner, context.epoch)
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
