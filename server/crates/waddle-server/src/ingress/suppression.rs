//! Pure application of the policies captured before admission.
use jid::BareJid;
use waddle_xmpp::{mam::MamTxStoreOutcome, Stanza};
use xmpp_parsers::message::Message;

use crate::{
    ingress_uow::ReconcileVerdict,
    server::routes::interpret::effects::{
        delivery::ExternalDeliveryEffect, Effect, ExternalEffect, IngressPlan,
        PlanEffectDependency, PlanSuppressionPolicy, PlannedEffect,
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
            if !super::recorded::external_in_recorded_audience(plan, effect) {
                return None;
            }
            if duplicate && !relay_carbons_recorded(plan, effect) {
                return None;
            }
            if duplicate
                && duplicate_policy(planned) == PlanSuppressionPolicy::SenderOnly
                && !sender_delivery(effect, plan.sanitized_message.from.as_ref())
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

/// A reconnect can change a local fanout plan into a remote-owner plan. Only
/// the obligation retained by reconciliation authorizes that remote fanout;
/// execution separately skips matching obligations that already have receipts.
fn relay_carbons_recorded(plan: &IngressPlan, effect: &ExternalEffect) -> bool {
    let ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
        owner,
        exclude,
        kind,
        ..
    }) = effect
    else {
        return true;
    };
    plan.intents.iter().any(|intent| {
        matches!(intent, waddle_xmpp::ingress::IngressEffectIntent::RelayCarbons {
            owner: recorded_owner,
            exclude: recorded_exclude,
            kind: recorded_kind,
        } if owner == recorded_owner && exclude == recorded_exclude && kind == recorded_kind)
    })
}

fn duplicate_policy(planned: &PlannedEffect) -> PlanSuppressionPolicy {
    if matches!(
        planned.effect,
        Effect::External(
            ExternalEffect::RoomMembershipMutation(_) | ExternalEffect::InviteLedger(_)
        )
    ) {
        PlanSuppressionPolicy::Always
    } else if planned
        .dependencies
        .iter()
        .any(|dependency| matches!(dependency, PlanEffectDependency::AfterDmPinMutation { .. }))
    {
        PlanSuppressionPolicy::SenderOnly
    } else {
        planned.suppression
    }
}

fn sender_delivery(effect: &ExternalEffect, sender: Option<&jid::Jid>) -> bool {
    let Some(sender) = sender else {
        return false;
    };
    match effect {
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. }) => {
            jid.to_bare() == sender.to_bare()
        }
        ExternalEffect::Frame(stanza) => {
            matches!(stanza.as_ref(), Stanza::Message(message) if message.to.as_ref().is_some_and(|recipient| recipient.to_bare() == sender.to_bare()))
        }
        _ => false,
    }
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
                        _ => false,
                    })
        })
}

fn subject_message(message: &Message) -> bool {
    waddle_xmpp::muc::is_groupchat_subject_change_message(message)
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
        ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route) => {
            subject_message(&route.message)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::interpret::effects::RoomExecutionPath;
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::message::{Lang, MessageType};

    #[test]
    fn planned_subject_retry_only_rebroadcasts_without_thread() {
        use crate::server::routes::interpret::effects::{
            delivery::PeerDeliveryKind, EffectSink, PlanSink,
        };
        let sender: jid::FullJid = "sender@example.com/device".parse().expect("sender");
        let peer: jid::FullJid = "peer@example.com/device".parse().expect("peer");
        #[derive(Clone, Copy)]
        enum ThreadShape {
            None,
            Typed,
            /// The inbound parser's representation of `<thread parent='…'/>`.
            ParentedPayload,
        }
        for shape in [
            ThreadShape::None,
            ThreadShape::Typed,
            ThreadShape::ParentedPayload,
        ] {
            let has_thread = !matches!(shape, ThreadShape::None);
            let mut message = Message::new(Some("room@example.com".parse().expect("room")));
            message.from = Some(sender.clone().into());
            message.type_ = MessageType::Groupchat;
            message.subjects.insert(Lang::new(), "topic".into());
            if has_thread {
                message.thread = Some(xmpp_parsers::message::Thread {
                    id: "timeline".into(),
                    parent: None,
                });
            }
            if matches!(shape, ThreadShape::ParentedPayload) {
                waddle_xmpp_core::parser_utils::reattach_thread_parent(
                    &mut message,
                    "root".into(),
                    waddle_xmpp_core::CLIENT_STANZA_NS,
                );
                assert!(message.thread.is_none());
            }
            let sink = PlanSink::new();
            sink.observe_sender(&sender);
            for recipient in [sender.clone(), peer.clone()] {
                let mut reflection = message.clone();
                reflection.to = Some(recipient.clone().into());
                sink.record(PlannedEffect::new(Effect::External(
                    ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                        jid: recipient,
                        stanza: Box::new(Stanza::Message(reflection)),
                        kind: PeerDeliveryKind::RegistryFrame,
                        call_setup: None,
                    }),
                )));
            }
            let plan = IngressPlan {
                failure: None,
                plan: sink.snapshot(),
                intents: vec![],
                sanitized_message: message,
                error_reply: None,
                rejection: None,
                room_execution: RoomExecutionPath::None,
            };
            assert_eq!(
                external_effect_indices(&plan, &ReconcileVerdict::FirstCommit, &[]),
                vec![0, 1]
            );
            let expected = if has_thread { vec![0] } else { vec![0, 1] };
            assert_eq!(
                external_effect_indices(&plan, &ReconcileVerdict::Consistent, &[]),
                expected
            );
        }
    }

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
                failure: None,
                plan: vec![effect],
                intents: vec![],
                sanitized_message: message,
                error_reply: None,
                rejection: None,
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
            failure: None,
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
            rejection: None,
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
    #[test]
    fn duplicate_pin_fanout_preserves_only_sender_resources() {
        let sender: jid::FullJid = "sender@example.com/device".parse().expect("sender");
        let peer: jid::FullJid = "peer@example.com/device".parse().expect("peer");
        let pair =
            crate::server::routes::websocket::DmPairKey::new(sender.to_bare(), peer.to_bare());
        let target = StanzaId::new("pin", sender.to_bare().into());
        let dependency = PlanEffectDependency::AfterDmPinMutation { pair, target };
        let mut incoming = Message::new(Some(peer.clone().into()));
        incoming.from = Some(sender.clone().into());
        let effects = [sender.clone(), peer].into_iter().map(|recipient| {
            PlannedEffect::new(Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                jid: recipient.clone(), stanza: Box::new(Stanza::Message(Message::new(Some(recipient.into())))),
                kind: crate::server::routes::interpret::effects::delivery::PeerDeliveryKind::RegistryFrame,
                call_setup: None,
            }))).with_dependency(dependency.clone())
        }).collect();
        let plan = IngressPlan {
            failure: None,
            rejection: None,
            plan: effects,
            intents: vec![],
            sanitized_message: incoming,
            error_reply: None,
            room_execution: RoomExecutionPath::None,
        };
        assert_eq!(
            external_effect_indices(&plan, &ReconcileVerdict::FirstCommit, &[]),
            vec![0, 1]
        );
        assert_eq!(
            external_effect_indices(&plan, &ReconcileVerdict::Consistent, &[]),
            vec![0]
        );
    }
}
