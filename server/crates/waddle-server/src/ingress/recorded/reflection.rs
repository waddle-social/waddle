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
    if reflection_target(effect) != Some(sender) {
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

/// Frame addressing identifies only an attempt reflection, never receipt ownership.
fn reflection_target(effect: &ExternalEffect) -> Option<&FullJid> {
    match effect {
        ExternalEffect::Frame(stanza) => match stanza.as_ref() {
            waddle_xmpp::Stanza::Message(message) => message.to.as_ref()?.try_as_full().ok(),
            _ => None,
        },
        _ => single_target(effect),
    }
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
    let target = reflection_target(effect)?;
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
    if matches!(effect, ExternalEffect::Frame(_)) {
        let delivery = repair.reflection_delivery.take()?;
        repair.effect = Effect::External(ExternalEffect::Delivery(*delivery));
        match &repair.effect {
            Effect::External(effect) if single_target(effect) == Some(target) => {}
            _ => return None,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::{ingress::EntityGeneration, Stanza};
    use xmpp_parsers::message::{Message, MessageType};

    #[test]
    fn frame_reflection_requires_exact_sender_groupchat_and_room_stamp() {
        let sender: FullJid = "sender@example.test/mobile".parse().expect("sender");
        let room: jid::BareJid = "room@muc.example.test".parse().expect("room");
        let id = waddle_xmpp_core::xep0359::StanzaId::new("recorded", room.clone().into());
        let intents = vec![IngressEffectIntent::RouteMucGroupchat {
            room,
            occupants: vec![sender.clone()],
            reflection: sender.clone(),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::StanzaId(id.clone()),
        }];
        let mut message = Message::new(Some(sender.clone().into()));
        message.type_ = MessageType::Groupchat;
        waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &id);
        let recognizes = |message: Message| {
            let effect = ExternalEffect::Frame(Box::new(Stanza::Message(message)));
            assert!(
                single_target(&effect).is_none(),
                "frames never own progress"
            );
            is_attempt_reflection(
                &PlannedEffect::new(Effect::External(effect)),
                &sender,
                &intents,
            )
        };
        assert!(recognizes(message.clone()));
        let mut other = message.clone();
        other.to = Some(
            sender
                .to_bare()
                .with_resource_str("web")
                .expect("sibling")
                .into(),
        );
        assert!(!recognizes(other));
        let mut bare = message.clone();
        bare.to = Some(sender.to_bare().into());
        assert!(!recognizes(bare));
        let mut chat = message.clone();
        chat.type_ = MessageType::Chat;
        assert!(!recognizes(chat));
        message.payloads.clear();
        assert!(!recognizes(message));
    }

    #[test]
    fn frame_historical_repair_preserves_live_detached_and_remote_resolution() {
        use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;
        let sender: FullJid = "sender@example.test/web".parse().expect("original sender");
        let sibling = sender
            .to_bare()
            .with_resource_str("mobile")
            .expect("sibling");
        let room: jid::BareJid = "room@muc.example.test".parse().expect("room");
        let id = waddle_xmpp_core::xep0359::StanzaId::new("frozen-id", room.clone().into());
        let identity = EffectMessageIdentity::StanzaId(id.clone());
        let intent = IngressEffectIntent::RouteMucGroupchat {
            room: room.clone(),
            occupants: vec![sender.clone(), sibling.clone()],
            reflection: sender,
            room_generation: EntityGeneration::INITIAL,
            route_identity: identity.clone(),
        };
        let mut progress = RouteProgress::from_intent(&intent, None, Vec::new())
            .expect("progress")
            .expect("MUC progress");
        progress.current_attempt_reflection = Some(sibling.clone());
        let mut frozen = Message::new(None);
        frozen.type_ = MessageType::Groupchat;
        frozen.from = Some(room.with_resource_str("original").expect("nick").into());
        frozen
            .bodies
            .insert(Default::default(), "frozen content".into());
        waddle_xmpp_core::xep0359::add_stanza_id(&mut frozen, &id);
        waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
            &mut frozen,
            &waddle_xmpp::xep::xep0421::OccupantId("frozen-occupant".into()),
        );
        let envelope = MessageEnvelope::new(frozen.clone());
        let mut fresh = frozen.clone();
        fresh.to = Some(sibling.clone().into());
        fresh.from = Some(room.with_resource_str("mobile").expect("nick").into());
        fresh
            .bodies
            .insert(Default::default(), "fresh content".into());
        let stanza = Box::new(Stanza::Message(fresh.clone()));
        let deliveries = [
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: None,
                jid: sibling.clone(),
                stanza: stanza.clone(),
                kind: PeerDeliveryKind::PeerStanza,
                call_setup: None,
            },
            ExternalDeliveryEffect::QueueDetached {
                route_identity: None,
                bare: sibling.to_bare(),
                resources: vec![sibling.clone()],
                stanza: stanza.clone(),
                call_setup: None,
            },
            ExternalDeliveryEffect::RelayFullJid {
                route_identity: None,
                origin: None,
                target: sibling.clone(),
                stanza: stanza.clone(),
                call_setup: None,
            },
        ];
        for delivery in deliveries {
            let expected_variant = std::mem::discriminant(&delivery);
            let mut reflection =
                PlannedEffect::new(Effect::External(ExternalEffect::Frame(stanza.clone())));
            reflection.reflection_delivery = Some(Box::new(delivery));
            let intents = std::slice::from_ref(&intent);
            let pending = std::slice::from_ref(&progress);
            let mut repair = historical_repair(&reflection, &envelope, intents, pending)
                .expect("frozen sibling repair");
            let Effect::External(ExternalEffect::Delivery(delivery)) = &repair.effect else {
                panic!("repair is delivery")
            };
            assert_eq!(std::mem::discriminant(delivery), expected_variant);
            let Effect::External(effect) = &repair.effect else {
                panic!("external repair")
            };
            assert_eq!(single_target(effect), Some(&sibling));
            assert!(crate::ingress::execute_uow::owns(effect, pending));
            let (restored_identity, restored_stanza) =
                delivery_payload(&mut repair).expect("delivery payload");
            assert_eq!(restored_identity.as_ref(), Some(&identity));
            let mut expected = frozen.clone();
            expected.to = Some(sibling.clone().into());
            let Stanza::Message(restored) = restored_stanza.as_ref() else {
                panic!("message repair")
            };
            assert_eq!(restored, &expected);
            let Effect::External(effect) = &reflection.effect else {
                panic!("reflection")
            };
            assert!(single_target(effect).is_none());
            assert!(!crate::ingress::execute_uow::owns(effect, pending));
            assert!(
                matches!(effect, ExternalEffect::Frame(copy) if matches!(copy.as_ref(), Stanza::Message(message) if message == &fresh))
            );
            let mut completed = progress.clone();
            completed.completed.push(sibling.clone());
            assert!(historical_repair(&reflection, &envelope, intents, &[completed]).is_none());
            assert!(historical_repair(
                &reflection,
                &MessageEnvelope::new(Message::new(None)),
                intents,
                pending
            )
            .is_none());
        }
    }
}
