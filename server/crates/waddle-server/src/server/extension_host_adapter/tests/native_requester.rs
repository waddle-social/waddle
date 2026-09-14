//! Native SCRAM accounts have the same extension admission authority as OIDC accounts.
use super::{direct_ingress, groupchat_ingress::GroupchatFixture};
use crate::{
    ingress::test_support::IngressFixture,
    ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository},
};
use waddle_extensions::{host_tools as host, DisplayText, WaddleId};

fn context(source_room: Option<jid::BareJid>) -> host::InvocationContext {
    host::InvocationContext {
        waddle_id: WaddleId::new("native-requester").expect("waddle"),
        plugin_id: direct_ingress::plugin(),
        requester: Some("romeo@example.com".parse().expect("requester")),
        source_room,
        kind: host::InvocationKind::MessageHook,
        provider_room_grants: vec![],
    }
}

fn request(target: host::MessageTarget) -> host::SendMessageRequest {
    host::SendMessageRequest {
        target,
        body: DisplayText::new("native requester body").expect("body"),
        thread_id: None,
        reply_to: None,
        markup: vec![],
        extensions: None,
    }
}

async fn assert_canonical_settled(f: &IngressFixture) -> xmpp_parsers::message::Message {
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    let key = waddle_xmpp::ingress::MessageKey::from_storage(
        f.optional_text("SELECT CAST(message_key AS TEXT) FROM ingress_messages")
            .await
            .expect("message key")
            .parse()
            .expect("uuid"),
    );
    let mut tx = f.uow.begin().await.expect("inspect canonical message");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load envelope")
        .expect("canonical message");
    assert!(waddle_xmpp_core::xep0359::extract_origin_id(envelope.message()).is_some());
    assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("complete receipts"));
    tx.commit().await.expect("inspection commit");
    assert!(f.count("ingress_effect_receipts").await > 0);
    envelope.message().clone()
}

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
    let response = host::ExtensionHostTools::send_message(
        &adapter,
        &context(None),
        request(host::MessageTarget::Direct(
            "juliet@example.com".parse().expect("recipient"),
        )),
    )
    .await
    .expect("native requester direct admission through host tools");
    let message = assert_canonical_settled(&f).await;
    assert_eq!(
        waddle_xmpp_core::xep0359::extract_origin_id(&message)
            .expect("origin id")
            .as_str(),
        response.stanza_id.as_str()
    );
    assert_eq!(
        message.from,
        Some("romeo@example.com/extension-host".parse().expect("sender"))
    );
    assert_eq!(f.count("pending_delivery").await, 1);
    assert_eq!(f.count("notification_candidates").await, 1);
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
    crate::server::routes::websocket::tests::create_test_server_owner_session(
        &fixture.adapter.state,
        "romeo",
    )
    .await;
    make_requester_native(&f).await;
    let response = host::ExtensionHostTools::send_message(
        &fixture.adapter,
        &context(Some(fixture.room.clone())),
        request(host::MessageTarget::Muc(fixture.room.clone())),
    )
    .await
    .expect("native requester groupchat admission through host tools");
    let message = assert_canonical_settled(&f).await;
    assert_eq!(
        waddle_xmpp_core::xep0359::extract_stanza_id_by(&message, &fixture.room.clone().into())
            .as_deref(),
        Some(response.stanza_id.as_str())
    );
    assert_eq!(message.type_, xmpp_parsers::message::MessageType::Groupchat);
    assert_eq!(
        message.from.as_ref().map(jid::Jid::to_bare),
        Some(fixture.room.clone())
    );
    assert_eq!(
        f.count("groupchat_notification_recovery WHERE recipient_bare_jid = 'juliet@example.com'")
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

async fn missing_requester_denied(f: IngressFixture) {
    let adapter = direct_ingress::adapter(&f).await;
    f.execute("DELETE FROM users WHERE jid = 'romeo@example.com'", ())
        .await;
    assert_eq!(
        f.count("native_users WHERE username = 'romeo' AND domain = 'example.com'")
            .await,
        0
    );
    let error = host::ExtensionHostTools::send_message(
        &adapter,
        &context(None),
        request(host::MessageTarget::Direct(
            "juliet@example.com".parse().expect("recipient"),
        )),
    )
    .await
    .expect_err("unregistered requester must be denied");
    assert_eq!(error.code, host::HostToolErrorCode::Denied);
    assert_eq!(f.count("ingress_messages").await, 0);
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("notification_candidates").await, 0);
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

#[tokio::test]
async fn extension_native_requester_missing_denied_sqlite() {
    missing_requester_denied(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn extension_native_requester_missing_denied_postgres() {
    if let Some(f) = IngressFixture::postgres("native_missing").await {
        missing_requester_denied(f).await;
    }
}
