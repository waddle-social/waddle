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
    match intent {
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
                    .filter(|message| message_identity(message, identity))
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
