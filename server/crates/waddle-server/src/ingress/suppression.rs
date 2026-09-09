//! Pure application of the policies captured before admission.
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
    archive_outcomes: &[(PlanEffectDependency, MamTxStoreOutcome)],
    unreceipted: &[waddle_xmpp::ingress::IngressEffectIntent],
    route_progress: &[super::recorded::RouteProgress],
) -> Vec<ExternalEffect> {
    external_effect_indices(plan, verdict, archive_outcomes, unreceipted, route_progress)
        .into_iter()
        .filter_map(|index| match &plan.plan[index].effect {
            Effect::External(effect) => {
                let mut effect = effect.clone();
                if let RouteProgressFilter::Keep { remaining } =
                    route_progress_filter(&effect, route_progress)
                {
                    if let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
                        resources,
                        ..
                    }) = &mut effect
                    {
                        *resources = remaining;
                    }
                }
                Some(effect)
            }
            _ => None,
        })
        .collect()
}

pub(crate) fn external_effect_indices(
    plan: &IngressPlan,
    verdict: &ReconcileVerdict,
    archive_outcomes: &[(PlanEffectDependency, MamTxStoreOutcome)],
    unreceipted: &[waddle_xmpp::ingress::IngressEffectIntent],
    route_progress: &[super::recorded::RouteProgress],
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
            let progress = route_progress_filter(effect, route_progress);
            if matches!(progress, RouteProgressFilter::Drop) {
                return None;
            }
            if duplicate
                && duplicate_policy(planned) == PlanSuppressionPolicy::SenderOnly
                && !sender_delivery(effect, plan.sanitized_message.from.as_ref())
                && !subject_rebroadcast(effect)
                && !matches!(progress, RouteProgressFilter::Keep { .. })
                && !unreceipted_repair(effect, unreceipted)
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

/// The same eligibility decision drives both cloned effects and dependency
/// indices, so trimming cannot detach an effect from its planned prerequisites.
enum RouteProgressFilter {
    Untracked,
    Keep { remaining: Vec<jid::FullJid> },
    Drop,
}

fn route_progress_filter(
    effect: &ExternalEffect,
    progress: &[super::recorded::RouteProgress],
) -> RouteProgressFilter {
    if !matches!(
        effect,
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueDetached { .. }
                | ExternalDeliveryEffect::RouteToPeer { .. }
        )
    ) {
        return RouteProgressFilter::Untracked;
    }
    let Some(progress) = progress.iter().find(|progress| progress.matches(effect)) else {
        return RouteProgressFilter::Untracked;
    };
    let remaining = progress.remaining(effect);
    if remaining.is_empty() {
        RouteProgressFilter::Drop
    } else {
        RouteProgressFilter::Keep { remaining }
    }
}

fn unreceipted_repair(
    effect: &ExternalEffect,
    unreceipted: &[waddle_xmpp::ingress::IngressEffectIntent],
) -> bool {
    super::recorded::recorded_route_obligation(unreceipted, effect)
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
/// Match the attempted archive dependency: after alias retention, a tombstone
/// can retain a historical stanza-id different from this attempt's minted id.
pub(super) fn tombstone_swallowed(
    planned: &PlannedEffect,
    archive_outcomes: &[(PlanEffectDependency, MamTxStoreOutcome)],
) -> bool {
    planned.tombstone_suppression == PlanSuppressionPolicy::TombstoneSwallowed
        && archive_outcomes.iter().any(|(attempted_archive, outcome)| {
            matches!(outcome, MamTxStoreOutcome::TombstoneHit(_))
                && (planned.dependencies.is_empty()
                    || planned.dependencies.contains(attempted_archive))
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
    use jid::BareJid;
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
                        route_identity: None,
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
                external_effect_indices(&plan, &ReconcileVerdict::FirstCommit, &[], &[], &[]),
                vec![0, 1]
            );
            let expected = if has_thread { vec![0] } else { vec![0, 1] };
            assert_eq!(
                external_effect_indices(&plan, &ReconcileVerdict::Consistent, &[], &[], &[]),
                expected
            );
        }
    }

    #[test]
    fn room_activity_requires_its_exact_recorded_intent_without_inbox_projection() {
        use crate::server::routes::interpret::effects::direct::ExternalDirectEffect;
        use waddle_xmpp::ingress::{
            EffectMessageIdentity, EntityGeneration, IngressEffectIntent,
            NotificationActivityMutation,
        };
        let room: BareJid = "room@example.com".parse().expect("room");
        let sender: jid::FullJid = "sender@example.com/device".parse().expect("sender");
        let owner = sender.to_bare();
        for mutation in [
            NotificationActivityMutation::ChatState {
                conversation: room.clone(),
                state: waddle_xmpp::xep::xep0085::ChatState::Composing,
                committed_at_ms: 1000,
            },
            NotificationActivityMutation::ReadMarker {
                conversation: room.clone(),
                committed_at_ms: 1000,
            },
        ] {
            let mut message = Message::new(Some(room.clone().into()));
            message.from = Some(sender.clone().into());
            message.type_ = MessageType::Groupchat;
            let mut plan = IngressPlan {
                failure: None,
                plan: vec![PlannedEffect::new(Effect::External(ExternalEffect::Direct(
                    ExternalDirectEffect::NotificationActivity {
                        owner: owner.clone(),
                        mutation: mutation.clone(),
                    },
                )))
                .with_suppression(PlanSuppressionPolicy::Always)],
                intents: vec![IngressEffectIntent::RouteMucGroupchat {
                    room: room.clone(),
                    occupants: vec![sender.clone()],
                    reflection: sender.clone(),
                    room_generation: EntityGeneration::INITIAL,
                    route_identity: EffectMessageIdentity::stanza(StanzaId::new(
                        "room-copy",
                        room.clone().into(),
                    )),
                }],
                sanitized_message: message,
                error_reply: None,
                rejection: None,
                room_execution: RoomExecutionPath::None,
            };
            for verdict in [ReconcileVerdict::FirstCommit, ReconcileVerdict::Consistent] {
                assert!(external_effect_indices(&plan, &verdict, &[], &[], &[]).is_empty());
                plan.intents
                    .push(IngressEffectIntent::NotificationActivityPreview {
                        owner: owner.clone(),
                        mutation: mutation.clone(),
                    });
                assert_eq!(
                    external_effect_indices(&plan, &verdict, &[], &[], &[]),
                    vec![0]
                );
                let IngressEffectIntent::NotificationActivityPreview {
                    owner: recorded_owner,
                    ..
                } = plan.intents.last_mut().expect("activity intent")
                else {
                    panic!("activity intent");
                };
                *recorded_owner = "other@example.com".parse().expect("other owner");
                assert!(external_effect_indices(&plan, &verdict, &[], &[], &[]).is_empty());
                plan.intents.pop();
                let mut changed_mutation = mutation.clone();
                match &mut changed_mutation {
                    NotificationActivityMutation::ChatState {
                        committed_at_ms, ..
                    }
                    | NotificationActivityMutation::ReadMarker {
                        committed_at_ms, ..
                    } => {
                        *committed_at_ms += 1;
                    }
                    _ => panic!("tested activity mutation"),
                }
                plan.intents
                    .push(IngressEffectIntent::NotificationActivityPreview {
                        owner: owner.clone(),
                        mutation: changed_mutation,
                    });
                assert!(external_effect_indices(&plan, &verdict, &[], &[], &[]).is_empty());
                plan.intents.pop();
            }
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
                    PlanEffectDependency::AfterArchive {
                        archive: archive.clone(),
                        minted: StanzaId::new("attempted", archive.clone().into()),
                    },
                    MamTxStoreOutcome::TombstoneHit(StanzaId::new("id", archive.clone().into())),
                )]
            } else {
                vec![]
            };
            assert_eq!(
                filter_external_effects(&plan, &verdict, &outcomes, &[], &[]).len(),
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
            (recipient.clone(), recipient_id, 0),
        ] {
            assert_eq!(
                filter_external_effects(
                    &plan,
                    &ReconcileVerdict::Consistent,
                    &[(
                        PlanEffectDependency::AfterArchive {
                            archive,
                            minted: id
                        },
                        MamTxStoreOutcome::TombstoneHit(StanzaId::new(
                            "historical-id",
                            recipient.clone().into(),
                        )),
                    )],
                    &[],
                    &[],
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
                route_identity: None,
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
            external_effect_indices(&plan, &ReconcileVerdict::FirstCommit, &[], &[], &[]),
            vec![0, 1]
        );
        assert_eq!(
            external_effect_indices(&plan, &ReconcileVerdict::Consistent, &[], &[], &[]),
            vec![0]
        );
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    use crate::ingress::recorded::RouteProgress;
    use crate::server::routes::interpret::effects::RoomExecutionPath;
    use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};

    #[test]
    fn ordinary_duplicate_trims_completed_and_unrecorded_targets_without_shifting_dependencies() {
        let a: jid::FullJid = "juliet@example.com/a".parse().expect("a");
        let b: jid::FullJid = "juliet@example.com/b".parse().expect("b");
        let c: jid::FullJid = "juliet@example.com/c".parse().expect("c");
        let identity = EffectMessageIdentity::capture_ordinal(1);
        let intent = IngressEffectIntent::RouteDirect {
            recipient: a.to_bare(),
            fanout: vec![a.clone(), b.clone()],
            route_identity: identity.clone(),
        };
        let progress = RouteProgress {
            receipt: crate::ingress::durable::receipt_key(&intent).expect("receipt"),
            recipient: a.to_bare(),
            fanout: vec![a.clone(), b.clone()],
            route_identity: identity.clone(),
            completed: vec![a.clone()],
        };
        let message = Message::new(Some(a.to_bare().into()));
        let mut plan = IngressPlan {
            failure: None,
            plan: vec![
                PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
                    ExternalDeliveryEffect::QueueDetached {
                        route_identity: Some(identity),
                        call_setup: None,
                        bare: a.to_bare(),
                        resources: vec![a.clone(), b.clone(), c],
                        stanza: Box::new(Stanza::Message(message.clone())),
                    },
                )))
                .with_suppression(PlanSuppressionPolicy::SenderOnly),
            ],
            intents: vec![intent],
            sanitized_message: message,
            error_reply: None,
            rejection: None,
            room_execution: RoomExecutionPath::None,
        };
        let saved = [progress];
        let effects =
            filter_external_effects(&plan, &ReconcileVerdict::Consistent, &[], &[], &saved);
        assert!(
            matches!(&effects[..], [ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. })] if resources == &[b])
        );
        assert_eq!(
            external_effect_indices(&plan, &ReconcileVerdict::Consistent, &[], &[], &saved),
            vec![0]
        );
        if let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            resources,
            ..
        })) = &mut plan.plan[0].effect
        {
            *resources = vec![a];
        }
        assert!(
            filter_external_effects(&plan, &ReconcileVerdict::Consistent, &[], &[], &saved)
                .is_empty()
        );
        assert!(
            external_effect_indices(&plan, &ReconcileVerdict::Consistent, &[], &[], &saved)
                .is_empty()
        );
    }
}
