//! Reconstruct generated room invitations from their frozen ingress authority.
use waddle_xmpp::ingress::{IngressEffectIntent, MucInviteLedgerAction, PendingDeliveryMutation};
use xmpp_parsers::message::Message;

use crate::ingress_substrate::MessageEnvelope;
use crate::ingress_uow::IngressUowError;
use crate::server::routes::websocket::handlers::message::{
    group_dm_invite, muc_direct, muc_invite,
};

/// `None` denotes an ordinary direct route. Generated invitation routes are
/// identified by recorded ledger/grant and fallback obligations, never by the
/// offered payload, so deleting a payload cannot bypass exact reconstruction.
pub(super) fn recorded_message(
    envelope: &MessageEnvelope,
    recorded: &[IngressEffectIntent],
    receipt: &super::EffectReceiptKey,
    occupant_id_secret: Option<&waddle_xmpp::xep::xep0421::OccupantIdSecret>,
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
        // Every generated invitation carries the room's XEP-0421 id, so it
        // cannot be rebuilt before the secret is bound.
        let occupant_id_secret =
            occupant_id_secret.ok_or(IngressUowError::OccupantIdSecretUnbound)?;
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
                        occupant_id_secret,
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
                    occupant_id_secret,
                )
            }
        };
        return Ok(Some(message));
    }
    Err(IngressUowError::EffectIntentConflict)
}

#[cfg(test)]
mod tests {
    use super::recorded_message;
    use crate::ingress_substrate::MessageEnvelope;
    use crate::ingress_uow::IngressUowError;
    use crate::server::routes::websocket::handlers::message::{muc_direct, muc_invite};
    use waddle_xmpp::ingress::{
        EffectMessageIdentity, IngressEffectIntent, MucInviteLedgerAction, MucInviteLedgerMutation,
        PendingDeliveryMutation,
    };
    use waddle_xmpp::pending_delivery::PendingRowId;
    use waddle_xmpp::xep::xep0421::{generate_room_occupant_id, OccupantIdSecret};
    use xmpp_parsers::message::{Id, Message, MessageType};

    fn bare(jid: &str) -> jid::BareJid {
        jid.parse().expect("bare jid")
    }

    fn secret() -> OccupantIdSecret {
        OccupantIdSecret::try_new(vec![b'r'; 32]).expect("secret")
    }

    /// The recorded intents and the inbound stanza of one generated relay.
    fn relay(
        action: MucInviteLedgerAction,
    ) -> (
        MessageEnvelope,
        Vec<IngressEffectIntent>,
        minidom::Element,
        jid::BareJid,
        jid::BareJid,
        jid::BareJid,
    ) {
        let room = bare("coven@muc.example.com");
        let (sender, recipient) = (bare("romeo@example.com"), bare("juliet@example.com"));
        let tag = match action {
            MucInviteLedgerAction::Recorded => "invite",
            MucInviteLedgerAction::Claimed => "decline",
        };
        let inbound = minidom::Element::builder(tag, waddle_xmpp::muc::presence::NS_MUC_USER)
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                recipient.to_string(),
            )
            .build();
        let mut incoming = Message::new(Some(room.clone().into()));
        incoming.from = Some(sender.clone().into());
        incoming.id = Some(Id("relay-1".into()));
        incoming.type_ = MessageType::Normal;
        incoming.payloads =
            vec![
                minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .append(inbound.clone())
                    .build(),
            ];
        let (inviter, invitee) = match action {
            MucInviteLedgerAction::Recorded => (sender.clone(), recipient.clone()),
            MucInviteLedgerAction::Claimed => (recipient.clone(), sender.clone()),
        };
        let intents = vec![
            IngressEffectIntent::RouteDirect {
                prepared: None,
                recipient: recipient.clone(),
                fanout: vec![],
                route_identity: EffectMessageIdentity::capture_ordinal(1),
            },
            IngressEffectIntent::PendingDelivery {
                mutation: PendingDeliveryMutation::Transient {
                    recipient: recipient.clone(),
                    row_id: PendingRowId::fresh(),
                },
            },
            IngressEffectIntent::MucInviteLedger {
                mutation: MucInviteLedgerMutation {
                    room: room.clone(),
                    inviter,
                    invitee,
                    action,
                    recorded_at: Some(chrono::Utc::now()),
                },
            },
        ];
        (
            MessageEnvelope::new(incoming),
            intents,
            inbound,
            room,
            sender,
            recipient,
        )
    }

    fn occupant_ids(message: &Message) -> Vec<String> {
        message
            .payloads
            .iter()
            .filter(|payload| payload.is("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID))
            .filter_map(|payload| payload.attr("id").map(str::to_owned))
            .collect()
    }

    #[test]
    fn reconstructed_invitation_equals_the_live_relay_with_the_room_occupant_id() {
        let secret = secret();
        let (envelope, intents, inbound, room, sender, recipient) =
            relay(MucInviteLedgerAction::Recorded);
        let live = muc_invite::mediated_invite_message(
            envelope.message(),
            &room,
            &sender,
            &recipient,
            &inbound,
            &secret,
        );
        let receipt = crate::ingress::receipt_key(&intents[0]).expect("receipt");
        let rebuilt = recorded_message(&envelope, &intents, &receipt, Some(&secret))
            .expect("reconstruct")
            .expect("generated invitation");
        assert_eq!(rebuilt, live);
        assert_eq!(
            occupant_ids(&live),
            vec![generate_room_occupant_id(&room, &secret)
                .as_str()
                .to_owned()]
        );
    }

    #[test]
    fn reconstructed_decline_equals_the_live_relay_with_the_room_occupant_id() {
        let secret = secret();
        let (envelope, intents, inbound, room, sender, recipient) =
            relay(MucInviteLedgerAction::Claimed);
        let live = muc_direct::mediated_decline_message(
            envelope.message(),
            &room,
            &sender,
            &recipient,
            &inbound,
            &secret,
        );
        let receipt = crate::ingress::receipt_key(&intents[0]).expect("receipt");
        let rebuilt = recorded_message(&envelope, &intents, &receipt, Some(&secret))
            .expect("reconstruct")
            .expect("generated decline");
        assert_eq!(rebuilt, live);
        assert_eq!(occupant_ids(&live).len(), 1);
    }

    #[test]
    fn reconstruction_refuses_to_run_before_the_secret_is_bound() {
        let (envelope, intents, ..) = relay(MucInviteLedgerAction::Recorded);
        let receipt = crate::ingress::receipt_key(&intents[0]).expect("receipt");
        assert!(matches!(
            recorded_message(&envelope, &intents, &receipt, None),
            Err(IngressUowError::OccupantIdSecretUnbound)
        ));
    }
}
