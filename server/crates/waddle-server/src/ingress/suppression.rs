//! Pure application of the policies captured before admission.
use jid::BareJid;
use waddle_xmpp::{mam::MamTxStoreOutcome, Stanza};
use xmpp_parsers::message::{Message, MessageType};

use crate::{
    ingress_uow::ReconcileVerdict,
    server::routes::interpret::effects::{
        delivery::ExternalDeliveryEffect, room::ExternalRoomEffect, Effect, ExternalEffect,
        IngressPlan, PlanEffectDependency, PlanSuppressionPolicy, PlannedEffect,
    },
};

pub fn filter_external_effects(
    plan: &IngressPlan,
    verdict: &ReconcileVerdict,
    archive_outcomes: &[(BareJid, MamTxStoreOutcome)],
) -> Vec<ExternalEffect> {
    external_effect_indices(plan, verdict, archive_outcomes)
        .into_iter()
        .filter_map(|index| match &plan.plan[index].effect {
            Effect::External(effect) => Some(effect.clone()),
            _ => None,
        })
        .collect()
}

pub(crate) fn external_effect_indices(
    plan: &IngressPlan,
    verdict: &ReconcileVerdict,
    archive_outcomes: &[(BareJid, MamTxStoreOutcome)],
) -> Vec<usize> {
    let duplicate = !matches!(verdict, ReconcileVerdict::FirstCommit);
    plan.plan
        .iter()
        .enumerate()
        .filter_map(|(index, planned)| {
            let Effect::External(effect) = &planned.effect else {
                return None;
            };
            if duplicate
                && planned.suppression == PlanSuppressionPolicy::SenderOnly
                && !subject_rebroadcast(effect)
            {
                return None;
            }
            if tombstone_swallowed(planned, archive_outcomes) {
                return None;
            }
            Some(index)
        })
        .collect()
}

/// Shared by commit-time durable application and post-commit external filtering.
pub(super) fn tombstone_swallowed(
    planned: &PlannedEffect,
    archive_outcomes: &[(BareJid, MamTxStoreOutcome)],
) -> bool {
    planned.tombstone_suppression == PlanSuppressionPolicy::TombstoneSwallowed
        && archive_outcomes.iter().any(|(archive, outcome)| {
            let MamTxStoreOutcome::TombstoneHit(id) = outcome else {
                return false;
            };
            planned.dependencies.is_empty()
                || planned
                    .dependencies
                    .iter()
                    .any(|dependency| match dependency {
                        PlanEffectDependency::AfterArchive {
                            archive: dependency_archive,
                            minted,
                        } => dependency_archive == archive && minted == id,
                    })
        })
}

fn subject_message(message: &Message) -> bool {
    message.type_ == MessageType::Groupchat
        && !message.subjects.is_empty()
        && message.bodies.is_empty()
}

fn subject_stanza(stanza: &Stanza) -> bool {
    matches!(stanza, Stanza::Message(message) if subject_message(message))
}

fn subject_rebroadcast(effect: &ExternalEffect) -> bool {
    match effect {
        ExternalEffect::Frame(stanza) => subject_stanza(stanza),
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { stanza, .. }
            | ExternalDeliveryEffect::QueueDetached { stanza, .. }
            | ExternalDeliveryEffect::RelayFullJid { stanza, .. }
            | ExternalDeliveryEffect::RelayBareJid { stanza, .. },
        ) => subject_stanza(stanza),
        ExternalEffect::Room(ExternalRoomEffect::BroadcastRoomSystemMessage {
            message, ..
        }) => subject_message(message),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::interpret::effects::RoomExecutionPath;
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::message::Lang;

    #[test]
    fn filter_external_effects_policy_table() {
        let archive: BareJid = "room@example.com".parse().expect("archive");
        for (duplicate, sender_only, subject, tombstone, preserve_tombstone, expected) in [
            (false, true, false, false, false, 1),
            (true, true, false, false, false, 0),
            (true, false, false, false, false, 1),
            (true, true, true, false, false, 1),
            (false, false, false, true, false, 0),
            (true, true, true, true, false, 0),
            (true, false, false, true, true, 1),
        ] {
            let mut message = Message::new(Some(archive.clone().into()));
            if subject {
                message.type_ = MessageType::Groupchat;
                message.subjects.insert(Lang::new(), "topic".into());
            }
            let mut effect = PlannedEffect::new(Effect::External(ExternalEffect::Frame(Box::new(
                Stanza::Message(message.clone()),
            ))));
            effect.suppression = if sender_only {
                PlanSuppressionPolicy::SenderOnly
            } else {
                PlanSuppressionPolicy::Always
            };
            if preserve_tombstone {
                effect.tombstone_suppression = PlanSuppressionPolicy::Always;
            }
            let plan = IngressPlan {
                plan: vec![effect],
                intents: vec![],
                sanitized_message: message,
                error_reply: None,
                room_execution: RoomExecutionPath::None,
            };
            let verdict = if duplicate {
                ReconcileVerdict::Consistent
            } else {
                ReconcileVerdict::FirstCommit
            };
            let outcomes = if tombstone {
                vec![(
                    archive.clone(),
                    MamTxStoreOutcome::TombstoneHit(StanzaId::new("id", archive.clone().into())),
                )]
            } else {
                vec![]
            };
            assert_eq!(
                filter_external_effects(&plan, &verdict, &outcomes).len(),
                expected
            );
        }
    }

    #[test]
    fn tombstone_suppression_requires_the_exact_archive_dependency() {
        let sender: BareJid = "sender@example.com".parse().expect("sender");
        let recipient: BareJid = "recipient@example.com".parse().expect("recipient");
        let sender_id = StanzaId::new("same-id", sender.clone().into());
        let recipient_id = StanzaId::new("same-id", recipient.clone().into());
        let message = Message::new(Some(recipient.clone().into()));
        let plan = IngressPlan {
            plan: vec![
                PlannedEffect::new(Effect::External(ExternalEffect::Frame(Box::new(
                    Stanza::Message(message.clone()),
                ))))
                .with_dependency(PlanEffectDependency::AfterArchive {
                    archive: recipient.clone(),
                    minted: recipient_id.clone(),
                }),
            ],
            intents: vec![],
            sanitized_message: message,
            error_reply: None,
            room_execution: RoomExecutionPath::None,
        };
        for (archive, id, expected) in [
            (sender, sender_id, 1),
            (
                recipient.clone(),
                StanzaId::new("other-id", recipient.clone().into()),
                1,
            ),
            (recipient, recipient_id, 0),
        ] {
            assert_eq!(
                filter_external_effects(
                    &plan,
                    &ReconcileVerdict::Consistent,
                    &[(archive, MamTxStoreOutcome::TombstoneHit(id))],
                )
                .len(),
                expected,
            );
        }
    }
}
