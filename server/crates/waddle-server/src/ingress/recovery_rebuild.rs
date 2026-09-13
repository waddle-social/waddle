//! Pure selection and reconstruction of obligations retained by canonical ingress.
use super::{
    decision::{self, AliasOutcomeClass, IngressDecision, IngressDecisionClass},
    recorded::RouteProgress,
};
use crate::{
    ingress_substrate::MessageEnvelope,
    ingress_uow::{IngressUowError, ReconcileVerdict},
    server::routes::{
        interpret::effects::{
            delivery::ExternalDeliveryEffect, Effect, ExternalEffect, IngressPlan,
            PlanEffectDependency, PlannedEffect, RoomExecutionPath,
        },
        websocket::handlers::message::{dm_pin, muc_direct},
    },
};
use chrono::{DateTime, Utc};
use waddle_xmpp::{
    inbox::storage::{GroupchatNotificationRecovery, GroupchatNotificationRecoveryKey},
    ingress::{
        EffectMessageIdentity, GroupchatNotificationRecoveryAction, IngressEffectIntent,
        IngressEffectKind, MessageKey, NormalizedTarget, NotificationActivityMutation,
        PendingDeliveryMutation,
    },
    Stanza,
};
use xmpp_parsers::message::MessageType;

pub(crate) const RECOVERABLE_KINDS: [IngressEffectKind; 7] = [
    IngressEffectKind::RouteDirect,
    IngressEffectKind::NotificationActivityPreview,
    IngressEffectKind::DmPinMutation,
    IngressEffectKind::MucInviteLedger,
    IngressEffectKind::GroupchatNotificationRecovery,
    IngressEffectKind::PendingDelivery,
    IngressEffectKind::RoomObserver,
];

pub(super) struct RecoveryInput<'a> {
    pub key: MessageKey,
    pub envelope: &'a MessageEnvelope,
    pub created_at: DateTime<Utc>,
    pub recorded: &'a [IngressEffectIntent],
    pub unreceipted: &'a [IngressEffectIntent],
    pub route_progress: Vec<RouteProgress>,
}
pub(super) struct RebuiltRecovery {
    pub decision: IngressDecision,
    pub delegated: Vec<GroupchatNotificationRecovery>,
    pub unrecoverable: Vec<IngressEffectKind>,
    /// Receipts of unreceipted intents no rebuilt effect or delegation can settle.
    pub unsupported_receipts: Vec<decision::EffectReceiptKey>,
}

pub(super) fn rebuild(input: RecoveryInput<'_>) -> Result<RebuiltRecovery, IngressUowError> {
    let mut plan = IngressPlan {
        failure: None,
        rejection: None,
        plan: vec![],
        intents: input.recorded.to_vec(),
        sanitized_message: input.envelope.message().clone(),
        error_reply: None,
        room_execution: RoomExecutionPath::None,
    };
    let mut unrecoverable = Vec::new();
    dm_pin::restore_recorded_dm_pin_effects(&mut plan, input.recorded, input.envelope)?;
    retain_pin_routes(&mut plan, input.unreceipted);
    muc_direct::restore_recorded_muc_decline(
        &mut plan,
        input.recorded,
        input.unreceipted,
        input.envelope,
        input.created_at,
    )?;
    if input
        .recorded
        .iter()
        .any(|i| matches!(i, IngressEffectIntent::PendingDelivery { .. }))
    {
        super::restore_offline::restore_recorded_offline_deliveries(
            &mut plan,
            input.recorded,
            input.unreceipted,
            input.envelope,
            input.created_at,
        );
    }
    if input
        .recorded
        .iter()
        .any(|i| matches!(i, IngressEffectIntent::RoomObserver { .. }))
    {
        match super::recorded::restore_room_observer_envelope(
            &mut plan,
            input.recorded,
            input.envelope,
        ) {
            Ok(()) | Err(IngressUowError::EffectIntentMessageMissing) => {}
            Err(error) => return Err(error),
        }
    }
    restore_direct_routes(&mut plan, &input);
    let delegated = delegated_recoveries(&input);
    let mut external = super::suppression::filter_external_effects(
        &plan,
        &ReconcileVerdict::Consistent,
        &[],
        input.unreceipted,
        &input.route_progress,
    );
    let external_dependencies = super::suppression::external_effect_indices(
        &plan,
        &ReconcileVerdict::Consistent,
        &[],
        input.unreceipted,
        &input.route_progress,
    )
    .into_iter()
    .map(|i| plan.plan[i].dependencies.clone())
    .collect();
    decision::bind_claim_keys(&mut external, input.key);
    let (external_receipts, arm_owned_receipts) =
        decision::assemble_receipts(&external, input.recorded, &input.route_progress)?;
    let mut receipts_pending = Vec::new();
    let mut unsupported_receipts = Vec::new();
    for intent in input.unreceipted {
        let receipt = super::durable::receipt_key(intent)?;
        if !external_receipts
            .iter()
            .any(|receipts| receipts.contains(&receipt))
            && !is_delegated(intent, &delegated)
        {
            if !unrecoverable.contains(&intent.kind()) {
                unrecoverable.push(intent.kind());
            }
            unsupported_receipts.push(receipt.clone());
        }
        receipts_pending.push(receipt);
    }
    let archive_ids = input
        .recorded
        .iter()
        .filter_map(|intent| match intent {
            IngressEffectIntent::ArchiveAuthoritative {
                archive, stanza_id, ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archive, stanza_id, ..
            } => Some((archive.clone(), stanza_id.clone())),
            _ => None,
        })
        .collect();
    Ok(RebuiltRecovery {
        decision: IngressDecision {
            class: IngressDecisionClass::ExistingRepaired,
            message_key: Some(input.key),
            ordinal: None,
            alias: AliasOutcomeClass::Existing,
            verdict: None,
            archive_ids,
            applied_durable: Default::default(),
            external,
            external_dependencies,
            external_receipts,
            arm_owned_receipts,
            route_progress: input.route_progress,
            receipts_pending,
        },
        delegated,
        unrecoverable,
        unsupported_receipts,
    })
}

/// Mutation receipts authorize only the event routes, never replaying mutable pin state.
fn retain_pin_routes(plan: &mut IngressPlan, unreceipted: &[IngressEffectIntent]) {
    let mutations: Vec<_> = plan
        .plan
        .iter()
        .filter_map(|planned| match &planned.effect {
            Effect::External(ExternalEffect::DmPinMutation(mutation)) => Some(mutation.clone()),
            _ => None,
        })
        .collect();
    plan.plan.retain(|planned| match &planned.effect {
        Effect::External(ExternalEffect::DmPinMutation(_)) => false,
        Effect::External(effect)
            if planned.dependencies.iter().any(|dependency| {
                matches!(dependency, PlanEffectDependency::AfterDmPinMutation { .. })
            }) =>
        {
            super::recorded::recorded_route_obligation(unreceipted, effect)
        }
        _ => true,
    });
    for mutation in mutations {
        let pending = unreceipted.iter().any(|intent| matches!(intent,
            IngressEffectIntent::DmPinMutation { pair, target_stanza_id, action }
            if crate::server::routes::websocket::DmPairKey::new(pair.0.clone(), pair.1.clone()) == mutation.pair
                && target_stanza_id == &mutation.target_stanza_id && action == &mutation.action));
        let matches = |dependency: &PlanEffectDependency| {
            matches!(dependency,
            PlanEffectDependency::AfterDmPinMutation { pair, target }
            if pair == &mutation.pair && target == &mutation.target_stanza_id)
        };
        plan.plan.retain_mut(|planned| {
            if pending {
                !planned.dependencies.iter().any(&matches)
            } else {
                planned
                    .dependencies
                    .retain(|dependency| !matches(dependency));
                true
            }
        });
    }
}

fn restore_direct_routes(plan: &mut IngressPlan, input: &RecoveryInput<'_>) {
    let pin_owned = input
        .recorded
        .iter()
        .any(|i| matches!(i, IngressEffectIntent::DmPinMutation { .. }));
    for intent in input.unreceipted {
        let IngressEffectIntent::RouteDirect {
            recipient,
            fanout,
            route_identity,
        } = intent
        else {
            continue;
        };
        if fanout.is_empty()
            || (pin_owned && matches!(route_identity, EffectMessageIdentity::StanzaId(_)))
            || !direct_provenance(input, recipient)
        {
            continue;
        }
        let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            route_identity: Some(route_identity.clone()),
            call_setup: None,
            bare: recipient.clone(),
            resources: fanout.clone(),
            stanza: Box::new(Stanza::Message(super::recorded::delivery_message(
                input.envelope,
                recipient,
                input.recorded,
            ))),
        });
        if plan.plan.iter().any(|planned| {
            matches!(&planned.effect, Effect::External(existing)
            if super::recorded::recorded_route_obligation(std::slice::from_ref(intent), existing))
        }) {
            continue;
        }
        plan.plan.push(PlannedEffect::new(Effect::External(effect)));
    }
}

fn direct_provenance(input: &RecoveryInput<'_>, recipient: &jid::BareJid) -> bool {
    let message = input.envelope.message();
    let (Some(target), Some(sender)) = (message.to.as_ref(), message.from.as_ref()) else {
        return false;
    };
    if !matches!(message.type_, MessageType::Chat | MessageType::Normal)
        || target.to_bare() != *recipient
        || input
            .recorded
            .iter()
            .any(super::restore_offline::specialized_invitation)
        || input.recorded.iter().any(|i| {
            matches!(i, IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Archived { recipient: saved, .. }
                | PendingDeliveryMutation::Transient { recipient: saved, .. }
        } if saved == recipient)
        })
    {
        return false;
    }
    let target = match target.clone().try_into_full() {
        Ok(full) => NormalizedTarget::Full(full),
        Err(bare) => NormalizedTarget::Bare(bare),
    };
    !super::commit::live_recipient_delegated(&target, &sender.to_bare(), input.recorded)
}

fn delegated_recoveries(input: &RecoveryInput<'_>) -> Vec<GroupchatNotificationRecovery> {
    let mut delegated = Vec::new();
    for intent in input.unreceipted {
        let IngressEffectIntent::GroupchatNotificationRecovery { mutation } = intent else {
            continue;
        };
        if !matches!(
            mutation.action,
            GroupchatNotificationRecoveryAction::Completed
                | GroupchatNotificationRecoveryAction::DeferredPolicy
        ) {
            continue;
        }
        let key = GroupchatNotificationRecoveryKey {
            recipient: mutation.recipient.clone(),
            room: mutation.room.clone(),
            thread_id: mutation.thread_id.as_ref().map(|t| t.as_str().to_owned()),
            archive_stanza_id: mutation.archive_stanza_id.clone(),
        };
        if delegated
            .iter()
            .any(|row: &GroupchatNotificationRecovery| row.key == key)
        {
            continue;
        }
        delegated.push(GroupchatNotificationRecovery {
            message_key: input.key,
            key,
            sender_jid: mutation.sender.clone(),
            is_live_occupant: mutation.is_live_occupant,
            room_members_only: mutation.room_members_only,
            sender_can_broadcast_channel_mention: mutation.sender_can_broadcast_channel_mention,
            created_at_ms: mutation.created_at_ms,
        });
    }
    delegated
}

fn is_delegated(intent: &IngressEffectIntent, delegated: &[GroupchatNotificationRecovery]) -> bool {
    match intent {
        IngressEffectIntent::GroupchatNotificationRecovery { mutation } => {
            matches!(
                mutation.action,
                GroupchatNotificationRecoveryAction::Completed
                    | GroupchatNotificationRecoveryAction::DeferredPolicy
            ) && delegated.iter().any(|row| {
                row.key.recipient == mutation.recipient
                    && row.key.room == mutation.room
                    && row.key.archive_stanza_id == mutation.archive_stanza_id
                    && row.key.thread_id.as_deref()
                        == mutation.thread_id.as_ref().map(|t| t.as_str())
            })
        }
        IngressEffectIntent::NotificationActivityPreview {
            owner,
            mutation:
                NotificationActivityMutation::NotificationCandidate {
                    conversation,
                    archive_stanza_id,
                    ..
                },
        } if owner != conversation => delegated.iter().any(|row| {
            &row.key.recipient == owner
                && &row.key.room == conversation
                && &row.key.archive_stanza_id == archive_stanza_id
        }),
        _ => false,
    }
}

#[cfg(test)]
#[path = "recovery_rebuild_tests.rs"]
mod tests;
