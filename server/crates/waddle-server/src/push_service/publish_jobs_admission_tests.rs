use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::push_service::test_support::{notification_item, owner};
use waddle_xmpp::StanzaErrorCondition;

fn assert_condition(error: XmppError, expected: StanzaErrorCondition) {
    assert!(
        matches!(&error, XmppError::Stanza { condition, .. } if *condition == expected),
        "expected {expected:?}, got {error:?}"
    );
}

async fn assert_refused_publication(
    push: &DatabasePushServiceStore,
    node: &str,
    publisher: &BareJid,
    expected: StanzaErrorCondition,
) {
    let service: BareJid = "push.example.com".parse().expect("service");
    let delivery = uuid::Uuid::new_v4();
    let item = notification_item(&delivery.to_string());
    let refusal = push
        .enqueue_canonical_notification_publish_job(
            node, &item, publisher, &service, None, delivery,
        )
        .await
        .expect_err("invalid target refuses canonical publication");
    match refusal {
        PushQueueAcceptanceError::KnownNotAccepted(error) => assert_condition(error, expected),
        other => panic!("invalid target must prove this invocation was refused: {other:?}"),
    }
    assert_condition(
        push.enqueue_notification_publish_job_from_user_server(node, &item, publisher)
            .await
            .expect_err("invalid target refuses wire publication"),
        expected,
    );
    assert!(push
        .delivery_attempts_for_node(node)
        .await
        .expect("attempts")
        .is_empty());
}

async fn missing_node_refuses_publication(fixture: IngressFixture) {
    let push = DatabasePushServiceStore::new(fixture.db.clone())
        .await
        .expect("push store");
    let missing_node = "urn:waddle:push-node:missing";
    let deleted_node = push
        .ensure_node(&owner(), "deleted-node")
        .await
        .expect("node before deletion");
    fixture
        .execute(
            "DELETE FROM push_nodes WHERE node = ?",
            crate::db_params![deleted_node.node()],
        )
        .await;
    for node in [missing_node, deleted_node.node()] {
        assert_refused_publication(&push, node, &owner(), StanzaErrorCondition::ItemNotFound).await;
    }
    assert!(push.queued_publish_jobs().await.expect("queue").is_empty());
    assert_eq!(fixture.count("push_publish_jobs").await, 0);
    assert_eq!(fixture.count("push_delivery_attempts").await, 0);
    drop(push);
    fixture.close().await;
}

async fn existing_node_enforces_admission(fixture: IngressFixture) {
    let push = DatabasePushServiceStore::new(fixture.db.clone())
        .await
        .expect("push store");
    let disabled = push
        .ensure_node(&owner(), "disabled-node")
        .await
        .expect("node");
    push.disable_nodes_for_owner(&owner(), Some(disabled.node()))
        .await
        .expect("disable node");
    assert_refused_publication(
        &push,
        disabled.node(),
        &owner(),
        StanzaErrorCondition::ItemNotFound,
    )
    .await;

    let active = push
        .ensure_node(&owner(), "active-node")
        .await
        .expect("active node");
    let stranger: BareJid = "mallory@example.com".parse().expect("stranger");
    assert_refused_publication(
        &push,
        active.node(),
        &stranger,
        StanzaErrorCondition::Forbidden,
    )
    .await;
    assert_eq!(fixture.count("push_publish_jobs").await, 0);
    assert_eq!(fixture.count("push_delivery_attempts").await, 0);

    let accepted = push
        .enqueue_notification_publish_job_from_user_server(
            active.node(),
            &notification_item("authorized-publication"),
            &owner(),
        )
        .await
        .expect("active node owner can still publish");
    assert!(accepted.queued());
    let queued = push.queued_publish_jobs().await.expect("queue");
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].job_id(), accepted.job_id().to_string());
    assert_eq!(queued[0].node(), active.node());
    assert_eq!(fixture.count("push_delivery_attempts").await, 0);
    drop(push);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_missing_node_refuses_publication() {
    missing_node_refuses_publication(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_missing_node_refuses_publication() {
    if let Some(fixture) = IngressFixture::postgres("push_missing_node").await {
        missing_node_refuses_publication(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_existing_node_enforces_admission() {
    existing_node_enforces_admission(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_existing_node_enforces_admission() {
    if let Some(fixture) = IngressFixture::postgres("push_node_admission").await {
        existing_node_enforces_admission(fixture).await;
    }
}
