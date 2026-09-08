use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::notification_activity::{
    NotificationActivityReader, NotificationActivityStore, NotificationChatState,
};
use crate::server::routes::interpret::effects::PlanSuppressionPolicy;
use waddle_xmpp::{
    ingress::{IngressEffectIntent, NotificationActivityMutation},
    registry::ConnectionRegistry,
};

async fn gone_preserves_newer_activity(fixture: IngressFixture) {
    let store = NotificationActivityStore::new(fixture.db.clone())
        .await
        .expect("activity store");
    let owner = "alice@example.com".parse().expect("owner");
    let room = "room@muc.example.com".parse().expect("room");
    store
        .record_chat_state_gone(&owner, &room, 2000)
        .await
        .expect("gone t2");
    store
        .record_chat_state(&owner, &room, NotificationChatState::Active, 3000)
        .await
        .expect("active t3");
    store
        .record_chat_state_gone(&owner, &room, 2000)
        .await
        .expect("replay gone t2");
    let activity = store
        .read_activity(&owner, &room)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 3000);
    assert_eq!(
        activity.last_chat_state,
        Some(NotificationChatState::Active)
    );
    store
        .record_chat_state_gone(&owner, &room, 4000)
        .await
        .expect("gone t4");
    let activity = store
        .read_activity(&owner, &room)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 0);
    assert_eq!(activity.last_chat_state, Some(NotificationChatState::Gone));
    fixture.close().await;
}

async fn receipted_activity_is_not_replayed(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("activity-retry"), "activity");
    let owner = submission.sender.to_bare();
    let room = "room@muc.example.com".parse().expect("room");
    let mutation = NotificationActivityMutation::ChatStateGone {
        conversation: room,
        committed_at_ms: 2000,
    };
    submission.plan.intents = vec![IngressEffectIntent::NotificationActivityPreview {
        owner: owner.clone(),
        mutation: mutation.clone(),
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(ExternalEffect::Direct(
        ExternalDirectEffect::NotificationActivity { owner, mutation },
    )))
    .with_suppression(PlanSuppressionPolicy::Always)];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit activity");
    assert_eq!(first.external.len(), 1);
    assert_eq!(first.external_receipts[0].len(), 1);
    let canonical = first.message_key.expect("canonical key");
    let registry = ConnectionRegistry::new();
    let socket = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(socket.as_ref());
    let completed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(completed.outcomes[0].1, ExternalOutcome::Done);
    assert!(completed.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(terminalize_if_complete(&fixture.uow, canonical)
        .await
        .expect("complete"));

    let duplicate = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("duplicate activity");
    assert_eq!(
        duplicate.external.len(),
        1,
        "reconciliation restores recorded activity"
    );
    assert!(duplicate.receipts_pending.is_empty());
    // An unavailable store would fail if the historical mutation executes again.
    let unavailable = Deps::new(&registry, "example.com");
    let skipped = execute_effects(
        &fixture.uow,
        &fixture.db,
        &duplicate,
        &ImmediateSink,
        &unavailable,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(skipped.outcomes[0].1, ExternalOutcome::Done);
    assert!(skipped.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_gone_preserves_newer_activity() {
    gone_preserves_newer_activity(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_gone_preserves_newer_activity() {
    if let Some(fixture) = IngressFixture::postgres("gone_monotonic").await {
        gone_preserves_newer_activity(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_receipted_activity_is_not_replayed() {
    receipted_activity_is_not_replayed(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_receipted_activity_is_not_replayed() {
    if let Some(fixture) = IngressFixture::postgres("activity_receipt").await {
        receipted_activity_is_not_replayed(fixture).await;
    }
}
