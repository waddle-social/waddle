use super::*;
use crate::ingress::test_support::IngressFixture;
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::stream_management::{SmIngressFrameReceipt, SmIngressReceiptKind};

async fn ingress_receipts_survive_sm_storage(fixture: IngressFixture) {
    let storage = DatabaseSmPersistence::open(Some(fixture.db.database_url()))
        .await
        .expect("SM storage");
    let session = fixture_session("ingress-frame-receipts");
    storage
        .upsert_session(session.clone())
        .await
        .expect("session");
    let mut frame = fixture_unacked(session.stream_id.as_str(), 11);
    frame.ingress_receipts = vec![SmIngressFrameReceipt {
        message_key: MessageKey::new(),
        kind: SmIngressReceiptKind::from_storage(1),
        semantic_identity_hash: [19; 32],
    }];
    // Nested relay owner identities precede the origin dispatch identity and
    // must retain that order across detach and cross-node reload.
    frame.ingress_receipts.insert(
        0,
        SmIngressFrameReceipt {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(23),
            semantic_identity_hash: [27; 32],
        },
    );
    storage.append_unacked(frame.clone()).await.expect("append");
    let rows = storage
        .list_unacked(&session.stream_id)
        .await
        .expect("load");
    assert_eq!(rows[0].ingress_receipts, frame.ingress_receipts);
    storage
        .store_session_atomic(session.clone(), vec![frame.clone()])
        .await
        .expect("atomic detached snapshot");
    let loaded = storage
        .list_all_sessions_with_unacked()
        .await
        .expect("joined snapshot");
    let (_, rows) = loaded
        .iter()
        .find(|(loaded, _)| loaded.stream_id == session.stream_id)
        .expect("detached session");
    assert_eq!(rows[0].ingress_receipts, frame.ingress_receipts);
    storage
        .ack_through(&session.stream_id, 11)
        .await
        .expect("ack");
    assert!(storage
        .list_unacked(&session.stream_id)
        .await
        .expect("acked queue")
        .is_empty());
    drop(storage);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_ingress_frame_receipts_survive_sm_storage() {
    ingress_receipts_survive_sm_storage(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_ingress_frame_receipts_survive_sm_storage() {
    if let Some(fixture) = IngressFixture::postgres("sm_frame_receipts").await {
        ingress_receipts_survive_sm_storage(fixture).await;
    }
}
