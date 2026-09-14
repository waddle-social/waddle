//! Committed conflicts retain acceptance while host frame settlement is pending.
use super::{groupchat_ingress::GroupchatFixture, host_boundary};
use crate::ingress::test_support::IngressFixture;
use std::time::Duration;

async fn conflict_deadline(f: IngressFixture) {
    let mut fixture = GroupchatFixture::new(&f).await;
    fixture
        .send("conflict-origin")
        .await
        .expect("first message");
    fixture.drain();
    host_boundary::fail_receipts(&f).await;
    let mut conflict = fixture.request("conflict-origin");
    conflict.body = "different offered body under the same origin".to_owned();
    let response = tokio::time::timeout(
        Duration::from_secs(4),
        fixture
            .adapter
            .send_message(&fixture.invocation(), conflict),
    )
    .await
    .expect("internal deadline returns before settlement budget")
    .expect("committed conflict is accepted while its frame cannot settle");
    assert_eq!(response.as_str(), "conflict-origin");
    assert_eq!(f.count("ingress_messages").await, 2);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NULL").await,
        1
    );
    assert!(
        fixture.drain().is_empty(),
        "conflict never reaches room occupants"
    );
    host_boundary::restore_receipts(&f).await;
    assert!(
        fixture
            .adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        2
    );
    drop(fixture);
    f.close().await;
}

#[tokio::test]
async fn extension_groupchat_conflict_deadline_sqlite() {
    conflict_deadline(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn extension_groupchat_conflict_deadline_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_conflict_deadline").await {
        conflict_deadline(f).await;
    }
}
