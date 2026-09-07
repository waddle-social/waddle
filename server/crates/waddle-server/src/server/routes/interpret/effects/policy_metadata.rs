//! Duplicate policy is computed from the frozen sender and typed delivery shape.
use super::{
    delivery::ExternalDeliveryEffect, direct::ExternalDirectEffect, room::ExternalRoomEffect,
    Effect, ExternalEffect, PlanSuppressionPolicy, PlannedEffect,
};
use jid::FullJid;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

fn subject_rebroadcast(stanza: &Stanza) -> bool {
    matches!(stanza, Stanza::Message(message)
        if waddle_xmpp::muc::is_groupchat_subject_change_message(message))
}

pub(super) fn apply_policy(effect: &mut PlannedEffect, sender: Option<&FullJid>) {
    effect.tombstone_suppression = if effect.tombstone_suppression == PlanSuppressionPolicy::Always
        || matches!(
            &effect.effect,
            Effect::Durable(super::DurableEffect::Direct(
                super::direct::DurableDirectEffect::RetractionTombstone { .. }
            )) | Effect::External(ExternalEffect::Direct(
                ExternalDirectEffect::ScrubReplayForTombstone { .. }
            )) | Effect::External(ExternalEffect::Direct(
                ExternalDirectEffect::ClearLinkPreviewRefs { .. }
            )) | Effect::Immediate(_)
        ) {
        PlanSuppressionPolicy::Always
    } else {
        PlanSuppressionPolicy::TombstoneSwallowed
    };
    let Effect::External(external) = &effect.effect else {
        return;
    };
    effect.suppression = match external {
        ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route) => {
            if sender.is_some_and(|sender| route.recipient == sender.to_bare()) {
                PlanSuppressionPolicy::Always
            } else {
                PlanSuppressionPolicy::SenderOnly
            }
        }
        ExternalEffect::Frame(_) => PlanSuppressionPolicy::Always,
        ExternalEffect::Delivery(delivery) => match delivery {
            ExternalDeliveryEffect::RouteToPeer { jid, stanza, .. }
            | ExternalDeliveryEffect::RelayFullJid {
                target: jid,
                stanza,
                ..
            } => {
                if sender == Some(jid) || subject_rebroadcast(stanza) {
                    PlanSuppressionPolicy::Always
                } else {
                    PlanSuppressionPolicy::SenderOnly
                }
            }
            ExternalDeliveryEffect::QueueDetached {
                resources, stanza, ..
            } => {
                if subject_rebroadcast(stanza) || resources.iter().all(|jid| sender == Some(jid)) {
                    PlanSuppressionPolicy::Always
                } else {
                    PlanSuppressionPolicy::SenderOnly
                }
            }
            ExternalDeliveryEffect::RelayBareJid { stanza, .. } if subject_rebroadcast(stanza) => {
                PlanSuppressionPolicy::Always
            }
            ExternalDeliveryEffect::RelayCarbons { .. } => PlanSuppressionPolicy::Always,
            ExternalDeliveryEffect::RelayBareJid { .. }
            | ExternalDeliveryEffect::Carbons { .. }
            | ExternalDeliveryEffect::QueueOfflineDelivery { .. } => {
                PlanSuppressionPolicy::SenderOnly
            }
            _ => effect.suppression,
        },
        ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { .. })
        | ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate { .. }) => {
            PlanSuppressionPolicy::SenderOnly
        }
        ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation { .. }) => {
            PlanSuppressionPolicy::Always
        }
        _ => effect.suppression,
    };
}

pub(super) fn message_dependencies(message: &Message) -> Vec<super::PlanEffectDependency> {
    waddle_xmpp::xep::extract_stanza_ids(message)
        .into_iter()
        .map(|minted| super::PlanEffectDependency::AfterArchive {
            archive: minted.by.to_bare(),
            minted,
        })
        .collect()
}

/// Subject reflection is truthful only after its planned room mutation succeeds.
pub(super) fn subject_dependency(
    effect: &PlannedEffect,
    plan: &[PlannedEffect],
) -> Option<super::PlanEffectDependency> {
    let stanza = match &effect.effect {
        Effect::External(ExternalEffect::Frame(stanza)) => stanza,
        Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { stanza, .. }
            | ExternalDeliveryEffect::QueueDetached { stanza, .. }
            | ExternalDeliveryEffect::RelayFullJid { stanza, .. }
            | ExternalDeliveryEffect::RelayBareJid { stanza, .. },
        )) => stanza,
        _ => return None,
    };
    let Stanza::Message(message) = stanza.as_ref() else {
        return None;
    };
    if !waddle_xmpp::muc::is_groupchat_subject_change_message(message) {
        return None;
    }
    let source = message.from.as_ref()?.to_bare();
    let texts = waddle_xmpp::muc::RoomSubjectTexts::from_message_subjects(&message.subjects);
    plan.iter().rev().find_map(|planned| match &planned.effect {
        Effect::External(ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
            room,
            mutation: super::room::RoomActorMutation::SetSubject { subject, .. },
        })) if *room == source && subject.texts == texts => {
            Some(super::PlanEffectDependency::AfterRoomSubject {
                room: room.clone(),
                state: subject.clone(),
            })
        }
        _ => None,
    })
}
