//! Recover only the notification of an already committed invitation grant.
use super::*;
use crate::server::routes::interpret::effects::{invite::MucUserRoute, PlanSuppressionPolicy};
use waddle_xmpp::{
    ingress::MucInviteLedgerAction,
    pending_delivery::{PendingPayload, PendingRow},
};

pub(super) fn restore(
    plan: &mut IngressPlan,
    input: &RecoveryInput<'_>,
) -> Result<Vec<decision::EffectReceiptKey>, IngressUowError> {
    let mut discarded = Vec::new();
    for ledger in input.recorded {
        let IngressEffectIntent::MucInviteLedger { mutation } = ledger else {
            continue;
        };
        if mutation.action != MucInviteLedgerAction::Recorded {
            continue;
        }
        let Some(recorded_at) = mutation.recorded_at else {
            continue;
        };
        // Receipt evidence authorizes reconstructing a notification, never
        // rerunning a grant or recreating a consumed invitation ledger row.
        let prerequisites_ready = input.recorded.iter().all(|intent| {
            let prerequisite = match intent {
                IngressEffectIntent::MucInviteLedger { mutation: other } => {
                    other.room == mutation.room
                        && other.invitee == mutation.invitee
                        && other.inviter == mutation.inviter
                }
                IngressEffectIntent::MucInviteMembershipGrant { grant } => {
                    grant.room == mutation.room && grant.invitee == mutation.invitee
                }
                IngressEffectIntent::GroupDmMembershipGrant { grant }
                | IngressEffectIntent::GroupDmInviteLedger { grant } => {
                    grant.room == mutation.room && grant.invitee == mutation.invitee
                }
                _ => false,
            };
            !prerequisite || !input.unreceipted.contains(intent)
        });
        if !prerequisites_ready {
            continue;
        }
        for route in input.unreceipted {
            let IngressEffectIntent::RouteDirect {
                prepared: _,
                recipient,
                fanout,
                route_identity,
            } = route
            else {
                continue;
            };
            if recipient != &mutation.invitee {
                continue;
            }
            let Some((pending, row_id)) = input.recorded.iter().find_map(|intent| match intent {
                IngressEffectIntent::PendingDelivery {
                    mutation:
                        PendingDeliveryMutation::Transient {
                            recipient: target,
                            row_id,
                        },
                } if target == recipient => Some((intent, row_id)),
                _ => None,
            }) else {
                continue;
            };
            if !input.unreceipted.contains(pending) {
                continue;
            }
            if input.blocked_recipients.contains(recipient)
                || Utc::now() - recorded_at
                    > crate::server::routes::websocket::muc_invites::INVITE_TTL
            {
                // This is policy resolution, not evidence of socket delivery.
                // Resolve the paired fallback too, without changing membership.
                discarded.push(super::super::receipt_key(route)?);
                discarded.push(super::super::receipt_key(pending)?);
                continue;
            }
            let receipt = super::super::receipt_key(route)?;
            let Some(message) = super::super::invitation_authority::recorded_message(
                input.envelope,
                input.recorded,
                &receipt,
            )?
            else {
                continue;
            };
            let route = MucUserRoute {
                route_identity: Some(route_identity.clone()),
                recipient: recipient.clone(),
                resources: fanout.clone(),
                fallback: PendingRow {
                    id: row_id.clone(),
                    recipient: recipient.clone(),
                    original_receipt_at: input.created_at,
                    payload: PendingPayload::Transient(Box::new(message.clone())),
                    flushed_in_session: None,
                    outbound_sequence: None,
                },
                message: Box::new(message),
                failure: None,
            };
            let effect = if fanout.is_empty() {
                ExternalEffect::QueueOfflineDelivery(route)
            } else {
                ExternalEffect::RouteToPeer(route)
            };
            plan.plan.push(
                PlannedEffect::new(Effect::External(effect))
                    .with_suppression(PlanSuppressionPolicy::Always),
            );
        }
    }
    Ok(discarded)
}
