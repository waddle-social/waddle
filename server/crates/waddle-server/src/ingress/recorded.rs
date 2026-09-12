//! Apply the payload-complete policy decisions retained by reconciliation.
#[cfg(test)]
#[path = "recorded/delivery_replay_tests.rs"]
mod delivery_replay_tests;
#[cfg(test)]
#[path = "recorded/preview_replay_tests.rs"]
mod preview_replay_tests;
#[cfg(test)]
mod tests;

use crate::server::routes::interpret::effects::{
    direct::{DurableDirectEffect, ExternalDirectEffect},
    room::{
        DurableRoomEffect, ExternalRoomEffect, PlannedGroupchatNotificationRecovery,
        RoomActorMutation,
    },
    DurableEffect, Effect, ExternalEffect, IngressPlan, PlanEffectDependency,
};
use waddle_xmpp::ingress::{
    GroupchatNotificationRecoveryAction, GroupchatNotificationRecoveryMutation,
    InboxProjectionMutation, IngressEffectIntent, RoomPinMutation,
};

/// Frozen fanout authority and the resources already durably completed.
#[derive(Clone, Debug)]
pub struct RouteProgress {
    pub receipt: super::decision::EffectReceiptKey,
    pub recipient: jid::BareJid,
    pub fanout: Vec<jid::FullJid>,
    pub route_identity: waddle_xmpp::ingress::EffectMessageIdentity,
    pub completed: Vec<jid::FullJid>,
}

impl RouteProgress {
    pub(super) fn matches(&self, effect: &ExternalEffect) -> bool {
        external_route_recipient(effect).as_ref() == Some(&self.recipient)
            && external_route_identity(effect) == Some(&self.route_identity)
    }

    pub(super) fn remaining(&self, effect: &ExternalEffect) -> Vec<jid::FullJid> {
        external_route_targets(effect)
            .into_iter()
            .filter(|target| self.fanout.contains(target) && !self.completed.contains(target))
            .collect()
    }
}

/// Restore direct delivery copies from canonical content and recorded archive
/// authority. Synthetic pin, invitation and room copies have their own restorers.
pub fn restore_delivery_payloads(
    plan: &mut IngressPlan,
    envelope: &crate::ingress_substrate::MessageEnvelope,
) {
    use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
    for planned in &mut plan.plan {
        if planned
            .dependencies
            .iter()
            .any(|dependency| matches!(dependency, PlanEffectDependency::AfterDmPinMutation { .. }))
        {
            continue;
        }
        let Effect::External(effect) = &mut planned.effect else {
            continue;
        };
        let Some(recipient) = external_route_recipient(effect) else {
            continue;
        };
        if !plan.intents.iter().any(|intent| {
            matches!(intent,
            IngressEffectIntent::RouteDirect { recipient: saved, route_identity, .. }
                if saved == &recipient && external_route_identity(effect) == Some(route_identity))
        }) {
            continue;
        }
        if let ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueDetached { stanza, .. }
            | ExternalDeliveryEffect::RouteToPeer { stanza, .. },
        ) = effect
        {
            if matches!(stanza.as_ref(), waddle_xmpp::Stanza::Message(_)) {
                **stanza = waddle_xmpp::Stanza::Message(delivery_message(
                    envelope,
                    &recipient,
                    &plan.intents,
                ));
            }
        }
    }
}

fn delivery_message(
    envelope: &crate::ingress_substrate::MessageEnvelope,
    recipient: &jid::BareJid,
    intents: &[IngressEffectIntent],
) -> xmpp_parsers::message::Message {
    let mut message = envelope.message().clone();
    // A bare-target message remains bare even when today's route is live;
    // full-target messages retain the canonical requested resource.
    if message
        .to
        .as_ref()
        .is_none_or(|target| target.to_bare() != *recipient)
    {
        message.to = Some(recipient.clone().into());
    }
    for intent in intents {
        if let IngressEffectIntent::ArchiveAuthoritative {
            archive, stanza_id, ..
        } = intent
        {
            if archive == recipient {
                waddle_xmpp_core::xep0359::add_stanza_id(&mut message, stanza_id);
            }
        }
    }
    message
}

/// Reconciliation preserves recorded payloads when the policy or audience
/// changes. Both application and receipt identity must use those same payloads.
/// Recorded-only obligations stay pending unless the plan contains their work.
pub fn apply_recorded_intents(plan: &IngressPlan, recorded: &[IngressEffectIntent]) -> IngressPlan {
    let mut result = plan.clone();
    for original in &plan.intents {
        let Some(authoritative) = recorded_match(recorded, original) else {
            continue;
        };
        for effect in &mut result.plan {
            if let (
                IngressEffectIntent::Pin {
                    room,
                    mutation: original_mutation,
                },
                IngressEffectIntent::Pin { mutation, .. },
            ) = (original, authoritative)
            {
                for dependency in &mut effect.dependencies {
                    if let crate::server::routes::interpret::effects::PlanEffectDependency::AfterRoomPin { room: dependent_room, change } = dependency {
                        if dependent_room == room && *change == pin_change(original_mutation) {
                            *change = pin_change(mutation);
                        }
                    }
                }
            }
            if let (
                IngressEffectIntent::RoomSubjectMutation {
                    room,
                    state: original_state,
                },
                IngressEffectIntent::RoomSubjectMutation {
                    state: saved_state, ..
                },
            ) = (original, authoritative)
            {
                for dependency in &mut effect.dependencies {
                    if let crate::server::routes::interpret::effects::PlanEffectDependency::AfterRoomSubject { room: dependent_room, state } = dependency {
                        if dependent_room == room && state == original_state {
                            *state = saved_state.clone();
                        }
                    }
                }
            }
            apply_effect(&mut effect.effect, original, authoritative);
        }
    }
    for intent in &mut result.intents {
        if let Some(authoritative) = recorded_match(recorded, intent) {
            *intent = authoritative.clone();
        }
    }
    result.intents.retain(|intent| recorded.contains(intent));
    // Preview effects batch several independently reconciled mutations. Drop
    // rejected work inside each batch, without shifting projection indices in
    // the surrounding plan when the entire batch becomes empty.
    for effect in &mut result.plan {
        if let Effect::External(ExternalEffect::Direct(
            ExternalDirectEffect::LinkPreviewRefs { mutations }
            | ExternalDirectEffect::ClearLinkPreviewRefs { mutations },
        )) = &mut effect.effect
        {
            mutations.retain(|mutation| {
                result.intents.iter().any(|intent| {
                    matches!(intent, IngressEffectIntent::LinkPreviewMediaRef { mutation: saved } if saved == mutation)
                })
            });
        }
    }
    restore_dm_call_state(&mut result, recorded);
    result
}

/// Rebuild every frozen state transition, including offers absent from today's plan.
fn restore_dm_call_state(plan: &mut IngressPlan, recorded: &[IngressEffectIntent]) {
    let mut states = recorded
        .iter()
        .filter_map(|intent| match intent {
            IngressEffectIntent::DmCallThreadState { sequence, state } => {
                Some((*sequence, state, intent))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    states.sort_by_key(|(sequence, _, _)| *sequence);
    // Preserve plan indices used by projection references: replace matching
    // effects in place and append only transitions missing from the retry plan.
    for (_, state, intent) in states {
        let existing = plan.plan.iter_mut().find(|planned| matches!(&planned.effect,
            Effect::External(ExternalEffect::Direct(ExternalDirectEffect::DmCallThreadState { receipt: Some(receipt), .. }))
                if receipt.authority_key() == intent.authority_key()));
        let effect = Effect::External(ExternalEffect::Direct(
            ExternalDirectEffect::DmCallThreadState {
                state: state.clone(),
                receipt: Some(Box::new(intent.clone())),
            },
        ));
        if let Some(existing) = existing {
            existing.effect = effect;
        } else {
            plan.plan.push(
                crate::server::routes::interpret::effects::PlannedEffect::new(effect)
                    .with_suppression(
                        crate::server::routes::interpret::effects::PlanSuppressionPolicy::Always,
                    ),
            );
        }
        if !plan.intents.contains(intent) {
            plan.intents.push(intent.clone());
        }
    }
}

/// Restore observer payloads from the canonical envelope before receipt matching.
pub fn restore_room_observer_envelope(
    plan: &mut IngressPlan,
    recorded: &[IngressEffectIntent],
    envelope: &crate::ingress_substrate::MessageEnvelope,
) -> Result<(), crate::ingress_uow::IngressUowError> {
    for effect in &mut plan.plan {
        if let Effect::External(ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage {
            message,
            error_request,
            ..
        })) = &mut effect.effect
        {
            **message = envelope.message().clone();
            **error_request = envelope
                .room_observer_request()
                .ok_or(crate::ingress_uow::IngressUowError::EffectIntentMessageMissing)?;
        }
    }
    let error_request = envelope
        .room_observer_request()
        .ok_or(crate::ingress_uow::IngressUowError::EffectIntentMessageMissing)?;
    for intent in recorded {
        let IngressEffectIntent::RoomObserver {
            room,
            requester,
            sender,
            plugin,
        } = intent
        else {
            continue;
        };
        let exists = plan.plan.iter().any(|planned| {
            matches!(
                &planned.effect,
                Effect::External(ExternalEffect::Room(
                    ExternalRoomEffect::ObserveRoomMessage {
                        plugin: planned_plugin,
                        ..
                    }
                )) if planned_plugin == plugin
            )
        });
        if !exists {
            let message = envelope.message().clone();
            let dependencies =
                crate::server::routes::interpret::effects::room::message_dependencies(
                    room, &message,
                );
            let mut planned =
                crate::server::routes::interpret::effects::PlannedEffect::new(Effect::External(
                    ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage {
                        room: room.clone(),
                        plugin: plugin.clone(),
                        message: Box::new(message),
                        requester: requester.clone(),
                        sender: sender.clone(),
                        error_request: Box::new(error_request.clone()),
                    }),
                ))
                .with_suppression(
                    crate::server::routes::interpret::effects::PlanSuppressionPolicy::Always,
                );
            planned.dependencies = dependencies;
            plan.plan.push(planned);
        }
        if !plan.intents.contains(intent) {
            plan.intents.push(intent.clone());
        }
    }
    Ok(())
}

/// Rebuild the conditional subject bounce from the committed message and error,
/// so a retry never substitutes today's provisional payload or error text.
pub fn restore_subject_rejection_replies(
    plan: &mut IngressPlan,
    envelope: &crate::ingress_substrate::MessageEnvelope,
) -> Result<(), crate::ingress_uow::IngressUowError> {
    use crate::ingress_uow::IngressUowError;
    for effect in &mut plan.plan {
        let Effect::External(ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
            room,
            mutation:
                RoomActorMutation::SetSubject {
                    subject,
                    rejection_reply,
                    ..
                },
        })) = &mut effect.effect
        else {
            continue;
        };
        let mut errors = plan.intents.iter().filter_map(|intent| match intent {
            IngressEffectIntent::ErrorReply { recipient, error }
                if recipient.to_bare() == subject.setter =>
            {
                Some((recipient, error))
            }
            _ => None,
        });
        let (recipient, error) = errors.next().ok_or(IngressUowError::EffectIntentConflict)?;
        if errors.next().is_some() {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let mut reply = envelope.message().clone();
        reply.type_ = xmpp_parsers::message::MessageType::Error;
        reply.from = Some(room.clone().into());
        reply.to = Some(recipient.clone().into());
        reply.payloads.push(error.to_xmpp().into());
        **rejection_reply = reply;
    }
    Ok(())
}

pub fn room_observer_envelope(
    plan: &IngressPlan,
) -> Option<crate::ingress_substrate::MessageEnvelope> {
    if !plan
        .intents
        .iter()
        .any(|intent| matches!(intent, IngressEffectIntent::RoomObserver { .. }))
    {
        return None;
    }
    plan.plan.iter().find_map(|effect| {
        if let Effect::External(ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage {
            message,
            error_request,
            ..
        })) = &effect.effect
        {
            Some(
                crate::ingress_substrate::MessageEnvelope::with_room_observer(
                    (**message).clone(),
                    (**error_request).clone(),
                ),
            )
        } else {
            None
        }
    })
}

fn recorded_match<'a>(
    recorded: &'a [IngressEffectIntent],
    planned: &IngressEffectIntent,
) -> Option<&'a IngressEffectIntent> {
    recorded
        .iter()
        .find(|row| *row == planned)
        .or_else(|| {
            recorded
                .iter()
                .find(|row| row.semantic_key() == planned.semantic_key())
        })
        .or_else(|| {
            recorded.iter().find(|row| {
                row.authority_key() == planned.authority_key() && same_mutation_shape(row, planned)
            })
        })
}

/// Member notification work belongs to the same frozen audience as its inbox
/// projection, even when its usual duplicate policy allows idempotent replay.
pub(super) fn external_in_recorded_audience(plan: &IngressPlan, effect: &ExternalEffect) -> bool {
    use waddle_xmpp::ingress::EffectAuthorityKey;
    if let ExternalEffect::Direct(ExternalDirectEffect::DmCallThreadState { receipt, .. }) = effect
    {
        return receipt
            .as_deref()
            .is_some_and(|intent| plan.intents.contains(intent));
    }

    if let ExternalEffect::Direct(
        ExternalDirectEffect::LinkPreviewRefs { mutations }
        | ExternalDirectEffect::ClearLinkPreviewRefs { mutations },
    ) = effect
    {
        return !mutations.is_empty();
    }
    if let ExternalEffect::Delivery(
        crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect::Carbons {
            owner,
            recipient,
            exclude,
            kind,
            ..
        },
    ) = effect
    {
        return plan.intents.iter().any(|intent| {
            matches!(intent,
            IngressEffectIntent::Carbons { carbon_recipients, excluded_source, kind: recorded_kind }
                if carbon_recipients.contains(recipient) && owner == &excluded_source.to_bare()
                    && exclude.iter().find(|source| &source.to_bare() == owner) == Some(excluded_source) && kind == recorded_kind)
        });
    }
    if let ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage { room, plugin, .. }) =
        effect
    {
        return plan.intents.iter().any(|intent| {
            matches!(
                intent,
                IngressEffectIntent::RoomObserver {
                    room: recorded_room,
                    plugin: recorded_plugin,
                    ..
                } if room == recorded_room && plugin == recorded_plugin
            )
        });
    }
    let (owner, room) = match effect {
        ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate { owner, room, .. }) => {
            (owner.clone(), room.clone())
        }
        ExternalEffect::Direct(ExternalDirectEffect::NotificationActivity { owner, mutation }) => {
            return plan
                .intents
                .contains(&IngressEffectIntent::NotificationActivityPreview {
                    owner: owner.clone(),
                    mutation: mutation.clone(),
                });
        }
        _ => return true,
    };
    if !plan.intents.iter().any(|intent| matches!(intent,
        IngressEffectIntent::RouteMucGroupchat { room: recorded_room, .. } if recorded_room == &room))
    {
        return true;
    }
    plan.intents.iter().any(|intent| {
        matches!(intent.authority_key(),
        EffectAuthorityKey::Inbox { owner: recorded_owner, partner, .. }
            if recorded_owner == owner && partner == room)
    })
}

/// Capture identity a delivery effect discharges, when it carries one.
pub(super) fn external_route_identity(
    effect: &ExternalEffect,
) -> Option<&waddle_xmpp::ingress::EffectMessageIdentity> {
    use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
    match effect {
        ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route) => {
            route.route_identity.as_ref()
        }
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { route_identity, .. }
            | ExternalDeliveryEffect::QueueDetached { route_identity, .. }
            | ExternalDeliveryEffect::RelayFullJid { route_identity, .. },
        ) => route_identity.as_ref(),
        _ => None,
    }
}

/// Resources this delivery effect targets.
fn external_route_targets(effect: &ExternalEffect) -> Vec<jid::FullJid> {
    use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
    match effect {
        ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route) => {
            route.resources.clone()
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. }) => {
            vec![jid.clone()]
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. }) => {
            resources.clone()
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { target, .. }) => {
            vec![target.clone()]
        }
        _ => Vec::new(),
    }
}

fn external_route_recipient(effect: &ExternalEffect) -> Option<jid::BareJid> {
    use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
    match effect {
        ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route) => {
            Some(route.recipient.clone())
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. }) => {
            Some(jid.to_bare())
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { bare, .. }) => {
            Some(bare.clone())
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { target, .. }) => {
            Some(target.to_bare())
        }
        _ => None,
    }
}

/// Whether one of these recorded obligations carries both this delivery's
/// capture identity and its exact audience. A reconnect must not repair a
/// fan-out the committed obligation never covered.
pub(super) fn recorded_route_obligation(
    intents: &[IngressEffectIntent],
    effect: &ExternalEffect,
) -> bool {
    let Some(identity) = external_route_identity(effect) else {
        return false;
    };
    let targets = external_route_targets(effect);
    let recipient = external_route_recipient(effect);
    intents.iter().any(|intent| match intent {
        IngressEffectIntent::RouteDirect {
            fanout,
            route_identity,
            recipient: recorded_recipient,
        } => {
            recipient.as_ref() == Some(recorded_recipient)
                && route_identity == identity
                && targets.iter().all(|target| fanout.contains(target))
        }
        IngressEffectIntent::RouteMucGroupchat {
            occupants,
            route_identity,
            ..
        }
        | IngressEffectIntent::RouteMucSystemBroadcast {
            occupants,
            route_identity,
            ..
        } => route_identity == identity && targets.iter().all(|target| occupants.contains(target)),
        _ => false,
    })
}

fn same_mutation_shape(recorded: &IngressEffectIntent, planned: &IngressEffectIntent) -> bool {
    match (recorded, planned) {
        (
            IngressEffectIntent::Carbons {
                carbon_recipients: saved,
                ..
            },
            IngressEffectIntent::Carbons {
                carbon_recipients: offered,
                ..
            },
        ) => saved == offered,
        (
            IngressEffectIntent::MucInviteLedger { mutation: saved },
            IngressEffectIntent::MucInviteLedger { mutation: offered },
        ) => saved.action == offered.action,
        (
            IngressEffectIntent::DmPinMutation { action: saved, .. },
            IngressEffectIntent::DmPinMutation {
                action: offered, ..
            },
        ) => std::mem::discriminant(saved) == std::mem::discriminant(offered),
        (
            IngressEffectIntent::NotificationActivityPreview {
                mutation: saved, ..
            },
            IngressEffectIntent::NotificationActivityPreview {
                mutation: offered, ..
            },
        ) => std::mem::discriminant(saved) == std::mem::discriminant(offered),
        (
            IngressEffectIntent::InboxProject {
                mutation: saved, ..
            },
            IngressEffectIntent::InboxProject {
                mutation: offered, ..
            },
        ) => std::mem::discriminant(saved) == std::mem::discriminant(offered),
        (
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: saved },
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: offered },
        ) => saved.action == offered.action,
        (
            IngressEffectIntent::LinkPreviewMediaRef { mutation: saved },
            IngressEffectIntent::LinkPreviewMediaRef { mutation: offered },
        ) => saved.state == offered.state,
        (
            IngressEffectIntent::RoomObserver { plugin: saved, .. },
            IngressEffectIntent::RoomObserver {
                plugin: offered, ..
            },
        ) => saved == offered,
        _ => true,
    }
}

fn apply_effect(
    effect: &mut Effect,
    original: &IngressEffectIntent,
    recorded: &IngressEffectIntent,
) {
    match effect {
        Effect::Durable(effect) => apply_durable(effect, original, recorded),
        Effect::External(effect) => apply_external(effect, original, recorded),
        Effect::Immediate(_) => {}
    }
}

fn apply_durable(
    effect: &mut DurableEffect,
    original: &IngressEffectIntent,
    recorded: &IngressEffectIntent,
) {
    match (effect, original, recorded) {
        (
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                owner,
                entry,
                is_recipient,
                ..
            }),
            IngressEffectIntent::InboxProject {
                owner: old_owner, ..
            },
            IngressEffectIntent::InboxProject {
                mutation:
                    InboxProjectionMutation::GroupchatChannel {
                        room,
                        increment_unread,
                    }
                    | InboxProjectionMutation::GroupchatChannelAndThread {
                        room,
                        increment_unread,
                        ..
                    },
                ..
            },
        ) if owner == old_owner && &entry.partner == room => *is_recipient = *increment_unread,
        (
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                entry,
                archive_stanza_id,
                ..
            }),
            IngressEffectIntent::ArchiveAuthoritative {
                archive, stanza_id, ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archive, stanza_id, ..
            },
            IngressEffectIntent::ArchiveAuthoritative { archived_at, .. }
            | IngressEffectIntent::SystemMessageArchive { archived_at, .. },
        ) if &entry.partner == archive && archive_stanza_id == stanza_id => {
            entry.last_updated = archived_at.timestamp()
        }
        (
            DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
                owner,
                entry,
                increment_unread,
            }),
            IngressEffectIntent::InboxProject {
                owner: old_owner,
                mutation: InboxProjectionMutation::Direct { entry: old, .. },
            },
            IngressEffectIntent::InboxProject {
                mutation:
                    InboxProjectionMutation::Direct {
                        entry: saved,
                        increment_unread: saved_unread,
                    },
                ..
            },
        ) if owner == old_owner && entry.as_ref() == old => {
            **entry = saved.clone();
            *increment_unread = *saved_unread;
        }
        (
            DurableEffect::Direct(DurableDirectEffect::DmCallThreadProjection { owner, mutation }),
            IngressEffectIntent::InboxProject {
                owner: old_owner,
                mutation: old,
            },
            IngressEffectIntent::InboxProject {
                mutation: saved, ..
            },
        ) if owner == old_owner && mutation.as_ref() == old => {
            **mutation = saved.clone();
        }
        (
            DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                archive,
                message,
                archive_expectation,
                ..
            })
            | DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                room: archive,
                message,
                archive_expectation,
                ..
            }),
            IngressEffectIntent::ArchiveAuthoritative {
                archive: old_archive,
                stanza_id,
                ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archive: old_archive,
                stanza_id,
                ..
            },
            IngressEffectIntent::ArchiveAuthoritative {
                archived_at,
                ordinal,
                ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archived_at,
                ordinal,
                ..
            },
        ) if archive == old_archive && message.id == stanza_id.id => {
            message.timestamp = *archived_at;
            *archive_expectation = waddle_xmpp::mam::ArchiveExpectation::Existing {
                stanza_id: stanza_id.clone(),
                archived_at: *archived_at,
                ordinal: *ordinal,
            };
        }
        (
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                recovery: Some(recovery),
                ..
            }),
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: old },
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: saved },
        ) if old.action == GroupchatNotificationRecoveryAction::Recorded
            && recovery_matches(recovery, old) =>
        {
            apply_recovery(recovery, saved);
        }
        _ => {}
    }
}

fn apply_external(
    effect: &mut ExternalEffect,
    original: &IngressEffectIntent,
    recorded: &IngressEffectIntent,
) {
    use crate::server::routes::interpret::effects::early::RoomMembershipMutation;
    use crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerMutation;
    match (effect, original, recorded) {
        (
            ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage { room, plugin, requester, sender, .. }),
            IngressEffectIntent::RoomObserver { room: original_room, plugin: original_plugin, .. },
            IngressEffectIntent::RoomObserver { room: saved_room, plugin: saved_plugin, requester: saved_requester, sender: saved_sender },
        ) if room == original_room && plugin == original_plugin => {
            *room = saved_room.clone();
            *plugin = saved_plugin.clone();
            *requester = saved_requester.clone();
            *sender = saved_sender.clone();
        }

        (
            ExternalEffect::Delivery(crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect::RelayCarbons { owner, exclude, kind, .. }),
            IngressEffectIntent::RelayCarbons { owner: original_owner, kind: original_kind, .. },
            IngressEffectIntent::RelayCarbons { owner: recorded_owner, exclude: recorded_exclude, kind: recorded_kind },
        ) if owner == original_owner && kind == original_kind => {
            *owner = recorded_owner.clone();
            *exclude = recorded_exclude.clone();
            *kind = *recorded_kind;
        }

        (
            ExternalEffect::Room(ExternalRoomEffect::ArchiveAfterPin { room, message, .. }),
            IngressEffectIntent::SystemMessageArchive {
                archive, stanza_id, ..
            } | IngressEffectIntent::ArchiveAuthoritative {
                archive, stanza_id, ..
            },
            IngressEffectIntent::SystemMessageArchive { archived_at, .. }
            | IngressEffectIntent::ArchiveAuthoritative { archived_at, .. },
        ) if room == archive && message.id == stanza_id.id => {
            // The Phase C archive transaction derives its expectation (including
            // the recorded ordinal) from recorded authority; only the receive
            // time travels on the effect.
            message.timestamp = *archived_at;
        }
        (
            ExternalEffect::InviteLedger(InviteLedgerMutation::Record {
                invite,
                recorded_at,
                ..
            }),
            IngressEffectIntent::MucInviteLedger { mutation: old },
            IngressEffectIntent::MucInviteLedger { mutation: saved },
        ) if invite.room == old.room
            && invite.invitee == old.invitee
            && invite.inviter == old.inviter
            && old.action == waddle_xmpp::ingress::MucInviteLedgerAction::Recorded
            && saved.action == old.action =>
        {
            if let Some(saved_at) = saved.recorded_at {
                *recorded_at = saved_at;
                invite.inviter = saved.inviter.clone();
            }
        }
        (
            ExternalEffect::DmPinMutation(mutation),
            IngressEffectIntent::DmPinMutation {
                pair,
                target_stanza_id,
                action: old,
            },
            IngressEffectIntent::DmPinMutation { action: saved, .. },
        ) if mutation.pair.low_peer == pair.0
            && mutation.pair.high_peer == pair.1
            && mutation.target_stanza_id == *target_stanza_id
            && mutation.action == *old =>
        {
            mutation.action = saved.clone();
        }
        (
            ExternalEffect::RoomMembershipMutation(RoomMembershipMutation::GroupDm(mutation)),
            IngressEffectIntent::GroupDmMembershipGrant { grant: old },
            IngressEffectIntent::GroupDmMembershipGrant { grant: saved },
        ) if mutation.grant == *old => mutation.grant = saved.clone(),
        (
            ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate {
                recovery: Some(recovery),
                ..
            }),
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: old },
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: saved },
        ) if old.action == GroupchatNotificationRecoveryAction::Completed
            && recovery_matches(recovery, old) =>
        {
            apply_recovery(recovery, saved);
        }
        (
            ExternalEffect::Direct(ExternalDirectEffect::NotificationActivity { owner, mutation }),
            IngressEffectIntent::NotificationActivityPreview {
                owner: old_owner,
                mutation: old,
            },
            IngressEffectIntent::NotificationActivityPreview {
                mutation: saved, ..
            },
        ) if owner == old_owner && mutation == old => *mutation = saved.clone(),
        (
            ExternalEffect::Direct(
                ExternalDirectEffect::LinkPreviewRefs { mutations }
                | ExternalDirectEffect::ClearLinkPreviewRefs { mutations },
            ),
            IngressEffectIntent::LinkPreviewMediaRef { mutation: old },
            IngressEffectIntent::LinkPreviewMediaRef { mutation: saved },
        ) => {
            for mutation in mutations {
                if mutation == old {
                    *mutation = saved.clone();
                }
            }
        }
        (
            ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
                room,
                mutation: RoomActorMutation::SetSubject { subject, .. },
            }),
            IngressEffectIntent::RoomSubjectMutation { room: old_room, .. },
            IngressEffectIntent::RoomSubjectMutation { state, .. },
        ) if room == old_room => *subject = state.clone(),
        (
            ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
                room,
                mutation: RoomActorMutation::ApplyPin { change, .. },
            }),
            IngressEffectIntent::Pin { room: old_room, .. },
            IngressEffectIntent::Pin { mutation, .. },
        ) if room == old_room => {
            *change = pin_change(mutation);
        }
        _ => {}
    }
}

fn recovery_matches(
    recovery: &PlannedGroupchatNotificationRecovery,
    mutation: &GroupchatNotificationRecoveryMutation,
) -> bool {
    recovery.key.recipient == mutation.recipient
        && recovery.key.room == mutation.room
        && recovery.key.archive_stanza_id == mutation.archive_stanza_id
        && recovery.key.thread_id.as_deref() == mutation.thread_id.as_ref().map(|id| id.as_str())
}

fn apply_recovery(
    recovery: &mut PlannedGroupchatNotificationRecovery,
    saved: &GroupchatNotificationRecoveryMutation,
) {
    recovery.sender_jid = saved.sender.clone();
    recovery.is_live_occupant = saved.is_live_occupant;
    recovery.room_members_only = saved.room_members_only;
    recovery.sender_can_broadcast_channel_mention = saved.sender_can_broadcast_channel_mention;
    recovery.created_at_ms = saved.created_at_ms;
}

fn pin_change(mutation: &RoomPinMutation) -> waddle_xmpp::muc::pin::PinStateChange {
    match mutation {
        RoomPinMutation::Pin { entry } => waddle_xmpp::muc::pin::PinStateChange::Pin(entry.clone()),
        RoomPinMutation::Unpin { target_stanza_id } => {
            waddle_xmpp::muc::pin::PinStateChange::Unpin {
                target_stanza_id: target_stanza_id.clone(),
            }
        }
    }
}
