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

async fn separate_store_upgrade(fixture: IngressFixture) {
    // Unlike the global store, this configured store never runs V1012.
    let directory = tempfile::tempdir().expect("separate SM directory");
    let schema = format!("separate_sm_{}", uuid::Uuid::new_v4().simple());
    let database_url = match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => {
            format!("sqlite://{}", directory.path().join("sm.db").display())
        }
        crate::db::DatabaseDriver::Postgres => {
            fixture
                .execute(&format!("CREATE SCHEMA {schema}"), ())
                .await;
            let mut url = url::Url::parse(fixture.db.database_url()).expect("Postgres URL");
            let retained: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(key, _)| key != "options")
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect();
            url.query_pairs_mut()
                .clear()
                .extend_pairs(retained)
                .append_pair("options", &format!("-c search_path={schema}"));
            url.to_string()
        }
    };
    let storage = DatabaseSmPersistence::open(Some(&database_url))
        .await
        .expect("separate SM store");
    let session = fixture_session("pre-upgrade");
    storage
        .upsert_session(session.clone())
        .await
        .expect("session");
    let frame = fixture_unacked(session.stream_id.as_str(), 11);
    storage.append_unacked(frame).await.expect("old frame");
    storage
        .execute("ALTER TABLE sm_unacked DROP COLUMN ingress_receipts", ())
        .await
        .expect("pre-upgrade schema");
    drop(storage);
    let storage = DatabaseSmPersistence::open(Some(&database_url))
        .await
        .expect("upgrade configured SM store");
    schema::initialize(&storage)
        .await
        .expect("idempotent upgrade");
    let restored = storage
        .list_all_sessions_with_unacked()
        .await
        .expect("restore old sessions");
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].1.len(), 1);
    assert!(restored[0].1[0].ingress_receipts.is_empty());
    let mut frame = fixture_unacked(session.stream_id.as_str(), 12);
    frame.ingress_receipts.push(SmIngressFrameReceipt {
        message_key: MessageKey::new(),
        kind: SmIngressReceiptKind::from_storage(1),
        semantic_identity_hash: [7; 32],
    });
    storage
        .store_session_atomic(session.clone(), vec![frame.clone()])
        .await
        .expect("write upgraded snapshot");
    assert_eq!(
        storage
            .list_unacked(&session.stream_id)
            .await
            .expect("restore new receipt")[0]
            .ingress_receipts,
        frame.ingress_receipts
    );
    drop(storage);
    if fixture.db.driver() == crate::db::DatabaseDriver::Postgres {
        fixture
            .execute(&format!("DROP SCHEMA {schema} CASCADE"), ())
            .await;
    }
    fixture.close().await;
}
#[tokio::test]
async fn sqlite_sm_persistence_separate_store_receipt_upgrade() {
    separate_store_upgrade(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_sm_persistence_separate_store_receipt_upgrade() {
    if let Some(fixture) = IngressFixture::postgres("separate_sm_upgrade").await {
        separate_store_upgrade(fixture).await;
    }
}
