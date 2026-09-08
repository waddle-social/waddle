use super::decision::IngressDecisionClass;
use crate::{
    ingress_uow::IngressUowError,
    server::routes::interpret::effects::{
        delivery::ExternalDeliveryEffect, Effect, ExternalEffect, IngressPlan, PlanRejection,
        PlannedEffect, PolicyDeniedReason, RoomExecutionPath,
    },
};
use waddle_xmpp::{
    ingress::{FrozenStanzaError, FrozenStanzaErrorType, IngressEffectIntent},
    protocol::CarbonKind,
    Stanza, StanzaErrorCondition,
};

pub(super) fn planned_rejection(
    plan: &IngressPlan,
) -> Result<Option<IngressDecisionClass>, IngressUowError> {
    plan.rejection
        .as_ref()
        .map(|rejection| {
            Ok(match rejection {
                PlanRejection::MissingRoomStanzaId => {
                    return Err(IngressUowError::MissingRoomStanzaId)
                }
                PlanRejection::AuthorizationDenied(_) => IngressDecisionClass::AuthorizationDenied,
                PlanRejection::SemanticMalformed(_) => IngressDecisionClass::SemanticMalformed,
                PlanRejection::PolicyDenied(PolicyDeniedReason::CaptureOverflow) => {
                    IngressDecisionClass::CaptureOverflow
                }
                PlanRejection::PolicyDenied(_) => IngressDecisionClass::PolicyDenied,
            })
        })
        .transpose()
}
pub(super) fn rejection_plan(
    plan: &IngressPlan,
    class: IngressDecisionClass,
    sender: &jid::FullJid,
) -> Result<IngressPlan, IngressUowError> {
    let mut rejected = plan.clone();
    rejected.plan.retain(|planned| {
        matches!(
            &planned.effect,
            Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::Carbons { owner, kind: CarbonKind::Sent, .. }
                | ExternalDeliveryEffect::RelayCarbons { owner, kind: CarbonKind::Sent, .. }
            )) if owner == &sender.to_bare() && class != IngressDecisionClass::AliasConflict
        )
    });
    // A denial drops archive work. Its sender carbon still describes the
    // offered stanza and must not depend on an archive we no longer write.
    for planned in &mut rejected.plan {
        planned.dependencies.clear();
    }
    rejected.intents.retain(|intent| {
        matches!(intent, IngressEffectIntent::ErrorReply { .. })
            || (class != IngressDecisionClass::AliasConflict
                && sender_carbon_intent(intent, &sender.to_bare()))
    });
    rejected.room_execution = RoomExecutionPath::None;
    let reply = if class == IngressDecisionClass::AliasConflict {
        let recipient = sender.clone();
        let error = FrozenStanzaError::new(
            FrozenStanzaErrorType::Cancel,
            StanzaErrorCondition::Conflict,
        );
        let mut message = plan.sanitized_message.clone();
        message.to = Some(recipient.clone().into());
        message.from = plan.sanitized_message.to.clone();
        message.type_ = xmpp_parsers::message::MessageType::Error;
        message.payloads.push(error.to_xmpp().into());
        rejected.intents = vec![IngressEffectIntent::ErrorReply { recipient, error }];
        Stanza::Message(message)
    } else {
        plan.error_reply
            .clone()
            .ok_or(IngressUowError::EffectIntentConflict)?
    };
    rejected.error_reply = Some(reply.clone());
    rejected
        .plan
        .push(PlannedEffect::new(Effect::External(ExternalEffect::Frame(
            Box::new(reply),
        ))));
    Ok(rejected)
}

fn sender_carbon_intent(intent: &IngressEffectIntent, sender: &jid::BareJid) -> bool {
    match intent {
        IngressEffectIntent::Carbons {
            excluded_source,
            kind: CarbonKind::Sent,
            ..
        } => &excluded_source.to_bare() == sender,
        IngressEffectIntent::RelayCarbons {
            owner,
            kind: CarbonKind::Sent,
            ..
        } => owner == sender,
        _ => false,
    }
}

pub(super) fn is_recorded_rejection(intents: &[IngressEffectIntent]) -> bool {
    let Some(sender) = intents.iter().find_map(|intent| match intent {
        IngressEffectIntent::ErrorReply { recipient, .. } => Some(recipient.to_bare()),
        _ => None,
    }) else {
        return false;
    };
    intents.iter().all(|intent| {
        matches!(intent, IngressEffectIntent::ErrorReply { .. })
            || sender_carbon_intent(intent, &sender)
    })
}

/// Reconstruct the original committed denial without consulting today's policy
/// or today's provisional message. A wire replay cannot turn a rejection into
/// an accepted message or change the standard stanza error it already owns.
pub(super) fn recorded_rejection_plan(
    envelope: &crate::ingress_substrate::MessageEnvelope,
    intents: &[IngressEffectIntent],
) -> Result<IngressPlan, IngressUowError> {
    if !is_recorded_rejection(intents) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let mut plan = Vec::with_capacity(intents.len());
    let mut error_reply = None;
    for intent in intents {
        let (recipient, error) = match intent {
            IngressEffectIntent::ErrorReply { recipient, error } => (recipient, error),
            IngressEffectIntent::Carbons {
                carbon_recipients,
                excluded_source,
                kind,
            } => {
                for recipient in carbon_recipients {
                    plan.push(PlannedEffect::new(Effect::External(
                        ExternalEffect::Delivery(ExternalDeliveryEffect::Carbons {
                            owner: excluded_source.to_bare(),
                            recipient: recipient.clone(),
                            exclude: vec![excluded_source.clone()],
                            message: Box::new(envelope.message().clone()),
                            kind: *kind,
                        }),
                    )));
                }
                continue;
            }
            IngressEffectIntent::RelayCarbons {
                owner,
                exclude,
                kind,
            } => {
                plan.push(PlannedEffect::new(Effect::External(
                    ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
                        origin: None,
                        owner: owner.clone(),
                        exclude: exclude.clone(),
                        message: Box::new(envelope.message().clone()),
                        kind: *kind,
                    }),
                )));
                continue;
            }
            _ => return Err(IngressUowError::EffectIntentConflict),
        };
        let mut message = envelope.message().clone();
        message.to = Some(recipient.clone().into());
        message.from = envelope.message().to.clone();
        message.type_ = xmpp_parsers::message::MessageType::Error;
        message.payloads.push(error.to_xmpp().into());
        let reply = Stanza::Message(message);
        error_reply.get_or_insert_with(|| reply.clone());
        plan.push(PlannedEffect::new(Effect::External(ExternalEffect::Frame(
            Box::new(reply),
        ))));
    }
    Ok(IngressPlan {
        failure: None,
        rejection: None,
        plan,
        intents: intents.to_vec(),
        sanitized_message: envelope.message().clone(),
        error_reply,
        room_execution: RoomExecutionPath::None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn malformed_reply_receipt(fixture: crate::ingress::test_support::IngressFixture) {
        use crate::ingress::{
            commit::commit_submission,
            execute::{execute_effects, terminalize_if_complete},
        };
        use crate::server::routes::interpret::{effects::ImmediateSink, Deps};
        let mut submission = fixture.submission(Some("malformed-reply-receipt"), "bad correction");
        submission.plan = crate::server::routes::interpret::reject_malformed_message(
            submission.plan.sanitized_message.clone(),
            &submission.sender,
        );
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("commit rejection");
        assert_eq!(decision.class, IngressDecisionClass::SemanticMalformed);
        let key = decision.message_key.expect("canonical key");
        assert_eq!(fixture.count("ingress_effect_intents").await, 1);
        assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
        assert!(!terminalize_if_complete(&fixture.uow, key)
            .await
            .expect("pending reply"));
        let registry = waddle_xmpp::registry::ConnectionRegistry::new();
        let deps = Deps::registry_only(&registry);
        let mut report = execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert_eq!(report.frame_obligations.len(), 1);
        assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
        assert!(!terminalize_if_complete(&fixture.uow, key)
            .await
            .expect("frame not written"));
        assert!(report
            .complete_frame_obligations(
                &fixture.uow,
                &fixture.db,
                std::time::Duration::from_secs(5)
            )
            .await
            .expect("write reply receipt"));
        assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_malformed_rejection_receipts_only_after_frame_write() {
        malformed_reply_receipt(crate::ingress::test_support::IngressFixture::sqlite().await).await;
    }

    #[tokio::test]
    async fn postgres_malformed_rejection_receipts_only_after_frame_write() {
        if let Some(fixture) =
            crate::ingress::test_support::IngressFixture::postgres("malformed_reply_receipt").await
        {
            malformed_reply_receipt(fixture).await;
        }
    }

    #[test]
    fn committed_denial_keeps_only_sender_carbon_obligations() {
        let sender: jid::FullJid = "sender@example.test/device".parse().expect("sender");
        let sibling: jid::FullJid = "sender@example.test/sibling".parse().expect("sibling");
        let recipient: jid::FullJid = "peer@example.test/device".parse().expect("recipient");
        let mut plan = crate::server::routes::interpret::reject_malformed_message(
            xmpp_parsers::message::Message::new(Some(recipient.clone().into())),
            &sender,
        );
        for (source, target, kind) in [
            (sender.clone(), sibling, CarbonKind::Sent),
            (recipient.clone(), recipient.clone(), CarbonKind::Received),
        ] {
            plan.intents.push(IngressEffectIntent::Carbons {
                carbon_recipients: vec![target.clone()],
                excluded_source: source.clone(),
                kind,
            });
            plan.plan.push(PlannedEffect::new(Effect::External(
                ExternalEffect::Delivery(ExternalDeliveryEffect::Carbons {
                    owner: source.to_bare(),
                    recipient: target,
                    exclude: vec![source],
                    message: Box::new(plan.sanitized_message.clone()),
                    kind,
                }),
            )));
        }
        plan.intents.push(IngressEffectIntent::RelayCarbons {
            owner: sender.to_bare(),
            exclude: vec![sender.clone()],
            kind: CarbonKind::Sent,
        });
        plan.plan.push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
                origin: None,
                owner: sender.to_bare(),
                exclude: vec![sender.clone()],
                message: Box::new(plan.sanitized_message.clone()),
                kind: CarbonKind::Sent,
            }),
        )));
        let denied =
            rejection_plan(&plan, IngressDecisionClass::PolicyDenied, &sender).expect("denial");
        assert_eq!(denied.plan.len(), 3);
        assert_eq!(denied.intents.len(), 3);
        assert!(is_recorded_rejection(&denied.intents));
        assert!(!denied.intents.iter().any(|intent| matches!(
            intent,
            IngressEffectIntent::Carbons {
                kind: CarbonKind::Received,
                ..
            }
        )));
        let replay = recorded_rejection_plan(
            &crate::ingress_substrate::MessageEnvelope::new(plan.sanitized_message.clone()),
            &denied.intents,
        )
        .expect("reconstruct sender obligations");
        assert_eq!(replay.plan.len(), 3);
        assert_eq!(replay.intents, denied.intents);
        let conflict = rejection_plan(&plan, IngressDecisionClass::AliasConflict, &sender)
            .expect("alias conflict");
        assert_eq!(conflict.plan.len(), 1);
        assert_eq!(conflict.intents.len(), 1);
    }

    #[test]
    fn error_intent_alone_does_not_classify_a_plan_rejection() {
        let sender: jid::FullJid = "sender@example.test/device".parse().expect("sender");
        let envelope = crate::ingress_substrate::MessageEnvelope::new(
            xmpp_parsers::message::Message::new(None),
        );
        let intents = vec![IngressEffectIntent::ErrorReply {
            recipient: sender,
            error: FrozenStanzaError::new(
                FrozenStanzaErrorType::Cancel,
                StanzaErrorCondition::Forbidden,
            ),
        }];
        let plan = recorded_rejection_plan(&envelope, &intents).expect("recorded plan");
        assert!(matches!(planned_rejection(&plan), Ok(None)));
    }

    #[test]
    fn recorded_denial_reconstruction_preserves_original_message_and_error() {
        let sender: jid::FullJid = "sender@example.test/device".parse().expect("sender");
        let mut offered = xmpp_parsers::message::Message::new(Some(
            "room@muc.example.test".parse().expect("room"),
        ));
        offered.from = Some(sender.clone().into());
        offered.id = Some(xmpp_parsers::message::Id("offered-id".to_owned()));
        offered.bodies.insert(
            xmpp_parsers::message::Lang::default(),
            "original body".into(),
        );
        let envelope = crate::ingress_substrate::MessageEnvelope::new(offered);
        let intents = vec![IngressEffectIntent::ErrorReply {
            recipient: sender.clone(),
            error: FrozenStanzaError::new(
                FrozenStanzaErrorType::Cancel,
                StanzaErrorCondition::Conflict,
            ),
        }];
        let plan = recorded_rejection_plan(&envelope, &intents).expect("recorded rejection");
        assert_eq!(plan.intents, intents);
        assert_eq!(plan.plan.len(), 1);
        let Some(Stanza::Message(reply)) = plan.error_reply else {
            panic!("error frame");
        };
        assert_eq!(reply.to, Some(sender.into()));
        assert_eq!(reply.from, envelope.message().to);
        assert_eq!(reply.id, envelope.message().id);
        assert_eq!(reply.bodies, envelope.message().bodies);
        assert_eq!(reply.type_, xmpp_parsers::message::MessageType::Error);
        let expected: minidom::Element = FrozenStanzaError::new(
            FrozenStanzaErrorType::Cancel,
            StanzaErrorCondition::Conflict,
        )
        .to_xmpp()
        .into();
        assert_eq!(reply.payloads.last(), Some(&expected));
    }
}
