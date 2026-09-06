//! Phase-A entry point for early message handlers and interpreter feedback.
use super::{
    effects::{
        Effect, EffectSink, ExternalEffect, IngressPlan, PlanRejection, PlanSink,
        PolicyDeniedReason,
    },
    Deps, XmppStateMachine,
};
use waddle_xmpp::{
    protocol::{InboundEvent, InboundFrame},
    Stanza,
};
use xmpp_parsers::message::{Message, MessageType};

pub fn build_plan_deps<'a>(deps: &Deps<'a>, sink: &'a PlanSink) -> Deps<'a> {
    let mut planned = deps.clone();
    planned.effects = sink;
    planned
}

/// Sanitize and plan an ingress message, including early handlers, asynchronous
/// read feedback and transient recipient passes. No connection authority is changed.
pub async fn plan_message_dispatch(
    sm: &mut XmppStateMachine,
    mut incoming: Message,
    deps: &Deps<'_>,
) -> IngressPlan {
    let sink = PlanSink::new();
    if let Some(sender) = sm.phase().bound_jid() {
        incoming.from = Some(jid::Jid::from(sender.clone()));
    }
    if let Some(sender) = incoming
        .from
        .as_ref()
        .and_then(|jid| jid.try_as_full().ok())
    {
        super::effects::EffectSink::observe_sender(&sink, sender);
    }
    let ingress_sender = incoming
        .from
        .as_ref()
        .and_then(|jid| jid.try_as_full().ok())
        .cloned();
    let capture = crate::ingress::IngressEffectCapture::new();
    sink.observe_message(&incoming);
    let planned = build_plan_deps(deps, &sink).with_ingress_effect_capture(Some(capture.clone()));
    let early = if let Some(sender) = ingress_sender.as_ref() {
        super::super::websocket::handlers::message::dispatch_early_message(
            &mut incoming,
            sender,
            &planned,
        )
        .await
        .is_some()
    } else {
        false
    };
    if !early {
        let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Message(incoming.clone()),
        ))));
        super::super::websocket::replay::drive_interpret_loop(events, sm, &planned).await;
    }
    finish_plan(&sink, &capture, incoming, ingress_sender)
}

pub(super) fn finish_plan(
    sink: &PlanSink,
    capture: &crate::ingress::IngressEffectCapture,
    incoming: Message,
    ingress_sender: Option<jid::FullJid>,
) -> IngressPlan {
    let (plan, room_execution) = sink.take();
    let error_reply = plan.iter().find_map(|effect| {
        let stanza = match &effect.effect {
            Effect::External(ExternalEffect::Frame(stanza)) => stanza.as_ref(),
            Effect::External(ExternalEffect::Delivery(delivery)) => match delivery {
                super::effects::delivery::ExternalDeliveryEffect::UndeliverableBounce { reply } => {
                    reply.as_ref()
                }
                super::effects::delivery::ExternalDeliveryEffect::RouteToPeer {
                    stanza, ..
                }
                | super::effects::delivery::ExternalDeliveryEffect::RelayFullJid {
                    stanza, ..
                }
                | super::effects::delivery::ExternalDeliveryEffect::RelayBareJid {
                    stanza, ..
                }
                | super::effects::delivery::ExternalDeliveryEffect::QueueDetached {
                    stanza, ..
                } => stanza.as_ref(),
                _ => return None,
            },
            _ => return None,
        };
        match stanza {
            Stanza::Message(message)
                if message.type_ == MessageType::Error && message.to == incoming.from =>
            {
                Some(stanza.clone())
            }
            _ => None,
        }
    });
    let snapshot = capture.snapshot();
    let sanitized_message = sink.message().unwrap_or(incoming);
    if snapshot.overflowed {
        return reject_capture_overflow(sanitized_message, ingress_sender);
    }
    IngressPlan {
        rejection: sink.rejection(),
        plan,
        intents: snapshot.intents,
        sanitized_message,
        error_reply,
        room_execution,
    }
}

/// Reject digest-level semantic errors before invoking handlers or planning effects.
pub(crate) fn reject_malformed_message(mut message: Message, sender: &jid::FullJid) -> IngressPlan {
    message.from = Some(sender.clone().into());
    // RFC 6120 §8.3.1: never send an error in response to an error stanza.
    // Commit responsibility for discarding it without recording a reply obligation.
    if message.type_ == MessageType::Error {
        return IngressPlan {
            rejection: None,
            plan: vec![],
            intents: vec![],
            sanitized_message: message,
            error_reply: None,
            room_execution: super::effects::RoomExecutionPath::None,
        };
    }
    let error = xmpp_parsers::stanza_error::StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Modify,
        xmpp_parsers::stanza_error::DefinedCondition::BadRequest,
        "en",
        "The message contains malformed protocol payloads.",
    );
    let reply =
        waddle_xmpp::protocol::handlers::errors::message_error_reply(&message, error.clone());
    let reply = Stanza::Message(reply);
    IngressPlan {
        rejection: Some(PlanRejection::SemanticMalformed(
            super::effects::SemanticMalformedReason::MalformedPayload,
        )),
        plan: vec![super::effects::PlannedEffect::new(Effect::External(
            ExternalEffect::Frame(Box::new(reply.clone())),
        ))],
        intents: vec![waddle_xmpp::ingress::IngressEffectIntent::ErrorReply {
            recipient: sender.clone(),
            error: waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error)
                .expect("server error is typed"),
        }],
        sanitized_message: message,
        error_reply: Some(reply),
        room_execution: super::effects::RoomExecutionPath::None,
    }
}

/// The retained capture is bounded. Never return an executable plan whose
/// obligations were discarded by that bound; Phase B can commit this standard
/// rejection instead of accepting a message with incomplete intents.
fn reject_capture_overflow(message: Message, sender: Option<jid::FullJid>) -> IngressPlan {
    let error = super::resource_constraint_error("The message exceeds the ingress planning limit.");
    let mut reply =
        waddle_xmpp::protocol::handlers::errors::message_error_reply(&message, error.clone());
    reply.to = sender.map(jid::Jid::from);
    let intents = reply
        .to
        .as_ref()
        .and_then(|jid| jid.try_as_full().ok())
        .map(
            |recipient| waddle_xmpp::ingress::IngressEffectIntent::ErrorReply {
                recipient: recipient.clone(),
                error: waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error)
                    .expect("server error is typed"),
            },
        )
        .into_iter()
        .collect();
    let reply = Stanza::Message(reply);
    IngressPlan {
        rejection: Some(PlanRejection::PolicyDenied(
            PolicyDeniedReason::CaptureOverflow,
        )),
        plan: vec![super::effects::PlannedEffect::new(Effect::External(
            ExternalEffect::Frame(Box::new(reply.clone())),
        ))],
        intents,
        sanitized_message: message,
        error_reply: Some(reply),
        room_execution: super::effects::RoomExecutionPath::None,
    }
}

/// Capture sender preparation even when storage hints suppress archive events.
/// Recipient copies and occupant-addressed room reflections must not replace
/// the ingress target envelope; the room chain records its own final prototype.
pub(super) fn observe_message(deps: &Deps<'_>, event: &super::OutboundEvent, depth: u8) {
    if !deps.effects.is_planning() || depth != 0 {
        return;
    }
    let Some(original) = deps.effects.message() else {
        return;
    };
    use super::OutboundEvent;
    let candidate = match event {
        OutboundEvent::DispatchToRoom { message, .. }
        | OutboundEvent::ArchiveDirect { message, .. }
        | OutboundEvent::SendCarbons { message, .. } => Some(message.as_ref()),
        OutboundEvent::RouteToConnection { stanza, .. } | OutboundEvent::SendStanza(stanza) => {
            match stanza.as_ref() {
                Stanza::Message(message) => Some(message),
                _ => None,
            }
        }
        _ => None,
    };
    let Some(message) = candidate else {
        return;
    };
    if message.type_ == MessageType::Error && message.to == original.from {
        let mut sanitized = message.clone();
        sanitized.to = original.to;
        sanitized.from = original.from;
        sanitized.type_ = original.type_;
        sanitized
            .payloads
            .retain(|payload| payload.name() != "error");
        deps.effects.observe_message(&sanitized);
    } else if message.to == original.to && message.from == original.from {
        if let Some(sender) = message.from.as_ref().and_then(|jid| jid.try_as_full().ok()) {
            deps.effects.observe_sender(sender);
        }
        deps.effects.observe_message(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::protocol::StanzaDispatcher;

    fn machine() -> XmppStateMachine {
        let mut dispatcher = StanzaDispatcher::new();
        waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
        let mut sm = XmppStateMachine::new("example.com", dispatcher);
        sm.transition_to_ready("alice@example.com/web".parse().expect("sender"), false);
        sm
    }

    fn message() -> Message {
        let mut message = Message::new(Some("bob@example.com/web".parse().expect("recipient")));
        message.type_ = MessageType::Chat;
        message
            .bodies
            .insert(Default::default(), "hello".to_owned());
        message
    }

    #[tokio::test]
    async fn client_authored_inbox_plan_rejects_with_one_frame() {
        let registry = waddle_xmpp::registry::ConnectionRegistry::new();
        let deps = Deps::registry_only(&registry);
        let mut incoming = message();
        incoming
            .payloads
            .push(minidom::Element::builder("result", waddle_xmpp::xep::NS_INBOX).build());
        waddle_xmpp_core::xep0359::add_stanza_id(
            &mut incoming,
            &waddle_xmpp_core::xep0359::StanzaId::new(
                "untrusted-client-id",
                "alice@example.com".parse().expect("archive"),
            ),
        );
        let plan = plan_message_dispatch(&mut machine(), incoming, &deps).await;
        assert_eq!(
            plan.rejection,
            Some(PlanRejection::SemanticMalformed(
                super::super::effects::SemanticMalformedReason::ClientAuthoredInboxPayload,
            ))
        );
        assert_eq!(plan.plan.len(), 1);
        assert!(matches!(
            plan.plan[0].effect,
            Effect::External(ExternalEffect::Frame(_))
        ));
        assert!(plan.error_reply.is_some());
        assert!(plan.plan[0].dependencies.is_empty());
        assert_eq!(plan.intents.len(), 1);
        assert!(matches!(
            plan.intents[0],
            waddle_xmpp::ingress::IngressEffectIntent::ErrorReply { .. }
        ));
    }

    #[tokio::test]
    async fn nonarchivable_envelope_retains_sender_canonicalization() {
        let registry = waddle_xmpp::registry::ConnectionRegistry::new();
        let deps = Deps::registry_only(&registry);
        let mut message = message();
        message.payloads.push(
            xmpp_parsers::minidom::Element::builder(
                "no-store",
                waddle_xmpp::xep::xep0334::NS_HINTS,
            )
            .build(),
        );
        let plan = plan_message_dispatch(&mut machine(), message, &deps).await;
        assert_eq!(
            plan.sanitized_message.from,
            Some("alice@example.com/web".parse().expect("sender"))
        );
        assert!(waddle_xmpp_core::xep0359::extract_stanza_id_by(
            &plan.sanitized_message,
            &"alice@example.com".parse().expect("archive")
        )
        .is_some());
    }

    #[tokio::test]
    async fn semantic_error_keeps_the_sanitized_ingress_envelope() {
        let registry = waddle_xmpp::registry::ConnectionRegistry::new();
        let deps = Deps::registry_only(&registry);
        let mut message = message();
        message.payloads.push(
            xmpp_parsers::minidom::Element::builder(
                "extensions",
                waddle_extensions::FRAMEWORK_NAMESPACE,
            )
            .build(),
        );
        let plan = plan_message_dispatch(&mut machine(), message, &deps).await;
        assert!(plan.error_reply.is_some());
        assert!(!plan
            .sanitized_message
            .payloads
            .iter()
            .any(
                |payload| payload.ns() == waddle_extensions::FRAMEWORK_NAMESPACE
                    || payload.name() == "error"
            ));
        assert_eq!(
            plan.sanitized_message.to,
            Some("bob@example.com/web".parse().expect("recipient"))
        );
    }
    #[test]
    fn overflow_rejection_contains_only_the_standard_error_obligation() {
        let mut input = message();
        input.from = Some("alice@example.com/web".parse().expect("sender"));
        let plan = reject_capture_overflow(
            input,
            Some("alice@example.com/web".parse().expect("sender")),
        );
        assert_eq!(plan.plan.len(), 1);
        assert_eq!(plan.intents.len(), 1);
        assert!(matches!(
            plan.intents[0],
            waddle_xmpp::ingress::IngressEffectIntent::ErrorReply { .. }
        ));
        assert!(plan.error_reply.is_some());
    }
    #[test]
    fn room_overflow_replies_to_the_ingress_sender_not_the_occupant_address() {
        let mut input = message();
        input.type_ = MessageType::Groupchat;
        input.to = Some("room@muc.example.com".parse().expect("room"));
        input.from = Some("room@muc.example.com/alice".parse().expect("occupant"));
        let sender = "alice@example.com/web"
            .parse::<jid::FullJid>()
            .expect("sender");
        let plan = reject_capture_overflow(input, Some(sender.clone()));
        assert!(
            matches!(plan.error_reply, Some(Stanza::Message(reply)) if reply.to == Some(sender.into()))
        );
    }
}
