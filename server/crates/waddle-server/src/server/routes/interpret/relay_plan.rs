//! Owner-side room planning for a committed origin ingress identity.
use super::{
    effects::{
        delivery::ExternalDeliveryEffect, Effect, EffectSink, ExternalEffect, IngressPlan, PlanSink,
    },
    Deps,
};
use jid::{BareJid, FullJid};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

pub(crate) async fn plan_muc_for_relay(
    deps: &Deps<'_>,
    room: BareJid,
    message: Message,
) -> IngressPlan {
    let sink = PlanSink::new();
    let sender = message
        .from
        .as_ref()
        .and_then(|jid| jid.try_as_full().ok())
        .cloned();
    if let Some(sender) = &sender {
        sink.observe_sender(sender);
    }
    sink.observe_message(&message);
    let capture = crate::ingress::IngressEffectCapture::new();
    let planned =
        super::build_plan_deps(deps, &sink).with_ingress_effect_capture(Some(capture.clone()));
    super::room_dispatch::dispatch_to_room(&planned, room, message.clone(), 0).await;
    let mut plan = super::message_plan::finish_plan(&sink, &capture, message, sender.clone());
    if let Some(sender) = sender {
        return_sender_reflection(&mut plan, &sender);
    }
    plan
}

/// The sender's reflection travels in the ordered reply, so origin socket
/// delivery cannot race or duplicate a separate owner-to-sender relay.
fn return_sender_reflection(plan: &mut IngressPlan, sender: &FullJid) {
    for planned in &mut plan.plan {
        let Effect::External(ExternalEffect::Delivery(delivery)) = &planned.effect else {
            continue;
        };
        let stanza = match delivery {
            ExternalDeliveryEffect::RouteToPeer { jid, stanza, .. } if jid == sender => stanza,
            ExternalDeliveryEffect::RelayFullJid { target, stanza, .. } if target == sender => {
                stanza
            }
            _ => continue,
        };
        if matches!(stanza.as_ref(), Stanza::Message(_)) {
            planned.effect = Effect::External(ExternalEffect::Frame(stanza.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::interpret::effects::{PlannedEffect, RoomExecutionPath};

    #[test]
    fn only_sender_reflection_becomes_an_ordered_reply() {
        let sender: FullJid = "sender@example.test/web".parse().expect("sender");
        let peer: FullJid = "peer@example.test/web".parse().expect("peer");
        let room: BareJid = "room@muc.example.test".parse().expect("room");
        let mut reflected = Message::new(Some(sender.clone().into()));
        reflected.from = Some(room.with_resource_str("nick").expect("occupant").into());
        reflected.type_ = xmpp_parsers::message::MessageType::Groupchat;
        let recorded =
            waddle_xmpp_core::xep0359::StanzaId::new("provisional-room-id", room.clone().into());
        waddle_xmpp_core::xep0359::add_stanza_id(&mut reflected, &recorded);
        let mut peer_copy = reflected.clone();
        peer_copy.to = Some(peer.clone().into());
        let mut plan = IngressPlan {
            rejection: None,
            plan: vec![
                PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
                    ExternalDeliveryEffect::RelayFullJid {
                        origin: None,
                        target: sender.clone(),
                        stanza: Box::new(Stanza::Message(reflected.clone())),
                        call_setup: None,
                    },
                ))),
                PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
                    ExternalDeliveryEffect::RelayFullJid {
                        origin: None,
                        target: peer.clone(),
                        stanza: Box::new(Stanza::Message(peer_copy)),
                        call_setup: None,
                    },
                ))),
            ],
            intents: vec![
                waddle_xmpp::ingress::IngressEffectIntent::ArchiveAuthoritative {
                    archive: room.clone(),
                    stanza_id: recorded.clone(),
                    by: room.clone(),
                    archived_at: chrono::Utc::now(),
                },
            ],
            sanitized_message: reflected.clone(),
            error_reply: None,
            room_execution: RoomExecutionPath::None,
        };
        return_sender_reflection(&mut plan, &sender);
        let trusted =
            waddle_xmpp_core::xep0359::StanzaId::new("recorded-room-id", room.clone().into());
        let plan = crate::ingress::restamp::restamp_plan(&plan, &[(room.clone(), trusted.clone())]);
        waddle_xmpp_core::xep0359::remove_stanza_ids_by(&mut reflected, &room.clone().into());
        waddle_xmpp_core::xep0359::add_stanza_id(&mut reflected, &trusted);
        let Effect::External(ExternalEffect::Frame(stanza)) = &plan.plan[0].effect else {
            panic!("sender reflection must travel in ordered reply");
        };
        assert_eq!(stanza.to_element(), Stanza::Message(reflected).to_element());
        assert!(matches!(&plan.plan[1].effect,
            Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { target, .. })) if target == &peer));
    }
}
