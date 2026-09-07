use crate::server::routes::interpret::effects::{delivery::ExternalDeliveryEffect, ExternalEffect};
use jid::{BareJid, FullJid};
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, FrozenStanzaError, IngressEffectIntent},
    Stanza,
};
use xmpp_parsers::{
    message::{Message, MessageType},
    stanza_error::StanzaError,
};

pub(super) fn route_receipts(
    external: &[ExternalEffect],
    intent: &IngressEffectIntent,
) -> Option<Vec<usize>> {
    if let IngressEffectIntent::PendingDelivery {
        mutation: waddle_xmpp::ingress::PendingDeliveryMutation::Transient { recipient, row_id },
    } = intent
    {
        return Some(external.iter().enumerate().filter_map(|(index, effect)| {
            matches!(effect, ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route)
                if &route.fallback.id == row_id && &route.fallback.recipient == recipient).then_some(index)
        }).collect());
    }
    if let Some(indices) = early_mutation_receipts(external, intent) {
        return Some(indices);
    }
    match intent {
        IngressEffectIntent::RelayCarbons { owner, exclude, kind } => Some(external.iter().enumerate().filter_map(|(index, effect)| {
            matches!(effect, ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons { owner: actual_owner, exclude: actual_exclude, kind: actual_kind, .. }) if owner == actual_owner && exclude == actual_exclude && kind == actual_kind).then_some(index)
        }).collect()),
        IngressEffectIntent::RouteDirect {
            recipient,
            fanout,
            route_identity,
        } => Some(if fanout.is_empty() {
            external
                .iter()
                .enumerate()
                .filter_map(|(index, effect)| {
                    bare_delivery(effect, recipient, route_identity).then_some(index)
                })
                .collect()
        } else {
            cover_recipients(external, fanout, route_identity)
        }),
        IngressEffectIntent::RouteMucGroupchat {
            occupants,
            reflection,
            route_identity,
            ..
        } => {
            let mut recipients = occupants.clone();
            if !recipients.contains(reflection) {
                recipients.push(reflection.clone());
            }
            Some(cover_recipients(external, &recipients, route_identity))
        }
        IngressEffectIntent::RouteMucSystemBroadcast {
            occupants,
            route_identity,
            ..
        } => Some(cover_recipients(external, occupants, route_identity)),
        IngressEffectIntent::RouteOccupantPm { recipient, sender } => Some(
            external
                .iter()
                .enumerate()
                .filter_map(|(index, effect)| {
                    full_delivery(effect, recipient)
                        .filter(|message| message.from.as_ref() == Some(&sender.clone().into()))
                        .map(|_| index)
                })
                .collect(),
        ),
        IngressEffectIntent::ErrorReply { recipient, error } => Some(
            external
                .iter()
                .enumerate()
                .filter_map(|(index, effect)| {
                    full_delivery(effect, recipient)
                        .filter(|message| {
                            message.type_ == MessageType::Error
                                && message.payloads.iter().any(|element| {
                                    StanzaError::try_from(element.clone())
                                        .ok()
                                        .and_then(|stanza_error| {
                                            FrozenStanzaError::from_xmpp(&stanza_error).ok()
                                        })
                                        .as_ref()
                                        == Some(error)
                                })
                        })
                        .map(|_| index)
                })
                .collect(),
        ),
        _ => None,
    }
}

fn cover_recipients(
    external: &[ExternalEffect],
    recipients: &[FullJid],
    identity: &EffectMessageIdentity,
) -> Vec<usize> {
    let mut indices = Vec::new();
    for recipient in recipients {
        let matches = external
            .iter()
            .enumerate()
            .filter_map(|(index, effect)| {
                full_delivery(effect, recipient)
                    .filter(|message| match effect {
                        ExternalEffect::RouteToPeer(route)
                        | ExternalEffect::QueueOfflineDelivery(route) => {
                            route.route_identity.as_ref() == Some(identity)
                        }
                        _ => message_identity(message, identity),
                    })
                    .map(|_| index)
            })
            .collect::<Vec<_>>();
        // A suppressed destination is still a recorded obligation. Sender-only
        // replay cannot receipt the original fan-out to every occupant.
        if matches.is_empty() {
            return Vec::new();
        }
        for index in matches {
            if !indices.contains(&index) {
                indices.push(index);
            }
        }
    }
    indices
}

fn message(stanza: &Stanza) -> Option<&Message> {
    match stanza {
        Stanza::Message(message) => Some(message),
        _ => None,
    }
}

fn full_delivery<'a>(effect: &'a ExternalEffect, recipient: &FullJid) -> Option<&'a Message> {
    match effect {
        ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route)
            if route.resources.contains(recipient) =>
        {
            Some(&route.message)
        }
        ExternalEffect::Frame(stanza)
        | ExternalEffect::Delivery(ExternalDeliveryEffect::UndeliverableBounce { reply: stanza }) => {
            message(stanza).filter(|message| message.to.as_ref() == Some(&recipient.clone().into()))
        }
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { jid, stanza, .. }
            | ExternalDeliveryEffect::RelayFullJid {
                target: jid,
                stanza,
                ..
            },
        ) if jid == recipient => message(stanza),
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            resources,
            stanza,
            ..
        }) if resources.contains(recipient) => message(stanza),
        _ => None,
    }
}

fn bare_delivery(
    effect: &ExternalEffect,
    recipient: &BareJid,
    identity: &EffectMessageIdentity,
) -> bool {
    match effect {
        ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route)
            if &route.recipient == recipient =>
        {
            route.route_identity.as_ref() == Some(identity)
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayBareJid {
            target, stanza, ..
        }) if target == recipient => {
            message(stanza).is_some_and(|message| message_identity(message, identity))
        }
        _ => false,
    }
}

fn message_identity(message: &Message, identity: &EffectMessageIdentity) -> bool {
    match identity {
        EffectMessageIdentity::StanzaId(id) => {
            waddle_xmpp::xep::extract_stanza_ids(message).contains(id)
        }
        EffectMessageIdentity::OriginId(id) => {
            waddle_xmpp_core::xep0359::extract_origin_id(message).as_ref() == Some(id)
        }
        // Capture ordinals have no wire identity. A typed association from the
        // plan interpreter is required before these can become receipts.
        EffectMessageIdentity::CaptureOrdinal(_) => false,
    }
}

fn early_mutation_receipts(
    external: &[ExternalEffect],
    intent: &IngressEffectIntent,
) -> Option<Vec<usize>> {
    use crate::server::routes::{
        interpret::effects::early::RoomMembershipMutation,
        websocket::handlers::message::muc_invite::InviteLedgerMutation,
    };
    use waddle_xmpp::ingress::MucInviteLedgerAction;
    if !matches!(
        intent,
        IngressEffectIntent::DmPinMutation { .. }
            | IngressEffectIntent::MucInviteMembershipGrant { .. }
            | IngressEffectIntent::GroupDmMembershipGrant { .. }
            | IngressEffectIntent::MucInviteLedger { .. }
            | IngressEffectIntent::GroupDmInviteLedger { .. }
    ) {
        return None;
    }
    Some(external.iter().enumerate().filter_map(|(index, effect)| {
        let matches = match (effect, intent) {
            (ExternalEffect::DmPinMutation(mutation), IngressEffectIntent::DmPinMutation { pair, target_stanza_id, action }) => {
                mutation.pair.low_peer == pair.0 && mutation.pair.high_peer == pair.1 && &mutation.target_stanza_id == target_stanza_id && &mutation.action == action
            }
            (ExternalEffect::RoomMembershipMutation(RoomMembershipMutation::GroupDm(mutation)), IngressEffectIntent::GroupDmMembershipGrant { grant }) => &mutation.grant == grant,
            (ExternalEffect::RoomMembershipMutation(RoomMembershipMutation::Muc(mutation)), IngressEffectIntent::MucInviteMembershipGrant { grant }) => {
                mutation.room == grant.room && mutation.invitee == grant.invitee && external.iter().any(|effect| matches!(effect, ExternalEffect::InviteLedger(InviteLedgerMutation::Record { invite, .. }) if invite.room == grant.room && invite.invitee == grant.invitee && invite.inviter == grant.inviter))
            }
            (ExternalEffect::InviteLedger(mutation), IngressEffectIntent::MucInviteLedger { mutation: recorded }) => {
                let (invite, action, recorded_at) = match mutation {
                    InviteLedgerMutation::Record { invite, recorded_at, .. } => (invite, MucInviteLedgerAction::Recorded, Some(*recorded_at)),
                    InviteLedgerMutation::Claim { invite } => (invite, MucInviteLedgerAction::Claimed, None),
                };
                invite.room == recorded.room && invite.invitee == recorded.invitee && invite.inviter == recorded.inviter && action == recorded.action && recorded_at == recorded.recorded_at
            }
            (ExternalEffect::InviteLedger(InviteLedgerMutation::Record { invite, .. }), IngressEffectIntent::GroupDmInviteLedger { grant }) => {
                invite.room == grant.room && invite.invitee == grant.invitee && invite.inviter == grant.inviter && external.iter().any(|effect| matches!(effect, ExternalEffect::RoomMembershipMutation(RoomMembershipMutation::GroupDm(mutation)) if &mutation.grant == grant))
            }
            _ => false,
        };
        matches.then_some(index)
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::ingress::EntityGeneration;
    use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};

    #[test]
    fn groupchat_receipt_requires_every_occupants_exact_room_stamp() {
        let room: BareJid = "room@muc.example.com".parse().expect("room");
        let sender: FullJid = "sender@example.com/phone".parse().expect("sender");
        let peer: FullJid = "peer@example.com/phone".parse().expect("peer");
        let stamp = StanzaId::new("accepted", room.clone().into());
        let intent = IngressEffectIntent::RouteMucGroupchat {
            room,
            occupants: vec![sender.clone(), peer.clone()],
            reflection: sender.clone(),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::stanza(stamp.clone()),
        };
        let delivery = |recipient: &FullJid, stamp: &StanzaId| {
            let mut message = Message::new(Some(recipient.clone().into()));
            message.type_ = MessageType::Groupchat;
            add_stanza_id(&mut message, stamp);
            ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                jid: recipient.clone(),
                stanza: Box::new(Stanza::Message(message)),
                kind: crate::server::routes::interpret::effects::delivery::PeerDeliveryKind::PeerStanza,
                call_setup: None,
            })
        };
        let wrong_authority = StanzaId::new("accepted", peer.to_bare().into());
        assert_eq!(
            route_receipts(
                &[delivery(&sender, &stamp), delivery(&peer, &wrong_authority)],
                &intent
            ),
            Some(vec![]),
            "matching id text under another assigning authority is not the room delivery"
        );
        assert_eq!(
            route_receipts(&[delivery(&sender, &stamp)], &intent),
            Some(vec![]),
            "sender reflection cannot confirm a suppressed occupant"
        );
        assert_eq!(
            route_receipts(
                &[delivery(&sender, &stamp), delivery(&peer, &stamp)],
                &intent
            ),
            Some(vec![0, 1])
        );
    }
}
