//! The plan of a fresh acceptance for a room an enrolled host-grant scope
//! manages, reduced to the audience the scope's policy admits.
//!
//! The planner plans for whoever the room actor and the legacy stores name:
//! seated occupants, durable members, members of the parent Space and
//! mentioned accounts. None of those stores is the room's authority. Before
//! the acceptance transaction records anything, it reads the canonical grant
//! rows under the scope's guard and keeps only the work that names admitted
//! principals:
//!
//! - every intent and every planned effect is classified here, and the match
//!   over each is exhaustive, so a new variant does not compile until it is
//!   classified;
//! - work of the room itself that names no principal (its archive, a
//!   retraction tombstone, link-preview references) is kept;
//! - work that names a principal (a route, an inbox row, a notification, a
//!   push, a recovery item) is kept for an admitted principal and dropped
//!   for anyone else, together with the effects that would carry it out.
//!   An effect is kept or dropped by the principal it names, so every
//!   principal nested in it (a notification candidate's recipient, a
//!   recovery item's recipient, the route a push is recorded under) must be
//!   that same principal;
//! - work this module does not classify as one of the above refuses the
//!   whole stanza. A managed room accepts nothing it cannot account for.
//!
//! The reduced plan is what the rest of the acceptance records, seals and
//! executes, as if the planner had never named the dropped principals.

use std::cell::RefCell;
use std::collections::HashSet;

use jid::{BareJid, Jid};
use waddle_xmpp::ingress::{
    InboxProjectionMutation, IngressEffectIntent, NotificationActivityMutation,
    TombstoneReplayTarget,
};
use waddle_xmpp::tombstone::TombstoneTarget;
use waddle_xmpp::Stanza;

use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
use crate::server::routes::interpret::effects::direct::{
    DurableDirectEffect, ExternalDirectEffect,
};
use crate::server::routes::interpret::effects::room::{
    DurableRoomEffect, ExternalRoomEffect, PlannedGroupchatNotificationRecovery,
};
use crate::server::routes::interpret::effects::{
    DurableEffect, Effect, ExternalEffect, IngressPlan, PlannedEffect, ProjectionRef,
};

/// The plan holds work a managed room does not accept: an intent or an
/// effect that is neither the room's own nor addressed to a principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Unclassified;

/// The room, the sender and the admission predicate a plan is reduced
/// under.
struct Reduction<'a> {
    room: &'a BareJid,
    sender: &'a BareJid,
    admitted: &'a dyn Fn(&BareJid) -> bool,
}

/// Every principal the plan names besides its sender, in first-named order:
/// the candidates admission decides `Read` for.
pub(super) fn named_principals(
    plan: &IngressPlan,
    room: &BareJid,
    sender: &BareJid,
) -> Result<Vec<BareJid>, Unclassified> {
    let named = RefCell::new((HashSet::new(), Vec::new()));
    // Classification asks about every principal it meets; answering yes to
    // each visits them all.
    let record = |principal: &BareJid| {
        let (seen, order) = &mut *named.borrow_mut();
        if seen.insert(principal.clone()) {
            order.push(principal.clone());
        }
        true
    };
    reduce(
        plan,
        &Reduction {
            room,
            sender,
            admitted: &record,
        },
    )?;
    Ok(named.into_inner().1)
}

/// The plan with every intent and effect for a principal outside `audience`
/// removed.
pub(super) fn reduce_to_audience(
    plan: &IngressPlan,
    room: &BareJid,
    sender: &BareJid,
    audience: &[BareJid],
) -> Result<IngressPlan, Unclassified> {
    let audience: HashSet<&BareJid> = audience.iter().collect();
    reduce(
        plan,
        &Reduction {
            room,
            sender,
            admitted: &|principal| audience.contains(principal),
        },
    )
}

/// Whether `intents`, as an acceptance recorded them, name only principals
/// in `audience` and hold nothing a managed room does not accept.
pub(super) fn intents_within_audience(
    intents: &[IngressEffectIntent],
    room: &BareJid,
    sender: &BareJid,
    audience: &[BareJid],
) -> bool {
    let audience: HashSet<&BareJid> = audience.iter().collect();
    let reduction = Reduction {
        room,
        sender,
        admitted: &|principal| audience.contains(principal),
    };
    intents.iter().all(|intent| {
        reduce_intent(intent, &reduction).is_ok_and(|kept| kept.as_ref() == Some(intent))
    })
}

fn reduce(plan: &IngressPlan, reduction: &Reduction<'_>) -> Result<IngressPlan, Unclassified> {
    let mut intents = Vec::with_capacity(plan.intents.len());
    for intent in &plan.intents {
        if let Some(intent) = reduce_intent(intent, reduction)? {
            intents.push(intent);
        }
    }
    // A push names the projection it reports by its position in the plan,
    // so positions are mapped as effects are dropped.
    let mut positions: Vec<Option<usize>> = Vec::with_capacity(plan.plan.len());
    let mut effects: Vec<PlannedEffect> = Vec::with_capacity(plan.plan.len());
    for planned in &plan.plan {
        match reduce_effect(planned, reduction)? {
            Some(kept) => {
                positions.push(Some(effects.len()));
                effects.push(kept);
            }
            None => positions.push(None),
        }
    }
    let mut reduced = Vec::with_capacity(effects.len());
    for mut planned in effects {
        if let Effect::External(ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
            projection,
            ..
        })) = &mut planned.effect
        {
            match positions.get(projection.0).copied().flatten() {
                Some(position) => *projection = ProjectionRef(position),
                // The projection it would report was not kept.
                None => continue,
            }
        }
        reduced.push(planned);
    }
    // Dropping a push after the positions were assigned would shift the
    // effects behind it. A push and its projection name the same owner, so
    // both are kept or both are dropped; anything else is not a plan this
    // reduction understands.
    if reduced.len() != positions.iter().flatten().count() {
        return Err(Unclassified);
    }
    Ok(IngressPlan {
        failure: plan.failure,
        rejection: plan.rejection.clone(),
        plan: reduced,
        intents,
        sanitized_message: plan.sanitized_message.clone(),
        room_canonical_message: plan.room_canonical_message.clone(),
        error_reply: plan.error_reply.clone(),
        room_execution: plan.room_execution.clone(),
    })
}

/// Keep work for `principal` when it is admitted.
fn for_principal<T: Clone>(
    work: &T,
    principal: &BareJid,
    reduction: &Reduction<'_>,
) -> Result<Option<T>, Unclassified> {
    Ok((reduction.admitted)(principal).then(|| work.clone()))
}

/// Keep work of the room itself, which names no principal.
fn of_room<T: Clone>(
    work: &T,
    named: &BareJid,
    reduction: &Reduction<'_>,
) -> Result<Option<T>, Unclassified> {
    if named == reduction.room {
        Ok(Some(work.clone()))
    } else {
        Err(Unclassified)
    }
}

/// One intent reduced to the admitted audience: `Some` is what may be
/// recorded, `None` drops it, and an error refuses the stanza.
fn reduce_intent(
    intent: &IngressEffectIntent,
    reduction: &Reduction<'_>,
) -> Result<Option<IngressEffectIntent>, Unclassified> {
    let room = reduction.room;
    match intent {
        // The room's own archive entry.
        IngressEffectIntent::ArchiveAuthoritative { archive, by, .. } => {
            if by != room {
                return Err(Unclassified);
            }
            of_room(intent, archive, reduction)
        }
        // The fan-out to seated occupants and the sender's reflection.
        IngressEffectIntent::RouteMucGroupchat {
            room: routed,
            occupants,
            reflection,
            room_generation,
            route_identity,
        } => {
            if routed != room || !(reduction.admitted)(&reflection.to_bare()) {
                return Err(Unclassified);
            }
            Ok(Some(IngressEffectIntent::RouteMucGroupchat {
                room: routed.clone(),
                occupants: occupants
                    .iter()
                    .filter(|occupant| (reduction.admitted)(&occupant.to_bare()))
                    .cloned()
                    .collect(),
                reflection: reflection.clone(),
                room_generation: *room_generation,
                route_identity: route_identity.clone(),
            }))
        }
        // A copy for one account's resources: an inbox push.
        IngressEffectIntent::RouteDirect {
            recipient,
            fanout,
            route_identity,
            prepared,
        } => {
            let fanout: Vec<_> = fanout
                .iter()
                .filter(|resource| (reduction.admitted)(&resource.to_bare()))
                .cloned()
                .collect();
            Ok(
                (reduction.admitted)(recipient).then(|| IngressEffectIntent::RouteDirect {
                    recipient: recipient.clone(),
                    fanout,
                    route_identity: route_identity.clone(),
                    prepared: prepared.clone(),
                }),
            )
        }
        IngressEffectIntent::InboxProject { owner, mutation } => {
            let projected = match mutation {
                InboxProjectionMutation::GroupchatChannel {
                    room: projected, ..
                }
                | InboxProjectionMutation::GroupchatThread {
                    room: projected, ..
                }
                | InboxProjectionMutation::GroupchatChannelRead { room: projected }
                | InboxProjectionMutation::GroupchatThreadRead {
                    room: projected, ..
                }
                | InboxProjectionMutation::GroupchatChannelAndThread {
                    room: projected, ..
                } => projected,
                InboxProjectionMutation::Direct { .. }
                | InboxProjectionMutation::DirectCallThreadAnchor { .. }
                | InboxProjectionMutation::DirectCallThreadEnded { .. } => {
                    return Err(Unclassified)
                }
            };
            if projected != room {
                return Err(Unclassified);
            }
            for_principal(intent, owner, reduction)
        }
        IngressEffectIntent::NotificationActivityPreview { owner, mutation } => {
            if activity_conversation(mutation) != room {
                return Err(Unclassified);
            }
            for_principal(intent, owner, reduction)
        }
        IngressEffectIntent::GroupchatNotificationRecovery { mutation } => {
            if mutation.room != *room {
                return Err(Unclassified);
            }
            for_principal(intent, &mutation.recipient, reduction)
        }
        IngressEffectIntent::LinkPreviewMediaRef { mutation } => {
            of_room(intent, &mutation.archive, reduction)
        }
        IngressEffectIntent::RetractionTombstone { mutation } => {
            of_room(intent, &mutation.archive, reduction)
        }
        IngressEffectIntent::TombstoneReplayDeletion { target, .. } => match target {
            TombstoneReplayTarget::Groupchat { room: scrubbed, .. } => {
                of_room(intent, scrubbed, reduction)
            }
            TombstoneReplayTarget::Direct { .. } => Err(Unclassified),
        },
        // The room's refusal of the stanza, addressed to its sender.
        IngressEffectIntent::ErrorReply { recipient, .. } => {
            if recipient.to_bare() == *reduction.sender {
                Ok(Some(intent.clone()))
            } else {
                Err(Unclassified)
            }
        }
        // An extension that observes or receives the room's messages reads
        // them. An extension actor is admitted like any other principal: by
        // a grant, which the first slice issues to accounts only.
        IngressEffectIntent::RoomObserver { room: observed, .. } => {
            if observed != room {
                return Err(Unclassified);
            }
            // The observing plugin holds no grant, whoever asked for it.
            Ok(None)
        }
        IngressEffectIntent::Extension { recipient, .. } => {
            for_principal(intent, recipient, reduction)
        }
        // Everything else is work of another surface (direct messages,
        // carbons, invitations, calls, offline queues), work for a remote
        // room owner, or a change of room state (subject, pins, system
        // messages) that this policy does not decide.
        IngressEffectIntent::DmCallThreadState { .. }
        | IngressEffectIntent::SystemMessageArchive { .. }
        | IngressEffectIntent::RouteMucSystemBroadcast { .. }
        | IngressEffectIntent::RouteOccupantPm { .. }
        | IngressEffectIntent::DispatchToRoomRemote { .. }
        | IngressEffectIntent::RelayCarbons { .. }
        | IngressEffectIntent::Carbons { .. }
        | IngressEffectIntent::PendingDelivery { .. }
        | IngressEffectIntent::DmPinMutation { .. }
        | IngressEffectIntent::MucInviteMembershipGrant { .. }
        | IngressEffectIntent::MucInviteLedger { .. }
        | IngressEffectIntent::GroupDmMembershipGrant { .. }
        | IngressEffectIntent::GroupDmInviteLedger { .. }
        | IngressEffectIntent::RoomSubjectMutation { .. }
        | IngressEffectIntent::CallSignal { .. }
        | IngressEffectIntent::Pin { .. } => Err(Unclassified),
    }
}

/// The conversation a notification activity mutation belongs to.
fn activity_conversation(mutation: &NotificationActivityMutation) -> &BareJid {
    match mutation {
        NotificationActivityMutation::ChatState { conversation, .. }
        | NotificationActivityMutation::ChatStateGone { conversation, .. }
        | NotificationActivityMutation::ReadMarker { conversation, .. }
        | NotificationActivityMutation::OutboundMessage { conversation, .. }
        | NotificationActivityMutation::OfflineDelivery { conversation, .. }
        | NotificationActivityMutation::NotificationCandidate { conversation, .. } => conversation,
    }
}

/// One planned effect reduced to the admitted audience, like
/// [`reduce_intent`].
fn reduce_effect(
    planned: &PlannedEffect,
    reduction: &Reduction<'_>,
) -> Result<Option<PlannedEffect>, Unclassified> {
    let effect = match &planned.effect {
        Effect::Durable(effect) => reduce_durable(effect, reduction)?.map(Effect::Durable),
        Effect::External(effect) => reduce_external(effect, reduction)?.map(Effect::External),
        // Immediate actions run while planning and are never recorded.
        Effect::Immediate(_) => return Err(Unclassified),
    };
    // The delivery a reflection frame stands for, and the nested host
    // consumer of a frame, name principals of their own.
    let reflection = match planned.reflection_delivery.as_deref() {
        Some(delivery) => reduce_delivery(delivery, reduction)?.is_some(),
        None => true,
    };
    let host = planned
        .host_frame_target
        .as_ref()
        .is_none_or(|target| (reduction.admitted)(&target.to_bare()));
    Ok(effect.filter(|_| reflection && host).map(|effect| {
        let mut kept = planned.clone();
        kept.effect = effect;
        kept
    }))
}

fn reduce_durable(
    effect: &DurableEffect,
    reduction: &Reduction<'_>,
) -> Result<Option<DurableEffect>, Unclassified> {
    match effect {
        DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat { room, .. }) => {
            of_room(effect, room, reduction)
        }
        DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
            owner,
            entry,
            recovery,
            ..
        }) => {
            if entry.partner != *reduction.room
                || !recovery_for(recovery.as_ref(), owner, reduction)
            {
                return Err(Unclassified);
            }
            for_principal(effect, owner, reduction)
        }
        DurableEffect::Direct(DurableDirectEffect::MarkInboxRead { owner, channel, .. }) => {
            if channel != reduction.room {
                return Err(Unclassified);
            }
            for_principal(effect, owner, reduction)
        }
        DurableEffect::Direct(DurableDirectEffect::RetractionTombstone { archive, .. }) => {
            of_room(effect, archive, reduction)
        }
        DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect { .. }
            | DurableDirectEffect::ProjectInbox { .. }
            | DurableDirectEffect::DmCallThreadProjection { .. },
        ) => Err(Unclassified),
    }
}

fn reduce_external(
    effect: &ExternalEffect,
    reduction: &Reduction<'_>,
) -> Result<Option<ExternalEffect>, Unclassified> {
    match effect {
        // A frame is written to the stream it names: the sender's own
        // reflection or the room's reply to the sender.
        ExternalEffect::Frame(stanza) => match stanza_recipient(stanza) {
            Some(recipient) => for_principal(effect, &recipient.to_bare(), reduction),
            None => Err(Unclassified),
        },
        ExternalEffect::Direct(direct) => match direct {
            ExternalDirectEffect::NotificationActivity { owner, mutation } => {
                if activity_conversation(mutation) != reduction.room {
                    return Err(Unclassified);
                }
                for_principal(effect, owner, reduction)
            }
            ExternalDirectEffect::PushInboxUpdate { owner, receipt, .. } => {
                // The receipt is the route the push is recorded under: a
                // copy for the owner's own resources.
                let receipt_for_owner = receipt.as_deref().is_none_or(|receipt| {
                    matches!(
                        receipt,
                        IngressEffectIntent::RouteDirect { recipient, fanout, .. }
                            if recipient == owner
                                && fanout.iter().all(|resource| resource.to_bare() == *owner)
                    )
                });
                if !receipt_for_owner {
                    return Err(Unclassified);
                }
                for_principal(effect, owner, reduction)
            }
            ExternalDirectEffect::LinkPreviewRefs { mutations }
            | ExternalDirectEffect::ClearLinkPreviewRefs { mutations } => {
                if mutations
                    .iter()
                    .all(|mutation| mutation.archive == *reduction.room)
                {
                    Ok(Some(effect.clone()))
                } else {
                    Err(Unclassified)
                }
            }
            ExternalDirectEffect::ScrubReplayForTombstone { target } => match target {
                TombstoneTarget::Groupchat { room, .. } => of_room(effect, room, reduction),
                TombstoneTarget::Direct { .. } => Err(Unclassified),
            },
            ExternalDirectEffect::DmCallThreadState { .. } => Err(Unclassified),
        },
        ExternalEffect::Room(room_effect) => match room_effect {
            ExternalRoomEffect::NotificationCandidate {
                owner,
                room,
                candidate,
                recovery,
                ..
            } => {
                let candidate_for_owner = candidate.as_deref().is_none_or(|candidate| {
                    candidate.recipient_bare_jid() == owner
                        && candidate.conversation_jid() == reduction.room
                });
                if room != reduction.room
                    || !candidate_for_owner
                    || !recovery_for(recovery.as_ref(), owner, reduction)
                {
                    return Err(Unclassified);
                }
                for_principal(effect, owner, reduction)
            }
            // See `IngressEffectIntent::RoomObserver`.
            ExternalRoomEffect::ObserveRoomMessage { room, .. } => {
                if room != reduction.room {
                    return Err(Unclassified);
                }
                Ok(None)
            }
            ExternalRoomEffect::ArchiveAfterPin { .. }
            | ExternalRoomEffect::RoomActorMutation { .. } => Err(Unclassified),
            #[cfg(feature = "clustering")]
            ExternalRoomEffect::RelayMucProxy { .. } => Err(Unclassified),
        },
        ExternalEffect::Delivery(delivery) => {
            Ok(reduce_delivery(delivery, reduction)?.map(ExternalEffect::Delivery))
        }
        ExternalEffect::RouteToPeer(_)
        | ExternalEffect::QueueOfflineDelivery(_)
        | ExternalEffect::RoomMembershipMutation(_)
        | ExternalEffect::InviteLedger(_)
        | ExternalEffect::DmPinMutation(_) => Err(Unclassified),
    }
}

/// Whether a notification recovery item an effect carries, if any, is the
/// item of `owner` in the reduction's room. An effect is kept or dropped by
/// its owner, so a nested principal that is not the owner would escape the
/// audience; such an effect is not classified.
fn recovery_for(
    recovery: Option<&PlannedGroupchatNotificationRecovery>,
    owner: &BareJid,
    reduction: &Reduction<'_>,
) -> bool {
    recovery.is_none_or(|recovery| {
        recovery.key.recipient == *owner && recovery.key.room == *reduction.room
    })
}

fn reduce_delivery(
    delivery: &ExternalDeliveryEffect,
    reduction: &Reduction<'_>,
) -> Result<Option<ExternalDeliveryEffect>, Unclassified> {
    match delivery {
        ExternalDeliveryEffect::HostOwnedCopy { target, .. } => {
            for_principal(delivery, &target.to_bare(), reduction)
        }
        ExternalDeliveryEffect::RouteToPeer { jid, .. } => {
            for_principal(delivery, &jid.to_bare(), reduction)
        }
        ExternalDeliveryEffect::RelayFullJid { target, .. } => {
            for_principal(delivery, &target.to_bare(), reduction)
        }
        ExternalDeliveryEffect::RelayBareJid { target, .. } => {
            for_principal(delivery, target, reduction)
        }
        ExternalDeliveryEffect::QueueDetached {
            route_identity,
            call_setup,
            bare,
            resources,
            stanza,
        } => {
            let resources: Vec<_> = resources
                .iter()
                .filter(|resource| (reduction.admitted)(&resource.to_bare()))
                .cloned()
                .collect();
            Ok(
                (reduction.admitted)(bare).then(|| ExternalDeliveryEffect::QueueDetached {
                    route_identity: route_identity.clone(),
                    call_setup: call_setup.clone(),
                    bare: bare.clone(),
                    resources,
                    stanza: stanza.clone(),
                }),
            )
        }
        // The room's bounce of the stanza, addressed to its sender.
        ExternalDeliveryEffect::UndeliverableBounce { reply } => match stanza_recipient(reply) {
            Some(recipient) if recipient.to_bare() == *reduction.sender => {
                Ok(Some(delivery.clone()))
            }
            _ => Err(Unclassified),
        },
        ExternalDeliveryEffect::SfuRevokeToken { .. }
        | ExternalDeliveryEffect::RelayCarbons { .. }
        | ExternalDeliveryEffect::Carbons { .. }
        | ExternalDeliveryEffect::QueueOfflineDelivery { .. } => Err(Unclassified),
    }
}

/// The address a message stanza is written to. A room acceptance plans no
/// other kind of stanza.
fn stanza_recipient(stanza: &Stanza) -> Option<&Jid> {
    match stanza {
        Stanza::Message(message) => message.to.as_ref(),
        Stanza::Presence(_) | Stanza::Iq(_) => None,
    }
}
