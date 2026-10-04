//! Reconstruct generated room invitations from their frozen ingress authority.
use waddle_xmpp::ingress::{IngressEffectIntent, MucInviteLedgerAction, PendingDeliveryMutation};
use xmpp_parsers::message::Message;

use crate::{
    ingress_substrate::MessageEnvelope,
    ingress_uow::IngressUowError,
    server::routes::websocket::handlers::message::{group_dm_invite, muc_direct, muc_invite},
};

/// `None` denotes an ordinary direct route. Generated invitation routes are
/// identified by recorded ledger/grant and fallback obligations, never by the
/// offered payload, so deleting a payload cannot bypass exact reconstruction.
pub(super) fn recorded_message(
    envelope: &MessageEnvelope,
    recorded: &[IngressEffectIntent],
    receipt: &super::EffectReceiptKey,
) -> Result<Option<Message>, IngressUowError> {
    if !recorded.iter().any(|intent| {
        matches!(
            intent,
            IngressEffectIntent::MucInviteLedger { .. }
                | IngressEffectIntent::GroupDmMembershipGrant { .. }
        )
    }) {
        return Ok(None);
    }
    let Some(recipient) = recorded.iter().find_map(|intent| match intent {
        IngressEffectIntent::RouteDirect { recipient, .. }
            if super::receipt_key(intent).ok().as_ref() == Some(receipt) =>
        {
            Some(recipient)
        }
        _ => None,
    }) else {
        return Ok(None);
    };
    if !recorded.iter().any(|intent| {
        matches!(intent,
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient { recipient: target, .. }
        } if target == recipient)
    }) {
        return Ok(None);
    }
    let incoming = envelope.message();
    for intent in recorded {
        let IngressEffectIntent::MucInviteLedger { mutation } = intent else {
            continue;
        };
        let (sender, target) = match mutation.action {
            MucInviteLedgerAction::Recorded => (&mutation.inviter, &mutation.invitee),
            MucInviteLedgerAction::Claimed => (&mutation.invitee, &mutation.inviter),
        };
        if target != recipient {
            continue;
        }
        if incoming.from.as_ref().map(jid::Jid::to_bare).as_ref() != Some(sender)
            || incoming.to.as_ref().map(jid::Jid::to_bare).as_ref() != Some(&mutation.room)
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let message = match mutation.action {
            MucInviteLedgerAction::Recorded => {
                let (invitee, inbound) = muc_invite::mediated_invitee(incoming)
                    .ok_or(IngressUowError::EffectIntentConflict)?;
                if invitee != mutation.invitee {
                    return Err(IngressUowError::EffectIntentConflict);
                }
                match recorded.iter().find_map(|intent| match intent {
                    IngressEffectIntent::GroupDmMembershipGrant { grant }
                        if grant.room == mutation.room
                            && grant.inviter == mutation.inviter
                            && grant.invitee == mutation.invitee =>
                    {
                        Some(grant)
                    }
                    _ => None,
                }) {
                    Some(grant) => {
                        group_dm_invite::recorded_invite_message(incoming, grant, &inbound)
                    }
                    None => muc_invite::mediated_invite_message(
                        incoming,
                        &mutation.room,
                        &mutation.inviter,
                        &mutation.invitee,
                        &inbound,
                    ),
                }
            }
            MucInviteLedgerAction::Claimed => {
                let decline = muc_direct::mediated_decline(incoming)
                    .ok_or(IngressUowError::EffectIntentConflict)?;
                muc_direct::mediated_decline_message(
                    incoming,
                    &mutation.room,
                    &mutation.invitee,
                    &mutation.inviter,
                    decline,
                )
            }
        };
        return Ok(Some(message));
    }
    Err(IngressUowError::EffectIntentConflict)
}
