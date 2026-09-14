//! Separate this attempt's reflection from repair of its historical occupant copy.
use super::{single_target, RouteProgress};
use crate::{
    ingress_substrate::MessageEnvelope,
    server::routes::interpret::effects::{
        delivery::ExternalDeliveryEffect, Effect, ExternalEffect, IngressPlan,
        PlanSuppressionPolicy, PlannedEffect,
    },
};
use jid::FullJid;
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};

pub(super) fn is_attempt_reflection(
    planned: &PlannedEffect,
    sender: &FullJid,
    intents: &[IngressEffectIntent],
) -> bool {
    let Effect::External(effect) = &planned.effect else {
        return false;
    };
    if single_target(effect) != Some(sender) {
        return false;
    }
    let Some(message) = crate::ingress::receipts::routing::full_delivery(effect, sender) else {
        return false;
    };
    message.type_ == xmpp_parsers::message::MessageType::Groupchat
        && intents.iter().any(|intent| {
            matches!(intent, IngressEffectIntent::RouteMucGroupchat {
                room, route_identity: EffectMessageIdentity::StanzaId(id), ..
            } if id.by == *room && crate::ingress::receipts::routing::message_identity(
                message, &EffectMessageIdentity::StanzaId(id.clone()),
            ))
        })
}

pub(in crate::ingress) fn prepare_attempt_reflections(plan: &mut IngressPlan, sender: &FullJid) {
    for planned in &mut plan.plan {
        if is_attempt_reflection(planned, sender, &plan.intents) {
            // Only validated historical repairs carry the frozen route identity.
            if let Some((identity, _)) = delivery_payload(planned) {
                *identity = None;
            }
            planned.suppression = PlanSuppressionPolicy::Always;
        }
    }
}

pub(super) fn historical_repair(
    reflection: &PlannedEffect,
    envelope: &MessageEnvelope,
    intents: &[IngressEffectIntent],
    progress: &[RouteProgress],
) -> Option<PlannedEffect> {
    let Effect::External(effect) = &reflection.effect else {
        return None;
    };
    let target = single_target(effect)?;
    let message = crate::ingress::receipts::routing::full_delivery(effect, target)?;
    let progress = progress.iter().find(|progress| {
        progress.current_attempt_reflection.as_ref() == Some(target)
            && progress.fanout.contains(target)
            && !progress.completed.contains(target)
            && crate::ingress::receipts::routing::message_identity(
                message,
                &progress.route_identity,
            )
    })?;
    let intent = progress.settle_evidence();
    let source = match crate::ingress::room_canonical::source(envelope, &intent) {
        Ok(source) => source,
        Err(error) => {
            tracing::debug!(%error, "frozen MUC source unavailable; leaving obligation pending");
            return None;
        }
    };
    let mut repair = reflection.clone();
    let (identity, stanza) = delivery_payload(&mut repair)?;
    *identity = Some(progress.route_identity.clone());
    **stanza = waddle_xmpp::Stanza::Message(crate::ingress::room_canonical::occupant_copy_message(
        source, target, intents,
    ));
    repair.suppression = PlanSuppressionPolicy::SenderOnly;
    Some(repair)
}

fn delivery_payload(
    planned: &mut PlannedEffect,
) -> Option<(
    &mut Option<EffectMessageIdentity>,
    &mut Box<waddle_xmpp::Stanza>,
)> {
    match &mut planned.effect {
        Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer {
                route_identity,
                stanza,
                ..
            }
            | ExternalDeliveryEffect::QueueDetached {
                route_identity,
                stanza,
                ..
            }
            | ExternalDeliveryEffect::RelayFullJid {
                route_identity,
                stanza,
                ..
            },
        )) => Some((route_identity, stanza)),
        _ => None,
    }
}
