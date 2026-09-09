//! A successful append does not prove durable progress when its transaction stalls.
use super::*;
use std::time::Duration;
use waddle_server::ingress::{
    effects::delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
    execute::execute_effects,
    Deps, ExternalEffect, ExternalOutcome, ImmediateSink,
};
use waddle_xmpp::{ingress::EffectMessageIdentity, registry::ConnectionRegistry, Stanza};

async fn progress_lock_contention(fixture: IngressFixture) {
    let resources: Vec<jid::FullJid> = ["juliet@example.com/phone", "juliet@example.com/laptop"]
        .map(|value| value.parse().expect("resource"))
        .to_vec();
    let registry = ConnectionRegistry::new();
    let mut receivers = Vec::new();
    for resource in &resources {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(resource.clone(), sender, false);
        receivers.push(receiver);
    }
    let deps = Deps::new(&registry, "example.com");
    let mut submission = archive_plan(
        &fixture,
        Some("detached-contention"),
        "canonical contention payload",
        "detached-contention-archive",
    );
    let identity = EffectMessageIdentity::capture_ordinal(1);
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RouteDirect {
            recipient: resources[0].to_bare(),
            fanout: resources.clone(),
            route_identity: identity.clone(),
        });
    for resource in &resources {
        submission
            .plan
            .plan
            .push(PlannedEffect::new(Effect::External(
                ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                    route_identity: Some(identity.clone()),
                    jid: resource.clone(),
                    stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
                    kind: PeerDeliveryKind::RegistryFrame,
                    call_setup: None,
                }),
            )));
    }
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit before competing lock");
    assert_eq!(decision.arm_owned_receipts.len(), 1);
    let mut blocker = fixture.uow.begin().await.expect("competing transaction");
    assert!(CanonicalMessageRepository::lock(
        &mut blocker,
        decision.message_key.expect("message key"),
    )
    .await
    .expect("hold canonical lock"));
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_millis(300),
    )
    .await;
    assert!(report.outcomes.iter().any(|(_, outcome)| {
        matches!(
            outcome,
            ExternalOutcome::Failed | ExternalOutcome::Uncertain
        )
    }));
    receivers[0]
        .try_recv()
        .expect("append precedes progress lock");
    blocker.commit().await.expect("release competing lock");
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0,
    );
    // Depending on the dialect's lock timeout, B may also have appended before
    // its transaction failed. Drain it: neither append has durable progress.
    while receivers[1].try_recv().is_ok() {}
    let retry = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("ordinary duplicate after contention");
    assert_eq!(retry.message_key, decision.message_key);
    assert_eq!(retry.external.len(), 2);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert!(report.terminalization_failure.is_none());
    for receiver in &mut receivers {
        receiver.try_recv().expect("unrecorded append retried");
        assert!(receiver.try_recv().is_err(), "one retry per resource");
    }
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1,
    );
    fixture.close().await;
}

#[tokio::test]
async fn ingress_detached_progress_lock_contention_sqlite() {
    progress_lock_contention(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_detached_progress_lock_contention_postgres() {
    if let Some(fixture) = IngressFixture::postgres("detached_contention").await {
        progress_lock_contention(fixture).await;
    }
}
