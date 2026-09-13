//! Native SCRAM accounts have the same extension admission authority as OIDC accounts.
use super::{direct_ingress, groupchat_ingress::GroupchatFixture};
use crate::ingress::test_support::IngressFixture;

async fn make_requester_native(f: &IngressFixture) {
    f.execute("DELETE FROM users WHERE jid = 'romeo@example.com'", ())
        .await;
    f.execute(
        "INSERT INTO native_users (username, domain, password_hash, salt, stored_key, server_key) VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params!["romeo", "example.com", "unused", "unused", vec![0_u8; 32], vec![0_u8; 32]],
    ).await;
    assert_eq!(f.count("users WHERE jid = 'romeo@example.com'").await, 0);
}

async fn native_direct(f: IngressFixture) {
    let adapter = direct_ingress::adapter(&f).await;
    make_requester_native(&f).await;
    adapter
        .send_message(
            &direct_ingress::invocation(),
            direct_ingress::request("native-direct"),
        )
        .await
        .expect("native requester direct admission");
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(std::time::Duration::from_secs(10))
            .await
    );
    drop(adapter);
    f.close().await;
}

async fn native_groupchat(f: IngressFixture) {
    let fixture = GroupchatFixture::new(&f).await;
    let session = crate::server::routes::websocket::tests::create_test_server_owner_session(
        &fixture.adapter.state,
        "romeo",
    )
    .await;
    make_requester_native(&f).await;
    let mut invocation = direct_ingress::invocation();
    invocation.session = Some(session);
    invocation.source_room = Some(fixture.room.clone());
    fixture
        .adapter
        .send_message(&invocation, fixture.request("native-groupchat"))
        .await
        .expect("native requester groupchat admission");
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_native_requester_direct_sqlite() {
    native_direct(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_native_requester_direct_postgres() {
    if let Some(f) = IngressFixture::postgres("native_direct").await {
        native_direct(f).await;
    }
}
#[tokio::test]
async fn extension_native_requester_groupchat_sqlite() {
    native_groupchat(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_native_requester_groupchat_postgres() {
    if let Some(f) = IngressFixture::postgres("native_groupchat").await {
        native_groupchat(f).await;
    }
}
