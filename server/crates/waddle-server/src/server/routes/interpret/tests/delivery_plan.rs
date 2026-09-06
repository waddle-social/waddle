use super::super::effects::delivery::ExternalDeliveryEffect;
use super::super::effects::{
    Effect, EffectOutcome, EffectSink, ExternalEffect, ImmediateSink, PlanSink, PlannedEffect,
};
use super::*;
use waddle_xmpp::stream_management::SmSessionRegistry;

fn reflection(target: &jid::FullJid) -> Stanza {
    let mut message = chat_msg(
        jid("room@conference.example.com/alice"),
        target.clone().into(),
        "reflection",
    );
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    Stanza::Message(message)
}

#[tokio::test]
async fn detached_groupchat_reflection_plans_replay_without_appending() {
    let registry = test_registry();
    let target: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    let sessions = Arc::new(InMemorySmSessionRegistry::new());
    sessions
        .store_session(detached_dm_session("groupchat-plan", &target))
        .await
        .expect("session");
    let sink = PlanSink::new();
    let deps = Deps {
        effects: &sink,
        sm_session_registry: Some(&sessions),
        ..Deps::registry_only(&registry)
    };
    interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: target.clone().into(),
            stanza: Box::new(reflection(&target)),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    let plan = sink.snapshot();
    assert!(plan.iter().any(|effect| matches!(&effect.effect,
        Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. })) if resources == &vec![target.clone()])));
    assert!(!plan.iter().any(|effect| matches!(
        effect.effect,
        Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { .. }
        ))
    )));
    assert!(sessions
        .peek_session("groupchat-plan")
        .await
        .expect("peek")
        .expect("session")
        .unacked_stanzas
        .is_empty());
}

#[tokio::test]
async fn unavailable_groupchat_reflection_stays_silent_during_plan() {
    let registry = test_registry();
    let target: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    let sink = PlanSink::new();
    let deps = Deps {
        effects: &sink,
        ..Deps::registry_only(&registry)
    };
    interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: target.clone().into(),
            stanza: Box::new(reflection(&target)),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    assert!(sink.snapshot().is_empty());
}

#[tokio::test]
async fn unavailable_iq_plans_frozen_error_reply() {
    let registry = test_registry();
    let target: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    let sink = PlanSink::new();
    let deps = Deps {
        effects: &sink,
        ..Deps::registry_only(&registry)
    };
    let iq = Iq::Get {
        from: Some(jid("alice@example.com/web")),
        to: Some(target.clone().into()),
        id: "plan-iq".into(),
        payload: minidom::Element::builder("ping", waddle_xmpp::xep::xep0199::NS_PING).build(),
    };
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: target.into(),
            stanza: Box::new(Stanza::Iq(Box::new(iq))),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    assert!(outcome.frames.is_empty());
    assert!(sink.snapshot().iter().any(|effect| matches!(&effect.effect,
        Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::UndeliverableBounce { reply })) if matches!(reply.as_ref(), Stanza::Iq(iq) if matches!(iq.as_ref(), Iq::Error { .. })))));
}

#[tokio::test]
async fn relay_executor_reports_unavailable_without_local_recipient_fallback() {
    let registry = test_registry();
    let deps = Deps::registry_only(&registry);
    let target: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    for effect in [
        ExternalDeliveryEffect::RelayFullJid {
            origin: None,
            target: target.clone(),
            stanza: Box::new(reflection(&target)),
            call_setup: None,
        },
        ExternalDeliveryEffect::RelayBareJid {
            origin: None,
            target: target.to_bare(),
            stanza: Box::new(reflection(&target)),
        },
    ] {
        let outcome = ImmediateSink
            .execute(
                PlannedEffect::new(Effect::External(ExternalEffect::Delivery(effect))),
                &deps,
            )
            .await;
        assert!(matches!(
            outcome,
            EffectOutcome::Delivery(FullJidDeliveryOutcome::Unavailable)
        ));
    }
}
