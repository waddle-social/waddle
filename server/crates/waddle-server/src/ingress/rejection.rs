use super::decision::IngressDecisionClass;
use crate::{
    ingress_uow::IngressUowError,
    server::routes::interpret::effects::{
        Effect, ExternalEffect, IngressPlan, PlanRejection, PlannedEffect, PolicyDeniedReason,
        RoomExecutionPath,
    },
};
use waddle_xmpp::{
    ingress::{FrozenStanzaError, FrozenStanzaErrorType, IngressEffectIntent},
    Stanza, StanzaErrorCondition,
};

pub(super) fn planned_rejection(plan: &IngressPlan) -> Option<IngressDecisionClass> {
    plan.rejection.as_ref().map(|rejection| match rejection {
        PlanRejection::AuthorizationDenied(_) => IngressDecisionClass::AuthorizationDenied,
        PlanRejection::SemanticMalformed(_) => IngressDecisionClass::SemanticMalformed,
        PlanRejection::PolicyDenied(PolicyDeniedReason::CaptureOverflow) => {
            IngressDecisionClass::CaptureOverflow
        }
        PlanRejection::PolicyDenied(_) => IngressDecisionClass::PolicyDenied,
    })
}
pub(super) fn rejection_plan(
    plan: &IngressPlan,
    class: IngressDecisionClass,
    sender: &jid::FullJid,
) -> Result<IngressPlan, IngressUowError> {
    let mut rejected = plan.clone();
    rejected.plan.clear();
    rejected
        .intents
        .retain(|intent| matches!(intent, IngressEffectIntent::ErrorReply { .. }));
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

pub(super) fn is_recorded_rejection(intents: &[IngressEffectIntent]) -> bool {
    !intents.is_empty()
        && intents
            .iter()
            .all(|intent| matches!(intent, IngressEffectIntent::ErrorReply { .. }))
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
        let IngressEffectIntent::ErrorReply { recipient, error } = intent else {
            return Err(IngressUowError::EffectIntentConflict);
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
    #[test]
    fn error_intent_alone_does_not_classify_a_plan_rejection() {
        let sender: jid::FullJid = "sender@example.test/device".parse().expect("sender");
        let envelope = crate::ingress_substrate::MessageEnvelope::new(
            xmpp_parsers::message::Message::new(None),
        )
        .expect("envelope");
        let intents = vec![IngressEffectIntent::ErrorReply {
            recipient: sender,
            error: FrozenStanzaError::new(
                FrozenStanzaErrorType::Cancel,
                StanzaErrorCondition::Forbidden,
            ),
        }];
        let plan = recorded_rejection_plan(&envelope, &intents).expect("recorded plan");
        assert_eq!(planned_rejection(&plan), None);
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
        let envelope = crate::ingress_substrate::MessageEnvelope::new(offered).expect("envelope");
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
