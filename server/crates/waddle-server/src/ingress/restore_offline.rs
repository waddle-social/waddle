//! Rebuild ordinary offline work from the first committed message and obligations.
use chrono::{DateTime, Utc};
use waddle_xmpp::{
    ingress::{
        IngressEffectIntent, NotificationActivityMutation, NotificationCandidateOutcome,
        PendingDeliveryMutation,
    },
    pending_delivery::{PendingPayload, PendingRow, PendingRowId},
};

use crate::{
    ingress_substrate::MessageEnvelope,
    server::routes::interpret::effects::{
        delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
        Effect, ExternalEffect, IngressPlan, PlannedEffect,
    },
};

pub(super) fn restore_recorded_offline_deliveries(
    plan: &mut IngressPlan,
    recorded: &[IngressEffectIntent],
    unreceipted: &[IngressEffectIntent],
    envelope: &MessageEnvelope,
    created_at: DateTime<Utc>,
) -> bool {
    if recorded.iter().any(specialized_invitation) {
        return false;
    }
    let ordinary: Vec<_> = recorded
        .iter()
        .filter_map(|intent| match intent {
            IngressEffectIntent::PendingDelivery { mutation }
                if !specialized_fallback(plan, pending_identity(mutation).1) =>
            {
                Some((intent, mutation))
            }
            _ => None,
        })
        .collect();
    // A new live snapshot must not add delivery authority for a message whose
    // original acceptance chose the offline queue. Keep separately recorded
    // routes (such as inbox pushes) intact.
    plan.intents.retain(|intent| {
        !matches!(intent, IngressEffectIntent::RouteDirect { recipient, .. }
            if ordinary.iter().any(|(_, mutation)| pending_identity(mutation).0 == recipient))
            || recorded.contains(intent)
    });
    // Today's payload/storage policy cannot invent notification obligations
    // for an originally transient or suppressed delivery.
    plan.intents.retain(|intent| {
        !matches!(intent, IngressEffectIntent::NotificationActivityPreview {
            owner, mutation: NotificationActivityMutation::NotificationCandidate { conversation, .. }
                | NotificationActivityMutation::OfflineDelivery { conversation, .. },
        } if owner == conversation && ordinary.iter().any(|(_, mutation)| pending_identity(mutation).0 == owner))
            || recorded.contains(intent)
    });
    let mut reconstructed = false;
    for (intent, mutation) in ordinary {
        let (recipient, row_id) = pending_identity(mutation);
        plan.intents.retain(|fresh| {
            !matches!(fresh,
            IngressEffectIntent::PendingDelivery { mutation: fresh }
                if pending_identity(fresh).0 == recipient)
                || recorded.contains(fresh)
        });
        if !plan.intents.contains(intent) {
            plan.intents.push(intent.clone());
        }
        let notifications: Vec<_> = unreceipted
            .iter()
            .filter(|intent| correlated_notification(intent, mutation))
            .cloned()
            .collect();
        if !unreceipted.contains(intent) && notifications.is_empty() {
            continue;
        }
        let payload = match mutation {
            PendingDeliveryMutation::Archived {
                archive_stanza_id, ..
            } => PendingPayload::Archived(archive_stanza_id.clone()),
            PendingDeliveryMutation::Transient { .. } => {
                PendingPayload::Transient(Box::new(envelope.message().clone()))
            }
        };
        let prepared_notification = match notifications.iter().find_map(|intent| {
            if let IngressEffectIntent::NotificationActivityPreview {
                mutation:
                    NotificationActivityMutation::NotificationCandidate {
                        archive_stanza_id,
                        outcome: NotificationCandidateOutcome::Inserted,
                        ..
                    },
                ..
            } = intent
            {
                Some(archive_stanza_id)
            } else {
                None
            }
        }) {
            Some(archive_stanza_id) => {
                let Some(sender) = envelope.message().from.as_ref() else {
                    continue;
                };
                let Ok(candidate) = crate::notification_outbox::direct_candidate_from_envelope(
                    envelope.message(),
                    recipient,
                    sender,
                    archive_stanza_id,
                ) else {
                    continue;
                };
                PreparedOfflineNotification::Prepared(Box::new(candidate))
            }
            None => PreparedOfflineNotification::Suppressed,
        };
        let row = PendingRow {
            id: row_id.clone(),
            recipient: recipient.clone(),
            original_receipt_at: created_at,
            payload,
            flushed_in_session: None,
            outbound_sequence: None,
        };
        let fresh = plan.plan.iter_mut().find(|planned| matches!(&planned.effect,
            Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery { row: fresh, .. }))
                if fresh.recipient == row.recipient && same_payload_kind(&fresh.payload, &row.payload)));
        let effect = Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueOfflineDelivery {
                prepared_notification,
                row,
                original_message: Box::new(envelope.message().clone()),
            },
        ));
        if let Some(fresh) = fresh {
            // Keep projection/dependency indices and captured prerequisites stable.
            fresh.effect = effect;
        } else {
            plan.plan.push(PlannedEffect::new(effect));
        }
        for notification in notifications {
            if !plan.intents.contains(&notification) {
                plan.intents.push(notification);
            }
        }
        reconstructed = true;
    }
    reconstructed
}

fn specialized_invitation(intent: &IngressEffectIntent) -> bool {
    matches!(
        intent,
        IngressEffectIntent::MucInviteLedger { .. }
            | IngressEffectIntent::GroupDmInviteLedger { .. }
            | IngressEffectIntent::MucInviteMembershipGrant { .. }
            | IngressEffectIntent::GroupDmMembershipGrant { .. }
    )
}

fn specialized_fallback(plan: &IngressPlan, row_id: &PendingRowId) -> bool {
    plan.plan.iter().any(|planned| matches!(&planned.effect,
        Effect::External(ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route))
            if &route.fallback.id == row_id))
}

fn pending_identity(mutation: &PendingDeliveryMutation) -> (&jid::BareJid, &PendingRowId) {
    match mutation {
        PendingDeliveryMutation::Archived {
            recipient, row_id, ..
        }
        | PendingDeliveryMutation::Transient { recipient, row_id } => (recipient, row_id),
    }
}

fn same_payload_kind(left: &PendingPayload, right: &PendingPayload) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

pub(super) fn correlated_notification(
    intent: &IngressEffectIntent,
    pending: &PendingDeliveryMutation,
) -> bool {
    let PendingDeliveryMutation::Archived {
        recipient,
        archive_stanza_id: archived,
        ..
    } = pending
    else {
        return false;
    };
    matches!(intent,
        IngressEffectIntent::NotificationActivityPreview {
            owner, mutation: NotificationActivityMutation::NotificationCandidate { conversation, archive_stanza_id, .. }
                | NotificationActivityMutation::OfflineDelivery { conversation, archive_stanza_id },
        } if owner == recipient && conversation == recipient && archive_stanza_id == archived)
}

pub(super) fn pending_obligation(
    effect: &ExternalEffect,
    unreceipted: &[IngressEffectIntent],
) -> bool {
    let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery { row, .. }) = effect
    else {
        return false;
    };
    let mutation = match &row.payload {
        PendingPayload::Archived(archive_stanza_id) => PendingDeliveryMutation::Archived {
            recipient: row.recipient.clone(),
            row_id: row.id.clone(),
            archive_stanza_id: archive_stanza_id.clone(),
        },
        PendingPayload::Transient(_) => PendingDeliveryMutation::Transient {
            recipient: row.recipient.clone(),
            row_id: row.id.clone(),
        },
    };
    unreceipted.iter().any(|intent| matches!(intent,
        IngressEffectIntent::PendingDelivery { mutation: saved } if pending_identity(saved).1 == &row.id)
        || correlated_notification(intent, &mutation))
}

/// Reject fresh delivery shapes that the original offline acceptance did not own.
pub(super) fn in_recorded_offline_audience(plan: &IngressPlan, effect: &ExternalEffect) -> bool {
    match effect {
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery { row, .. }) => {
            plan.intents.iter().any(|intent| matches!(intent,
                IngressEffectIntent::PendingDelivery { mutation }
                    if pending_identity(mutation) == (&row.recipient, &row.id)
                        && match (mutation, &row.payload) {
                            (PendingDeliveryMutation::Archived { archive_stanza_id, .. }, PendingPayload::Archived(id)) => archive_stanza_id == id,
                            (PendingDeliveryMutation::Transient { .. }, PendingPayload::Transient(_)) => true,
                            _ => false,
                        }))
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. }
            | ExternalDeliveryEffect::RelayFullJid { target: jid, .. }) => {
            live_route_allowed(plan, effect, &jid.to_bare())
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { bare, .. }) => {
            live_route_allowed(plan, effect, bare)
        }
        _ => true,
    }
}

fn live_route_allowed(
    plan: &IngressPlan,
    effect: &ExternalEffect,
    recipient: &jid::BareJid,
) -> bool {
    !plan.intents.iter().any(|intent| matches!(intent,
        IngressEffectIntent::PendingDelivery { mutation } if pending_identity(mutation).0 == recipient))
        || super::recorded::recorded_route_obligation(&plan.intents, effect)
}

#[cfg(test)]
#[path = "restore_offline_tests.rs"]
mod tests;
