//! The original room reflection owns delivery authority independent of occupant fanout.
use crate::server::routes::interpret::effects::{
    delivery::ExternalDeliveryEffect, Effect, ExternalEffect, IngressPlan,
};
use waddle_xmpp::ingress::IngressEffectIntent;

/// Freeze the original reflection before reconciliation. A sibling retransmission
/// must not expand the audience: the recorded room route still owns that choice.
pub(super) fn freeze(plan: &mut IngressPlan, recorded: &[IngressEffectIntent]) {
    let reflections: Vec<_> = plan
        .intents
        .iter()
        .filter_map(|intent| {
            let IngressEffectIntent::RouteMucGroupchat { room, .. } = intent else {
                return None;
            };
            let authority = recorded.iter().find(|saved| matches!(saved,
            IngressEffectIntent::RouteMucGroupchat { room: saved_room, .. } if saved_room == room
        )).unwrap_or(intent);
            original_intent(authority)
        })
        .collect();
    for intent in reflections {
        if !plan.intents.contains(&intent) {
            plan.intents.push(intent);
        }
    }
}

pub(super) fn original_intent(room_intent: &IngressEffectIntent) -> Option<IngressEffectIntent> {
    let IngressEffectIntent::RouteMucGroupchat {
        reflection,
        route_identity,
        ..
    } = room_intent
    else {
        return None;
    };
    Some(IngressEffectIntent::RouteDirect {
        recipient: reflection.to_bare(),
        fanout: vec![reflection.clone()],
        route_identity: route_identity.clone(),
    })
}

pub(super) fn classify_progress(
    progress: &mut super::RouteProgress,
    intents: &[IngressEffectIntent],
) {
    progress.reflection_room = intents.iter().find_map(|intent| {
        let IngressEffectIntent::RouteMucGroupchat { room, .. } = intent else {
            return None;
        };
        let reflection = original_intent(intent)?;
        (super::receipt_key(&reflection).ok().as_ref() == Some(&progress.receipt))
            .then(|| room.clone())
    });
}

/// Delivery effects need explicit progress authority; frames retain the generic
/// receipt that is completed only at the actual transport write boundary.
pub(super) fn bind(plan: &mut IngressPlan) {
    for planned in &mut plan.plan {
        let Effect::External(effect) = &mut planned.effect else {
            continue;
        };
        let Some(target) = super::recorded::single_target(effect) else {
            continue;
        };
        let identity = plan
            .intents
            .iter()
            .filter_map(original_intent)
            .find_map(|intent| {
                let IngressEffectIntent::RouteDirect {
                    fanout,
                    route_identity,
                    ..
                } = intent
                else {
                    return None;
                };
                (fanout.as_slice() == [target.clone()]
                    && super::receipts::routing::full_delivery(effect, target).is_some_and(
                        |message| {
                            super::receipts::routing::message_identity(message, &route_identity)
                        },
                    ))
                .then_some(route_identity)
            });
        let Some(identity) = identity else { continue };
        if let ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { route_identity, .. }
            | ExternalDeliveryEffect::RelayFullJid { route_identity, .. }
            | ExternalDeliveryEffect::QueueDetached { route_identity, .. },
        ) = effect
        {
            *route_identity = Some(identity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::interpret::effects::{PlannedEffect, RoomExecutionPath};
    use waddle_xmpp::{
        ingress::{EffectMessageIdentity, EntityGeneration},
        Stanza,
    };
    use xmpp_parsers::message::{Message, MessageType};

    fn room_intent(reflection: &str) -> IngressEffectIntent {
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        IngressEffectIntent::RouteMucGroupchat {
            room: room.clone(),
            occupants: vec![],
            reflection: reflection.parse().expect("reflection"),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::StanzaId(
                waddle_xmpp_core::xep0359::StanzaId::new("frozen", room.into()),
            ),
        }
    }

    #[test]
    fn reflection_dispatch_freezes_original_and_maps_only_its_frame() {
        let original = room_intent("sender@example.com/original");
        let sibling = room_intent("sender@example.com/sibling");
        let mut plan = IngressPlan {
            failure: None,
            rejection: None,
            plan: vec![],
            intents: vec![sibling],
            room_canonical_message: None,
            sanitized_message: Message::new(None),
            error_reply: None,
            room_execution: RoomExecutionPath::None,
        };
        freeze(&mut plan, std::slice::from_ref(&original));
        let reflection = original_intent(&original).expect("original reflection");
        assert_eq!(
            plan.intents
                .iter()
                .filter(|i| matches!(i, IngressEffectIntent::RouteDirect { .. }))
                .collect::<Vec<_>>(),
            vec![&reflection]
        );
        let IngressEffectIntent::RouteMucGroupchat {
            route_identity: EffectMessageIdentity::StanzaId(id),
            ..
        } = original
        else {
            panic!("stamp")
        };
        let frames = ["sender@example.com/original", "sender@example.com/sibling"].map(|target| {
            let mut message = Message::new(Some(target.parse().expect("target")));
            message.type_ = MessageType::Groupchat;
            waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &id);
            ExternalEffect::Frame(Box::new(Stanza::Message(message)))
        });
        let receipts = super::super::receipts::external_receipts(&frames, &[reflection.clone()])
            .expect("receipts");
        assert_eq!(
            receipts[0],
            vec![super::super::receipt_key(&reflection).expect("key")]
        );
        assert!(receipts[1].is_empty());
        plan.plan = frames
            .into_iter()
            .map(|effect| PlannedEffect::new(Effect::External(effect)))
            .collect();
        bind(&mut plan);
        assert!(matches!(
            &plan.plan[0].effect,
            Effect::External(ExternalEffect::Frame(_))
        ));
    }
}
