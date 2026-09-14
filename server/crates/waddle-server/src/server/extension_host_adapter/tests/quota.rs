//! Host refusal follows the settled offline quota result, with no new transport frame.
use super::super::ExtensionHostAdapterError;
use super::direct_ingress::{adapter, invocation, plugin, request};
use crate::{
    ingress::test_support::IngressFixture, pending_delivery::DatabasePendingDeliveryStorage,
};
use std::{sync::Arc, time::Duration};
use waddle_xmpp::pending_delivery::QuotaPolicy;

async fn quota_refusal(f: IngressFixture) {
    let mut adapter = adapter(&f).await;
    Arc::get_mut(&mut adapter.state)
        .expect("unique state")
        .deps
        .protocol
        .pending_delivery_storage = Arc::new(
        DatabasePendingDeliveryStorage::from_database(
            f.db.clone(),
            QuotaPolicy::CountCap { max_rows: 0 },
        )
        .await
        .expect("zero quota storage"),
    );
    let (socket_tx, mut socket_rx) = tokio::sync::mpsc::channel(8);
    crate::server::routes::websocket::tests::register_test_connection(
        &adapter.state,
        &invocation().actor_jid,
        socket_tx,
    )
    .await;
    use waddle_extensions::{host_tools as host, DisplayText, WaddleId};
    let context = host::InvocationContext {
        waddle_id: WaddleId::new("quota-boundary").expect("waddle"),
        plugin_id: plugin(),
        requester: Some(invocation().actor_jid.to_bare()),
        source_room: None,
        kind: host::InvocationKind::MessageHook,
        provider_room_grants: vec![],
    };
    let error = host::ExtensionHostTools::send_message(
        &adapter,
        &context,
        host::SendMessageRequest {
            target: host::MessageTarget::Direct("juliet@example.com".parse().expect("recipient")),
            body: DisplayText::new("host quota refusal").expect("body"),
            thread_id: None,
            reply_to: None,
            markup: vec![],
            extensions: None,
        },
    )
    .await
    .expect_err("the real host tool must receive the refusal");
    assert_eq!(error.code, host::HostToolErrorCode::Denied);
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("notification_candidates").await, 0);
    assert_eq!(f.count("ingress_effect_receipts WHERE kind = 22").await, 1);
    assert!(
        matches!(
            socket_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "the real host tool must consume its own refusal"
    );
    let result = adapter
        .send_message(&invocation(), request("extension-quota"))
        .await;
    let Err(ExtensionHostAdapterError::Rejected(error)) = result else {
        panic!("quota refusal must reach the host: {result:?}");
    };
    assert_eq!(error.type_, xmpp_parsers::stanza_error::ErrorType::Cancel);
    assert_eq!(
        error.defined_condition,
        xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        2
    );
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("notification_candidates").await, 0);
    assert_eq!(f.count("ingress_effect_receipts WHERE kind = 22").await, 2);
    assert!(
        matches!(
            socket_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "an unrelated client bound to the host's synthetic JID must not receive its quota bounce"
    );
    // Refusal receipts are terminal; a same-origin replay cannot requeue work.
    adapter
        .send_message(&invocation(), request("extension-quota"))
        .await
        .expect("settled replay accepted");
    assert_eq!(f.count("ingress_messages").await, 2);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        2
    );
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("notification_candidates").await, 0);
    assert_eq!(f.count("ingress_effect_receipts WHERE kind = 22").await, 2);
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_direct_quota_sqlite() {
    quota_refusal(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_direct_quota_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_direct_quota").await {
        quota_refusal(f).await;
    }
}
