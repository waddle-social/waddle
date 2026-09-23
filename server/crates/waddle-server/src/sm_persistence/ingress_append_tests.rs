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
        payload: *fixture_unacked(session.stream_id.as_str(), 12).stanza,
        original_receipt_at: fixed_time(),
        disposition: IngressCustodyDisposition::Pending,
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

/// Issue #1789: a drained batch proves every unallocated obligation with the snapshot
/// and withholds only the conflicting proof. The conflicting entry keeps its row — its
/// sequence was already counted — and the conflict must not poison the transaction.
async fn drained_batch_withholds_only_conflicting_proofs(storage: &DatabaseSmPersistence) {
    let standing_stream = format!("drain-standing-{}", uuid::Uuid::new_v4());
    let standing_session = fixture_session(&standing_stream);
    let standing = append_for(&standing_session);
    assert_eq!(
        storage
            .store_session_atomic_with_ingress_append(
                standing_session,
                vec![fixture_unacked(&standing_stream, 12)],
                standing.clone(),
            )
            .await
            .expect("standing allocation"),
        KeyedSnapshotOutcome::Committed
    );

    let drained_stream = format!("drain-batch-{}", uuid::Uuid::new_v4());
    let drained_session = fixture_session(&drained_stream);
    let conflicting = PersistedIngressAppend {
        accepting_stream: drained_session.stream_id.clone(),
        sequence: 11,
        ..standing.clone()
    };
    let fresh = PersistedIngressAppend {
        sequence: 12,
        ..append_for(&drained_session)
    };
    let withheld = storage
        .store_session_atomic_with_principal_and_ingress_appends(
            &fixture_principal(),
            drained_session.clone(),
            vec![
                fixture_unacked(&drained_stream, 11),
                fixture_unacked(&drained_stream, 12),
            ],
            vec![conflicting, fresh.clone()],
        )
        .await
        .expect("a ledger conflict is not an error");
    assert_eq!(withheld, vec![standing.key.clone()]);

    let queued = storage
        .list_unacked(&drained_session.stream_id)
        .await
        .expect("list drained queue");
    assert_eq!(
        queued.iter().map(|row| row.sequence).collect::<Vec<_>>(),
        vec![11, 12],
        "the conflicting entry stays queued"
    );
    assert!(storage
        .get_session_principal(&drained_session.stream_id)
        .await
        .expect("principal read")
        .is_some());
    assert_eq!(
        storage
            .get_ingress_append(&fresh.key)
            .await
            .expect("fresh proof"),
        Some(fresh)
    );
    assert_eq!(
        storage
            .get_ingress_append(&standing.key)
            .await
            .expect("standing proof"),
        Some(standing),
        "the standing allocation is untouched"
    );
}

#[tokio::test]
async fn sqlite_drained_batch_withholds_only_conflicting_proofs() {
    drained_batch_withholds_only_conflicting_proofs(
        &DatabaseSmPersistence::open(None)
            .await
            .expect("storage open"),
    )
    .await;
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
    custody_survives_gap_and_completion_is_exact(&storage).await;
    custody_acknowledgement_wraps_without_session(&storage).await;
    custody_scrub_cutoff_and_exact_promotion(&storage).await;
    custody_pagination_advances_after_completed_cursor(&storage).await;
    tombstone_deletion_catches_allocation_after_initial_scrub(&storage).await;
    sequence_lookup_retains_terminal_and_distinct_allocations(&storage).await;
    allocation_and_duplicate(&storage).await;
    concurrent_allocation(&url).await;
    distinct_obligations(&storage).await;
    unrelated_constraint(&storage).await;
    old_stream_and_deletion(&storage).await;
    corrupt_ledger(&storage).await;
    drained_batch_withholds_only_conflicting_proofs(&storage).await;
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

async fn custody_survives_gap_and_completion_is_exact(storage: &DatabaseSmPersistence) {
    let mut session = fixture_session(&format!("custody-{}", uuid::Uuid::new_v4()));
    let mut proof = append_for(&session);
    // Sort before ordinary fixtures so the bounded scan must return this row.
    proof.appended_at = DateTime::<Utc>::from_timestamp_millis(1).expect("timestamp");
    session.replay_gap_through = Some(proof.sequence);
    storage
        .store_session_atomic_with_ingress_append(session.clone(), Vec::new(), proof.clone())
        .await
        .expect("store custody even when its replay entry was evicted");
    storage
        .delete_session(&session.stream_id)
        .await
        .expect("delete session");
    assert_eq!(
        storage.get_ingress_append(&proof.key).await.expect("read"),
        Some(proof.clone())
    );
    assert!(storage
        .list_pending_ingress_appends(0)
        .await
        .expect("empty scan")
        .is_empty());
    assert_eq!(
        storage
            .list_pending_ingress_appends(1)
            .await
            .expect("bounded scan"),
        vec![proof.clone()]
    );
    assert!(!storage
        .complete_ingress_append(
            &proof.key,
            &SmSessionId::new("wrong-stream"),
            proof.sequence,
            IngressCustodyDisposition::Promoted
        )
        .await
        .expect("stale stream"));
    assert!(!storage
        .complete_ingress_append(
            &proof.key,
            &proof.accepting_stream,
            proof.sequence + 1,
            IngressCustodyDisposition::Promoted
        )
        .await
        .expect("stale sequence"));
    assert!(storage
        .complete_ingress_append(
            &proof.key,
            &proof.accepting_stream,
            proof.sequence,
            IngressCustodyDisposition::Pending
        )
        .await
        .is_err());
    assert!(storage
        .complete_ingress_append(
            &proof.key,
            &proof.accepting_stream,
            proof.sequence,
            IngressCustodyDisposition::Promoted
        )
        .await
        .expect("durable handoff"));
    assert!(!storage
        .complete_ingress_append(
            &proof.key,
            &proof.accepting_stream,
            proof.sequence,
            IngressCustodyDisposition::Acknowledged
        )
        .await
        .expect("cannot replace terminal evidence"));
    proof.disposition = IngressCustodyDisposition::Promoted;
    assert_eq!(
        storage
            .get_ingress_append(&proof.key)
            .await
            .expect("immutable proof and payload"),
        Some(proof)
    );
}

async fn custody_acknowledgement_wraps_without_session(storage: &DatabaseSmPersistence) {
    let session = fixture_session(&format!("custody-wrap-{}", uuid::Uuid::new_v4()));
    let mut proofs = Vec::new();
    for sequence in [u32::MAX - 1, u32::MAX, 0, 1] {
        let mut proof = append_for(&session);
        proof.sequence = sequence;
        storage
            .store_session_atomic_with_ingress_append(session.clone(), Vec::new(), proof.clone())
            .await
            .expect("allocate");
        proofs.push(proof);
    }
    storage
        .delete_session(&session.stream_id)
        .await
        .expect("resume deletes snapshot");
    storage
        .complete_ingress_appends_through(&session.stream_id, u32::MAX - 1, 0)
        .await
        .expect("ack wrap");
    for proof in &mut proofs {
        if proof.sequence == u32::MAX || proof.sequence == 0 {
            proof.disposition = IngressCustodyDisposition::Acknowledged;
        }
        assert_eq!(
            storage
                .get_ingress_append(&proof.key)
                .await
                .expect("read custody"),
            Some(proof.clone())
        );
    }
    // A repeated h never acknowledges a new interval.
    storage
        .complete_ingress_appends_through(&session.stream_id, 1, 1)
        .await
        .expect("empty ack");
    assert_eq!(
        storage
            .get_ingress_append(&proofs[3].key)
            .await
            .expect("read"),
        Some(proofs[3].clone())
    );
}

#[tokio::test]
async fn sqlite_custody_survives_gap_and_exact_completion() {
    let storage = DatabaseSmPersistence::open(None).await.expect("storage");
    custody_survives_gap_and_completion_is_exact(&storage).await;
    custody_acknowledgement_wraps_without_session(&storage).await;
}

#[tokio::test]
async fn sqlite_new_allocation_cannot_start_with_completed_custody() {
    let storage = DatabaseSmPersistence::open(None).await.expect("storage");
    let session = fixture_session("invalid-custody-disposition");
    let mut proof = append_for(&session);
    proof.disposition = IngressCustodyDisposition::Acknowledged;
    assert!(storage
        .store_session_atomic_with_ingress_append(session.clone(), Vec::new(), proof.clone())
        .await
        .is_err());
    assert!(storage
        .get_session(&session.stream_id)
        .await
        .expect("session")
        .is_none());
    assert!(storage
        .get_ingress_append(&proof.key)
        .await
        .expect("proof")
        .is_none());
}

async fn custody_scrub_cutoff_and_exact_promotion(storage: &DatabaseSmPersistence) {
    let session = fixture_session(&format!("custody-scrub-{}", uuid::Uuid::new_v4()));
    let mut proofs = Vec::new();
    for index in 0..3 {
        let mut proof = append_for(&session);
        proof.sequence += index;
        if index == 1 {
            proof.original_receipt_at += chrono::Duration::seconds(1);
        }
        let Stanza::Message(message) = &mut proof.payload else {
            panic!("message fixture")
        };
        message.id = Some(xmpp_parsers::message::Id("retracted-message".to_string()));
        message.from = Some(
            if index == 2 {
                "mallory@example.com/phone"
            } else {
                "alice@example.com/phone"
            }
            .parse()
            .expect("author"),
        );
        message.to = Some("bob@example.com/web".parse().expect("recipient"));
        storage
            .store_session_atomic_with_ingress_append(session.clone(), Vec::new(), proof.clone())
            .await
            .expect("store custody");
        proofs.push(proof);
    }
    storage
        .delete_session(&session.stream_id)
        .await
        .expect("delete replay snapshot");
    let target = waddle_xmpp::tombstone::TombstoneTarget::Direct {
        wire_id: "retracted-message".to_string(),
        author: "alice@example.com".parse().expect("author"),
        archive: "bob@example.com".parse().expect("archive"),
    };
    storage
        .scrub_ingress_custody(&target, fixed_time())
        .await
        .expect("durable tombstone");
    proofs[0].disposition = IngressCustodyDisposition::Tombstoned;
    for proof in &proofs {
        assert_eq!(
            storage.get_ingress_append(&proof.key).await.expect("read"),
            Some(proof.clone())
        );
    }
    assert!(storage
        .complete_ingress_append(
            &proofs[1].key,
            &session.stream_id,
            proofs[1].sequence,
            IngressCustodyDisposition::Promoted,
        )
        .await
        .expect("exact promotion"));
    proofs[1].disposition = IngressCustodyDisposition::Promoted;
    for proof in &proofs {
        assert_eq!(
            storage.get_ingress_append(&proof.key).await.expect("read"),
            Some(proof.clone())
        );
    }
}

#[tokio::test]
async fn sqlite_custody_scrub_respects_cutoff_author_and_exact_promotion() {
    let storage = DatabaseSmPersistence::open(None).await.expect("storage");
    custody_scrub_cutoff_and_exact_promotion(&storage).await;
}

async fn custody_pagination_advances_after_completed_cursor(storage: &DatabaseSmPersistence) {
    let session = fixture_session("custody-pagination");
    let base = append_for(&session);
    for (hash, resource) in [
        (1, "alice@example.com/phone"),
        (1, "alice@example.com/web"),
        (2, "alice@example.com/phone"),
    ] {
        let mut proof = base.clone();
        proof.key.semantic_identity_hash = [hash; 32];
        proof.key.resource = resource.parse().expect("resource");
        storage
            .store_session_atomic_with_ingress_append(session.clone(), Vec::new(), proof)
            .await
            .expect("allocate");
    }
    let mut before_first = base.key.clone();
    before_first.semantic_identity_hash = [0; 32];
    let first = storage
        .list_pending_ingress_appends_after(Some(&before_first), 1)
        .await
        .expect("first page");
    assert_eq!(first.len(), 1);
    storage
        .complete_ingress_append(
            &first[0].key,
            &first[0].accepting_stream,
            first[0].sequence,
            IngressCustodyDisposition::Acknowledged,
        )
        .await
        .expect("complete cursor allocation");
    let second = storage
        .list_pending_ingress_appends_after(Some(&first[0].key), 1)
        .await
        .expect("second page");
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].key.semantic_identity_hash, [1; 32]);
    assert_eq!(second[0].key.resource.to_string(), "alice@example.com/web");
    let third = storage
        .list_pending_ingress_appends_after(Some(&second[0].key), 1)
        .await
        .expect("third page");
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].key.semantic_identity_hash, [2; 32]);
    assert!(storage
        .list_pending_ingress_appends_after(Some(&third[0].key), 1)
        .await
        .expect("end")
        .iter()
        .all(|row| row.key.message_key != base.key.message_key));
}

#[tokio::test]
async fn sqlite_pending_custody_pagination_advances_after_completed_cursor() {
    let storage = DatabaseSmPersistence::open(None).await.expect("storage");
    custody_pagination_advances_after_completed_cursor(&storage).await;
}

async fn tombstone_deletion_catches_allocation_after_initial_scrub(
    storage: &DatabaseSmPersistence,
) {
    let session = fixture_session(&format!("late-tombstone-custody-{}", uuid::Uuid::new_v4()));
    let mut proof = append_for(&session);
    let Stanza::Message(message) = &mut proof.payload else {
        panic!("message fixture")
    };
    message.id = Some(xmpp_parsers::message::Id("late-allocation".to_string()));
    message.from = Some("alice@example.com/phone".parse().expect("author"));
    message.to = Some("bob@example.com/web".parse().expect("recipient"));
    let target = waddle_xmpp::tombstone::TombstoneTarget::Direct {
        wire_id: "late-allocation".to_string(),
        author: "alice@example.com".parse().expect("author"),
        archive: "bob@example.com".parse().expect("archive"),
    };
    storage
        .scrub_ingress_custody(&target, fixed_time())
        .await
        .expect("initial scrub precedes allocation");
    let mut queued = fixture_unacked(session.stream_id.as_str(), proof.sequence);
    queued.stanza = Box::new(proof.payload.clone());
    storage
        .store_session_atomic_with_ingress_append(session.clone(), vec![queued], proof.clone())
        .await
        .expect("late allocation");
    assert_eq!(
        storage
            .delete_tombstoned_unacked(&session.stream_id, &[proof.sequence])
            .await
            .expect("targeted scrub"),
        1
    );
    assert!(storage
        .list_unacked(&session.stream_id)
        .await
        .expect("queue")
        .is_empty());
    proof.disposition = IngressCustodyDisposition::Tombstoned;
    assert_eq!(
        storage
            .get_ingress_append(&proof.key)
            .await
            .expect("suppressed custody"),
        Some(proof.clone())
    );
    assert_eq!(
        storage
            .delete_tombstoned_unacked(&session.stream_id, &[proof.sequence])
            .await
            .expect("idempotent scrub"),
        0
    );
    assert_eq!(
        storage
            .get_ingress_append(&proof.key)
            .await
            .expect("immutable terminal proof"),
        Some(proof)
    );
}

#[tokio::test]
async fn sqlite_tombstone_deletion_catches_allocation_after_initial_scrub() {
    let storage = DatabaseSmPersistence::open(None).await.expect("storage");
    tombstone_deletion_catches_allocation_after_initial_scrub(&storage).await;
}

#[tokio::test]
async fn sqlite_tombstone_custody_failure_rolls_back_replay_deletion() {
    let storage = DatabaseSmPersistence::open(None).await.expect("storage");
    let session = fixture_session("atomic-tombstone-failure");
    let proof = append_for(&session);
    storage
        .store_session_atomic_with_ingress_append(
            session.clone(),
            vec![fixture_unacked(session.stream_id.as_str(), proof.sequence)],
            proof.clone(),
        )
        .await
        .expect("allocate");
    storage.execute("CREATE TRIGGER fail_custody_tombstone BEFORE UPDATE OF disposition ON sm_ingress_appends BEGIN SELECT RAISE(ABORT, 'injected custody update failure'); END", ()).await.expect("install fault");
    assert!(storage
        .delete_tombstoned_unacked(&session.stream_id, &[proof.sequence])
        .await
        .is_err());
    assert_eq!(
        storage
            .list_unacked(&session.stream_id)
            .await
            .expect("replay remains")
            .len(),
        1
    );
    assert_eq!(
        storage
            .get_ingress_append(&proof.key)
            .await
            .expect("custody remains"),
        Some(proof)
    );
}

async fn sequence_lookup_retains_terminal_and_distinct_allocations(
    storage: &DatabaseSmPersistence,
) {
    let session = fixture_session(&format!("custody-sequence-{}", uuid::Uuid::new_v4()));
    let mut first = append_for(&session);
    let second = append_for(&session);
    let mut other_sequence = append_for(&session);
    other_sequence.sequence += 1;
    for proof in [&first, &second, &other_sequence] {
        storage
            .store_session_atomic_with_ingress_append(session.clone(), Vec::new(), proof.clone())
            .await
            .expect("allocate");
    }
    storage
        .complete_ingress_append(
            &first.key,
            &first.accepting_stream,
            first.sequence,
            IngressCustodyDisposition::Acknowledged,
        )
        .await
        .expect("acknowledge first");
    first.disposition = IngressCustodyDisposition::Acknowledged;
    let allocations = storage
        .get_ingress_appends_for_sequence(&session.stream_id, first.sequence)
        .await
        .expect("lookup sequence");
    assert_eq!(allocations.len(), 2);
    assert!(allocations.contains(&first));
    assert!(allocations.contains(&second));
    assert!(storage
        .get_ingress_appends_for_sequence(&SmSessionId::new("unrelated-stream"), first.sequence)
        .await
        .expect("different stream")
        .is_empty());
}

#[tokio::test]
async fn sqlite_sequence_lookup_retains_terminal_and_distinct_allocations() {
    let storage = DatabaseSmPersistence::open(None).await.expect("storage");
    sequence_lookup_retains_terminal_and_distinct_allocations(&storage).await;
}
