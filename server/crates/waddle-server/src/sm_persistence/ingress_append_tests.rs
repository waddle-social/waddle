use super::*;
use crate::db::Value;
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::stream_management::{SmIngressAppendKey, SmIngressReceiptKind};

fn append_for(session: &PersistedSession) -> PersistedIngressAppend {
    PersistedIngressAppend {
        key: SmIngressAppendKey {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [7; 32],
            resource: session.jid.clone(),
        },
        accepting_stream: session.stream_id.clone(),
        appended_at: fixed_time(),
    }
}

async fn snapshot(storage: &DatabaseSmPersistence, stream: &SmSessionId) -> Vec<Vec<Value>> {
    let mut values = Vec::new();
    for (sql, count) in [
        ("SELECT user_id, full_jid, occupancy_session, inbound_count, outbound_count, last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, carbons_enabled, roster_interested, blocklist_interested, presence_available, presence_show, presence_status, presence_priority, replay_gap_through, presence_payloads FROM sm_sessions WHERE stream_id = ?", 18),
        ("SELECT sequence, stanza_xml, original_receipt_at_ms, ingress_receipts FROM sm_unacked WHERE stream_id = ? ORDER BY sequence", 4),
    ] {
        let mut rows = storage.query(sql, crate::db_params![stream.to_string()]).await.unwrap();
        while let Some(row) = rows.next().await.unwrap() {
            values.push((0..count).map(|index| row.get_value(index).unwrap()).collect());
        }
    }
    values
}

async fn allocation_and_duplicate(storage: &DatabaseSmPersistence) {
    let stream = format!("keyed-{}", uuid::Uuid::new_v4());
    let session = fixture_session(&stream);
    let append = append_for(&session);
    let queue = vec![fixture_unacked(&stream, 11), fixture_unacked(&stream, 12)];
    assert_eq!(
        storage
            .store_session_atomic_with_ingress_append(session.clone(), queue, append.clone())
            .await
            .unwrap(),
        KeyedSnapshotOutcome::Committed
    );
    assert_eq!(
        storage
            .list_unacked(&session.stream_id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        storage.get_ingress_append(&append.key).await.unwrap(),
        Some(append.clone())
    );
    let before = snapshot(storage, &session.stream_id).await;
    let mut replacement = session.clone();
    replacement.outbound_count = 30;
    replacement.last_acked = 25;
    replacement.replay_gap_through = Some(26);
    replacement.detached_at += chrono::Duration::minutes(10);
    assert_eq!(
        storage
            .store_session_atomic_with_ingress_append(
                replacement,
                vec![fixture_unacked(&stream, 30)],
                append.clone()
            )
            .await
            .unwrap(),
        KeyedSnapshotOutcome::ObligationAlreadyAllocated {
            accepting_stream: session.stream_id.clone()
        }
    );
    assert_eq!(snapshot(storage, &session.stream_id).await, before);
    assert_eq!(
        storage.get_ingress_append(&append.key).await.unwrap(),
        Some(append)
    );
    storage.delete_session(&session.stream_id).await.unwrap();
}

async fn distinct_obligations(storage: &DatabaseSmPersistence) {
    let stream = format!("distinct-{}", uuid::Uuid::new_v4());
    let mut session = fixture_session(&stream);
    let first = append_for(&session);
    let mut second = first.clone();
    second.key.semantic_identity_hash = [8; 32];
    assert_eq!(
        storage
            .store_session_atomic_with_ingress_append(
                session.clone(),
                vec![fixture_unacked(&stream, 11)],
                first.clone()
            )
            .await
            .unwrap(),
        KeyedSnapshotOutcome::Committed
    );
    session.outbound_count = 13;
    assert_eq!(
        storage
            .store_session_atomic_with_ingress_append(
                session.clone(),
                vec![fixture_unacked(&stream, 11), fixture_unacked(&stream, 13)],
                second.clone()
            )
            .await
            .unwrap(),
        KeyedSnapshotOutcome::Committed
    );
    assert_eq!(
        storage
            .list_unacked(&session.stream_id)
            .await
            .unwrap()
            .len(),
        2
    );
    for append in [first, second] {
        assert_eq!(
            storage.get_ingress_append(&append.key).await.unwrap(),
            Some(append)
        );
    }
    storage.delete_session(&session.stream_id).await.unwrap();
}

async fn unrelated_constraint(storage: &DatabaseSmPersistence) {
    let stream = format!("constraint-{}", uuid::Uuid::new_v4());
    let session = fixture_session(&stream);
    storage
        .store_session_atomic(session.clone(), vec![fixture_unacked(&stream, 11)])
        .await
        .unwrap();
    let before = snapshot(storage, &session.stream_id).await;
    let append = append_for(&session);
    let result = storage
        .store_session_atomic_with_ingress_append(
            session.clone(),
            vec![fixture_unacked(&stream, 12), fixture_unacked(&stream, 12)],
            append.clone(),
        )
        .await;
    assert!(
        result.is_err(),
        "non-ledger queue PK violation must stay an error: {result:?}"
    );
    assert_eq!(snapshot(storage, &session.stream_id).await, before);
    assert_eq!(storage.get_ingress_append(&append.key).await.unwrap(), None);
    storage.delete_session(&session.stream_id).await.unwrap();
}

async fn old_stream_and_deletion(storage: &DatabaseSmPersistence) {
    let old = fixture_session(&format!("old-{}", uuid::Uuid::new_v4()));
    let append = append_for(&old);
    storage
        .store_session_atomic_with_ingress_append(
            old.clone(),
            vec![fixture_unacked(old.stream_id.as_str(), 12)],
            append.clone(),
        )
        .await
        .unwrap();
    storage.delete_session(&old.stream_id).await.unwrap();
    assert!(storage.get_session(&old.stream_id).await.unwrap().is_none());
    assert_eq!(
        storage.get_ingress_append(&append.key).await.unwrap(),
        Some(append.clone())
    );
    let new = fixture_session(&format!("new-{}", uuid::Uuid::new_v4()));
    let retry = PersistedIngressAppend {
        accepting_stream: new.stream_id.clone(),
        ..append.clone()
    };
    assert_eq!(
        storage
            .store_session_atomic_with_ingress_append(
                new.clone(),
                vec![fixture_unacked(new.stream_id.as_str(), 12)],
                retry
            )
            .await
            .unwrap(),
        KeyedSnapshotOutcome::ObligationAlreadyAllocated {
            accepting_stream: old.stream_id
        }
    );
    assert!(storage.get_session(&new.stream_id).await.unwrap().is_none());
    assert!(storage
        .list_unacked(&new.stream_id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        storage.get_ingress_append(&append.key).await.unwrap(),
        Some(append)
    );
}

async fn corrupt_ledger(storage: &DatabaseSmPersistence) {
    let session = fixture_session(&format!("corrupt-{}", uuid::Uuid::new_v4()));
    let append = append_for(&session);
    storage
        .store_session_atomic_with_ingress_append(session.clone(), Vec::new(), append.clone())
        .await
        .unwrap();
    storage
        .execute(
            "UPDATE sm_ingress_appends SET appended_at_ms = ? WHERE message_key = ?",
            crate::db_params![i64::MAX, append.key.message_key.to_storage().to_string()],
        )
        .await
        .unwrap();
    assert!(matches!(
        storage.get_ingress_append(&append.key).await,
        Err(SmPersistenceError::Corrupt { .. })
    ));
    storage.delete_session(&session.stream_id).await.unwrap();
}

#[tokio::test]
async fn sqlite_fresh_allocation_and_duplicate_rollback() {
    allocation_and_duplicate(&DatabaseSmPersistence::open(None).await.unwrap()).await;
}

#[tokio::test]
async fn sqlite_distinct_obligations_allocate_same_stream() {
    distinct_obligations(&DatabaseSmPersistence::open(None).await.unwrap()).await;
}

#[tokio::test]
async fn sqlite_nonledger_constraint_is_error() {
    unrelated_constraint(&DatabaseSmPersistence::open(None).await.unwrap()).await;
}

#[tokio::test]
async fn sqlite_older_stream_proof_survives_deletion() {
    old_stream_and_deletion(&DatabaseSmPersistence::open(None).await.unwrap()).await;
}

#[tokio::test]
async fn sqlite_corrupt_ledger_is_error() {
    corrupt_ledger(&DatabaseSmPersistence::open(None).await.unwrap()).await;
}

#[tokio::test]
async fn postgres_keyed_append_storage_contract() {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (portable keyed SM append contract)");
        return;
    };
    #[cfg(feature = "clustering")]
    let _table_lock = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let storage = DatabaseSmPersistence::open(Some(&url)).await.unwrap();
    allocation_and_duplicate(&storage).await;
    distinct_obligations(&storage).await;
    unrelated_constraint(&storage).await;
    old_stream_and_deletion(&storage).await;
    corrupt_ledger(&storage).await;
}

#[tokio::test]
async fn sqlite_other_ledger_unique_constraint_is_error_and_rolls_back() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    storage
        .execute(
            "CREATE UNIQUE INDEX test_ledger_accepting_stream ON sm_ingress_appends (accepting_stream_id)",
            (),
        )
        .await
        .unwrap();
    let session = fixture_session("ledger-other-constraint");
    let first = append_for(&session);
    storage
        .store_session_atomic_with_ingress_append(
            session.clone(),
            vec![fixture_unacked(session.stream_id.as_str(), 12)],
            first.clone(),
        )
        .await
        .unwrap();
    let before = snapshot(&storage, &session.stream_id).await;
    let mut replacement = session.clone();
    replacement.outbound_count = 13;
    replacement.last_acked = 12;
    replacement.replay_gap_through = Some(12);
    replacement.detached_at += chrono::Duration::minutes(1);
    let second = append_for(&session);
    let result = storage
        .store_session_atomic_with_ingress_append(
            replacement,
            vec![fixture_unacked(session.stream_id.as_str(), 13)],
            second.clone(),
        )
        .await;
    assert!(
        result.is_err(),
        "unrelated ledger uniqueness must stay an error: {result:?}"
    );
    assert_eq!(snapshot(&storage, &session.stream_id).await, before);
    assert_eq!(
        storage.get_ingress_append(&first.key).await.unwrap(),
        Some(first)
    );
    assert_eq!(storage.get_ingress_append(&second.key).await.unwrap(), None);
}
