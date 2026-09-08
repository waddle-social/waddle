use super::*;
use kameo::actor::Spawn;
use waddle_server::ingress::{
    effects::{direct::ExternalDirectEffect, ProjectionRef},
    execute::execute_effects,
    Deps, ExternalEffect, ExternalOutcome, ImmediateSink,
};
use waddle_xmpp::{
    inbox::{ConversationKind, InboxEntry},
    ingress::{EffectMessageIdentity, InboxProjectionMutation},
    registry::{ConnectionRegistry, RegisterUserResource, UserRegistryActor},
    Stanza,
};

async fn two_resource_inbox_push(fixture: IngressFixture) {
    let registry = ConnectionRegistry::new();
    let users = UserRegistryActor::spawn(UserRegistryActor::new());
    let owner = fixture.principal.bare_jid().clone();
    let mut resources = ["phone", "laptop"]
        .map(|resource| owner.with_resource_str(resource).expect("resource"))
        .to_vec();
    resources.sort_by_key(ToString::to_string);
    let mut receivers = Vec::new();
    for resource in &resources {
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        registry.register_with_carbons(resource.clone(), sender, false);
        users
            .ask(RegisterUserResource {
                entry: registry.get_entry(resource).expect("registered resource"),
                jid: resource.clone(),
            })
            .await
            .expect("register inbox recipient");
        receivers.push(receiver);
    }
    let mut submission = archive_plan(
        &fixture,
        Some("settlement-inbox-push"),
        "two-resource inbox push",
        "settlement-inbox-archive",
    );
    let entry = InboxEntry::new(
        "juliet@example.com".parse().expect("peer"),
        ConversationKind::Direct,
        "settlement-inbox-archive",
        chrono::Utc::now().timestamp_millis(),
    );
    submission
        .plan
        .intents
        .push(IngressEffectIntent::InboxProject {
            owner: owner.clone(),
            mutation: InboxProjectionMutation::Direct {
                entry: entry.clone(),
                increment_unread: true,
            },
        });
    let projection = ProjectionRef(submission.plan.plan.len());
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ProjectInbox {
                owner: owner.clone(),
                entry: Box::new(entry),
                increment_unread: true,
            },
        ))));
    let receipt = IngressEffectIntent::RouteDirect {
        recipient: owner.clone(),
        fanout: resources.to_vec(),
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    };
    submission.plan.intents.push(receipt.clone());
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
                owner,
                projection,
                receipt: Some(Box::new(receipt)),
            }),
        )));
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit projection and frozen push fanout");
    assert_eq!(decision.external.len(), 1);
    assert_eq!(decision.external_receipts[0].len(), 1);
    assert!(
        decision.arm_owned_receipts.is_empty(),
        "multi-resource inbox pushes retain generic receipt ownership"
    );
    let mut deps = Deps::new(&registry, "example.com");
    deps.user_registry = Some(&users);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(report.receipt_failures.is_empty());
    assert!(report.terminalization_failure.is_none());
    for receiver in &mut receivers {
        let Stanza::Message(message) = receiver.try_recv().expect("inbox push").stanza else {
            panic!("inbox push must be a message");
        };
        assert!(message
            .payloads
            .iter()
            .any(|payload| payload.is("push", waddle_xmpp::xep::xep0430::NS_WADDLE_INBOX)));
        assert!(receiver.try_recv().is_err(), "one push per resource");
    }
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1,
        "generic fanout completion terminalizes the message"
    );
    fixture.close().await;
}

#[tokio::test]
async fn ingress_settlement_two_resource_inbox_push_sqlite() {
    two_resource_inbox_push(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_settlement_two_resource_inbox_push_postgres() {
    if let Some(fixture) = IngressFixture::postgres("settlement_inbox").await {
        two_resource_inbox_push(fixture).await;
    }
}
