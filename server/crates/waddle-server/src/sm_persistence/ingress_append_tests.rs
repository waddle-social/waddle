use super::*;
use crate::db::Value;
use std::sync::Arc;
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
        sequence: 12,
        supersedes: None,
        appended_at: fixed_time(),
    }
}

/// The pre-check cannot serialize callers, so the database constraint has to.
///
/// Both writers get their OWN persistence instance: a shared one would serialize
/// them on its per-stream mutex, so the second would merely read a committed
/// ledger row and never exercise the uniqueness race or the conflict
/// classification path. A barrier lines them up at the insert boundary.
async fn concurrent_allocation(url: &str) {
    let stream = format!("concurrent-{}", uuid::Uuid::new_v4());
    let first_storage = Arc::new(
        DatabaseSmPersistence::open(Some(url))
            .await
            .expect("first writer storage"),
    );
    let second_storage = Arc::new(
        DatabaseSmPersistence::open(Some(url))
            .await
            .expect("second writer storage"),
    );
    let session = fixture_session(&stream);
    let append = append_for(&session);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let writers = [
        (Arc::clone(&first_storage), 11u32),
        (Arc::clone(&second_storage), 12u32),
    ]
    .map(|(storage, sequence)| {
        let session = session.clone();
        let append = append.clone();
        let stream = stream.clone();
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            storage
                .store_session_atomic_with_ingress_append(
                    session,
                    vec![fixture_unacked(&stream, sequence)],
                    append,
                )
                .await
        })
    });
    let mut outcomes = Vec::new();
    for writer in writers {
        outcomes.push(writer.await.expect("writer task"));
    }
    // A writer may legitimately lose the database write lock instead of the
    // uniqueness race; what must never happen is two allocations.
    let committed = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Ok(KeyedSnapshotOutcome::Committed)))
        .count();
    assert_eq!(
        committed, 1,
        "exactly one concurrent writer may allocate the obligation: {outcomes:?}"
    );
    let proof = first_storage
        .get_ingress_append(&append.key)
        .await
        .expect("ledger read")
        .expect("the winner's proof stands");
    assert_eq!(
        first_storage
            .list_unacked(&proof.accepting_stream)
            .await
            .expect("queue read")
            .len(),
        1,
        "the losing writer left no queue entry behind"
    );
    first_storage
        .delete_session(&session.stream_id)
        .await
        .expect("cleanup");
}

async fn snapshot(storage: &DatabaseSmPersistence, stream: &SmSessionId) -> Vec<Vec<Value>> {
    let mut values = Vec::new();
    for (sql, count) in [
        ("SELECT user_id, full_jid, occupancy_session, inbound_count, outbound_count, last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, carbons_enabled, roster_interested, blocklist_interested, presence_available, presence_show, presence_status, presence_priority, replay_gap_through, presence_payloads FROM sm_sessions WHERE stream_id = ?", 18),
        ("SELECT sequence, stanza_xml, original_receipt_at_ms, ingress_receipts FROM sm_unacked WHERE stream_id = ? ORDER BY sequence", 4),
    ] {
        let mut rows = storage.query(sql, crate::db_params![stream.to_string()]).await.expect("schema query");
        while let Some(row) = rows.next().await.expect("row fetch") {
            values.push((0..count).map(|index| row.get_value(index).expect("column decode")).collect());
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
            .expect("keyed snapshot write"),
        KeyedSnapshotOutcome::Committed
    );
    assert_eq!(
        storage
            .list_unacked(&session.stream_id)
            .await
            .expect("test fixture")
            .len(),
        2
    );
    assert_eq!(
        storage
            .get_ingress_append(&append.key)
            .await
            .expect("ledger read"),
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
            .expect("keyed snapshot write"),
        KeyedSnapshotOutcome::ObligationAlreadyAllocated {
            accepting_stream: session.stream_id.clone()
        }
    );
    assert_eq!(snapshot(storage, &session.stream_id).await, before);
    assert_eq!(
        storage
            .get_ingress_append(&append.key)
            .await
            .expect("ledger read"),
        Some(append)
    );
    storage
        .delete_session(&session.stream_id)
        .await
        .expect("session delete");
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
            .expect("keyed snapshot write"),
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
            .expect("keyed snapshot write"),
        KeyedSnapshotOutcome::Committed
    );
    assert_eq!(
        storage
            .list_unacked(&session.stream_id)
            .await
            .expect("test fixture")
            .len(),
        2
    );
    for append in [first, second] {
        assert_eq!(
            storage
                .get_ingress_append(&append.key)
                .await
                .expect("ledger read"),
            Some(append)
        );
    }
    storage
        .delete_session(&session.stream_id)
        .await
        .expect("session delete");
}

async fn unrelated_constraint(storage: &DatabaseSmPersistence) {
    let stream = format!("constraint-{}", uuid::Uuid::new_v4());
    let session = fixture_session(&stream);
    storage
        .store_session_atomic(session.clone(), vec![fixture_unacked(&stream, 11)])
        .await
        .expect("snapshot write");
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
    assert_eq!(
        storage
            .get_ingress_append(&append.key)
            .await
            .expect("ledger read"),
        None
    );
    storage
        .delete_session(&session.stream_id)
        .await
        .expect("session delete");
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
        .expect("keyed snapshot write");
    storage
        .delete_session(&old.stream_id)
        .await
        .expect("session delete");
    assert!(storage
        .get_session(&old.stream_id)
        .await
        .expect("durable session read")
        .is_none());
    assert_eq!(
        storage
            .get_ingress_append(&append.key)
            .await
            .expect("ledger read"),
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
            .expect("keyed snapshot write"),
        KeyedSnapshotOutcome::ObligationAlreadyAllocated {
            accepting_stream: old.stream_id
        }
    );
    assert!(storage
        .get_session(&new.stream_id)
        .await
        .expect("durable session read")
        .is_none());
    assert!(storage
        .list_unacked(&new.stream_id)
        .await
        .expect("test fixture")
        .is_empty());
    assert_eq!(
        storage
            .get_ingress_append(&append.key)
            .await
            .expect("ledger read"),
        Some(append)
    );
}

async fn corrupt_ledger(storage: &DatabaseSmPersistence) {
    let session = fixture_session(&format!("corrupt-{}", uuid::Uuid::new_v4()));
    let append = append_for(&session);
    storage
        .store_session_atomic_with_ingress_append(session.clone(), Vec::new(), append.clone())
        .await
        .expect("keyed snapshot write");
    storage
        .execute(
            "UPDATE sm_ingress_appends SET appended_at_ms = ? WHERE message_key = ?",
            crate::db_params![i64::MAX, append.key.message_key.to_storage().to_string()],
        )
        .await
        .expect("test fixture");
    assert!(matches!(
        storage.get_ingress_append(&append.key).await,
        Err(SmPersistenceError::Corrupt { .. })
    ));
    storage
        .delete_session(&session.stream_id)
        .await
        .expect("session delete");
}

#[tokio::test]
async fn sqlite_fresh_allocation_and_duplicate_rollback() {
    allocation_and_duplicate(
        &DatabaseSmPersistence::open(None)
            .await
            .expect("storage open"),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_concurrent_writers_allocate_once() {
    // A file-backed database so two independent instances share one ledger;
    // `open(None)` would give each writer its own private in-memory database.
    let directory = tempfile::tempdir().expect("sqlite race directory");
    let path = directory.path().join("keyed-race.db");
    concurrent_allocation(path.to_str().expect("sqlite path")).await;
}

#[tokio::test]
async fn sqlite_distinct_obligations_allocate_same_stream() {
    distinct_obligations(
        &DatabaseSmPersistence::open(None)
            .await
            .expect("storage open"),
    )
    .await;
}

#[tokio::test]
async fn sqlite_nonledger_constraint_is_error() {
    unrelated_constraint(
        &DatabaseSmPersistence::open(None)
            .await
            .expect("storage open"),
    )
    .await;
}

#[tokio::test]
async fn sqlite_older_stream_proof_survives_deletion() {
    old_stream_and_deletion(
        &DatabaseSmPersistence::open(None)
            .await
            .expect("storage open"),
    )
    .await;
}

#[tokio::test]
async fn sqlite_corrupt_ledger_is_error() {
    corrupt_ledger(
        &DatabaseSmPersistence::open(None)
            .await
            .expect("storage open"),
    )
    .await;
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
    let storage = DatabaseSmPersistence::open(Some(&url))
        .await
        .expect("storage open");
    allocation_and_duplicate(&storage).await;
    concurrent_allocation(&url).await;
    distinct_obligations(&storage).await;
    unrelated_constraint(&storage).await;
    old_stream_and_deletion(&storage).await;
    corrupt_ledger(&storage).await;
}

#[tokio::test]
async fn sqlite_other_ledger_unique_constraint_is_error_and_rolls_back() {
    let storage = DatabaseSmPersistence::open(None)
        .await
        .expect("storage open");
    storage
        .execute(
            "CREATE UNIQUE INDEX test_ledger_accepting_stream ON sm_ingress_appends (accepting_stream_id)",
            (),
        )
        .await
        .expect("test fixture");
    let session = fixture_session("ledger-other-constraint");
    let first = append_for(&session);
    storage
        .store_session_atomic_with_ingress_append(
            session.clone(),
            vec![fixture_unacked(session.stream_id.as_str(), 12)],
            first.clone(),
        )
        .await
        .expect("keyed snapshot write");
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
        storage
            .get_ingress_append(&first.key)
            .await
            .expect("ledger read"),
        Some(first)
    );
    assert_eq!(
        storage
            .get_ingress_append(&second.key)
            .await
            .expect("ledger read"),
        None
    );
}
