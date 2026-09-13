//! Host refusal follows the settled offline quota result, with no new transport frame.
use super::super::ExtensionHostAdapterError;
use super::direct_ingress::{adapter, invocation, request};
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
        1
    );
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("notification_candidates").await, 0);
    assert_eq!(f.count("ingress_effect_receipts WHERE kind = 22").await, 1);
    // Refusal receipts are terminal; a same-origin replay cannot requeue work.
    adapter
        .send_message(&invocation(), request("extension-quota"))
        .await
        .expect("settled replay accepted");
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
