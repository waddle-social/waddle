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

/// The canonical lock arbitrates live queue attempts against *new* detached
/// delivery, while accepted frames retain their independent custody path.
async fn live_attempt_interlock(
    fixture: crate::ingress::test_support::IngressFixture,
    storage: Box<dyn SmPersistenceStorage>,
) {
    use crate::ingress::{commit::commit_submission, receipt_key};
    use crate::ingress_uow::{SendAttemptRepository, SendClaim, SendObligation};
    use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};
    use waddle_xmpp::ownership::NodeIdentity;

    let snapshot_storage = DatabaseSmPersistence::from_database(fixture.db.clone())
        .await
        .expect("SM storage");
    for state in [0, 1, 2] {
        let mut session = fixture_session(&format!("live-interlock-{state}"));
        session.jid = "juliet@example.com/phone".parse().expect("recipient");
        storage
            .store_session_atomic(
                session.clone(),
                vec![fixture_unacked(session.stream_id.as_str(), 11)],
            )
            .await
            .expect("baseline");
        let before = snapshot(&snapshot_storage, &session.stream_id).await;
        let intent = IngressEffectIntent::RouteDirect {
            recipient: session.jid.to_bare(),
            fanout: vec![session.jid.clone()],
            route_identity: EffectMessageIdentity::capture_ordinal(1),
        };
        let mut submission = fixture.submission(None, "interlocked delivery");
        submission.plan.intents = vec![intent.clone()];
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("canonical message");
        let receipt = receipt_key(&intent).expect("receipt");
        let obligation = SendObligation {
            message: decision.message_key.expect("message"),
            receipt,
            recipient: session.jid.clone(),
        };
        let mut tx = fixture.uow.begin().await.expect("claim transaction");
        let SendClaim::Acquired(lease) = SendAttemptRepository::claim(
            &mut tx,
            &obligation,
            &NodeIdentity::new("interlock", "owner"),
            Duration::from_secs(60),
        )
        .await
        .expect("claim") else {
            panic!("fresh lease")
        };
        if state > 0 {
            assert!(SendAttemptRepository::start(&mut tx, &lease)
                .await
                .expect("start"));
        }
        if state > 1 {
            assert!(SendAttemptRepository::complete(&mut tx, &lease)
                .await
                .expect("complete"));
        }
        let mut append = append_for(&session);
        append.key = SmIngressAppendKey {
            message_key: obligation.message,
            kind: SmIngressReceiptKind::from_storage(obligation.receipt.kind.to_storage()),
            semantic_identity_hash: obligation.receipt.semantic_identity_hash,
            resource: session.jid.clone(),
        };
        session.outbound_count = 13;
        let attempted_append = storage.store_session_atomic_with_ingress_delivery(
            session.clone(),
            vec![fixture_unacked(session.stream_id.as_str(), 13)],
            append.clone(),
        );
        tokio::pin!(attempted_append);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut attempted_append)
                .await
                .is_err(),
            "detached allocation must wait for the live claim transaction"
        );
        tx.commit().await.expect("commit attempt");
        let result = tokio::time::timeout(Duration::from_secs(5), &mut attempted_append)
            .await
            .expect("detached write resolves after live commit");
        assert!(
            matches!(result, Err(SmPersistenceError::IngressDeliveryBlocked)),
            "live state {state} must not accept detached delivery: {result:?}"
        );
        assert_eq!(
            snapshot(&snapshot_storage, &session.stream_id).await,
            before
        );
        assert!(storage
            .get_ingress_append(&append.key)
            .await
            .expect("ledger")
            .is_none());
        fixture
            .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
            .await;
        if state < 2 {
            assert_eq!(
                storage
                    .store_session_atomic_with_ingress_delivery(
                        session.clone(),
                        vec![fixture_unacked(session.stream_id.as_str(), 13)],
                        append.clone()
                    )
                    .await
                    .expect("expired unfinished attempt permits custody"),
                KeyedSnapshotOutcome::Committed
            );
            // The old lease cannot start after the detached path took custody.
            let mut tx = fixture.uow.begin().await.expect("stale start");
            assert!(!SendAttemptRepository::start(&mut tx, &lease)
                .await
                .expect("stale start rejected"));
            tx.commit().await.expect("commit stale start");
        } else {
            assert!(
                matches!(
                    storage
                        .store_session_atomic_with_ingress_delivery(
                            session.clone(),
                            vec![],
                            append.clone()
                        )
                        .await,
                    Err(SmPersistenceError::IngressDeliveryBlocked)
                ),
                "expiry never clears an attempted delivery"
            );
            assert_eq!(
                snapshot(&snapshot_storage, &session.stream_id).await,
                before
            );
            assert_eq!(
                storage
                    .store_session_atomic_with_ingress_append(
                        session.clone(),
                        vec![fixture_unacked(session.stream_id.as_str(), 13)],
                        append.clone()
                    )
                    .await
                    .expect("already accepted frame must retain custody"),
                KeyedSnapshotOutcome::Committed
            );
        }
        assert_eq!(
            storage
                .get_ingress_append(&append.key)
                .await
                .expect("custody"),
            Some(append)
        );
    }
    let session = fixture_session("missing-canonical");
    let append = append_for(&session);
    assert!(storage
        .store_session_atomic_with_ingress_delivery(session.clone(), vec![], append.clone())
        .await
        .is_err());
    assert!(storage
        .get_session(&session.stream_id)
        .await
        .expect("missing session")
        .is_none());
    assert!(storage
        .get_ingress_append(&append.key)
        .await
        .expect("missing custody")
        .is_none());
    drop(snapshot_storage);
    drop(storage);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_live_attempt_blocks_new_detached_delivery() {
    let fixture = crate::ingress::test_support::IngressFixture::sqlite().await;
    let storage = DatabaseSmPersistence::from_database(fixture.db.clone())
        .await
        .expect("SM storage");
    live_attempt_interlock(fixture, Box::new(storage)).await;
}

#[tokio::test]
async fn postgres_live_attempt_blocks_new_detached_delivery() {
    if let Some(fixture) =
        crate::ingress::test_support::IngressFixture::postgres("sm_live_interlock").await
    {
        let storage = DatabaseSmPersistence::from_database(fixture.db.clone())
            .await
            .expect("SM storage");
        live_attempt_interlock(fixture, Box::new(storage)).await;
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_fenced_live_attempt_blocks_new_detached_delivery() {
    use crate::clustering::claims::PostgresClaimStore;
    use crate::sm_persistence_fenced::PostgresFencedSmPersistence;
    use waddle_xmpp::ownership::{ClaimStore, NodeIdentity, SharedNodeIdentity};
    if let Some(fixture) =
        crate::ingress::test_support::IngressFixture::postgres("sm_fenced_live_interlock").await
    {
        let fenced_db = Database::from_config(
            "sm-fenced-live-interlock",
            &DatabaseConfig::new(
                DatabaseDriver::Postgres,
                fixture.db.database_url().to_owned(),
            )
            .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("fenced database with control-plane pool");
        let claims = Arc::new(PostgresClaimStore::new(fenced_db.clone()));
        claims.ensure_schema().await.expect("claim schema");
        let storage = PostgresFencedSmPersistence::open(
            fenced_db,
            claims,
            SharedNodeIdentity::new(NodeIdentity::new("interlock", "fenced-owner")),
        )
        .await
        .expect("fenced SM storage");
        live_attempt_interlock(fixture, Box::new(storage)).await;
    }
}

/// A stale executor has passed its status read before another transaction hands
/// the obligation to offline custody and retires its expired send attempt.
async fn completed_handoff_blocks_stale_detached_append(
    fixture: crate::ingress::test_support::IngressFixture,
    storage: Box<dyn SmPersistenceStorage>,
) {
    use crate::ingress::{commit::commit_submission, receipt_key};
    use crate::ingress_uow::{
        CanonicalMessageRepository, DeliveryProgressRepository, EffectReceiptRepository,
        PendingReceiptRepository, SendAttemptRepository, SendClaim, SendObligation,
    };
    use waddle_xmpp::{
        ingress::{EffectMessageIdentity, IngressEffectIntent},
        ownership::NodeIdentity,
        pending_delivery::{
            storage::PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
        },
    };
    let snapshot_storage = DatabaseSmPersistence::from_database(fixture.db.clone())
        .await
        .expect("snapshot storage");
    let pending = crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
        fixture.db.clone(),
        QuotaPolicy::Unlimited,
    )
    .await
    .expect("pending storage");
    for aggregate in [false, true] {
        let mut session = fixture_session(&format!("settled-handoff-{aggregate}"));
        session.jid = "juliet@example.com/phone".parse().expect("target");
        let sibling = "juliet@example.com/laptop"
            .parse::<jid::FullJid>()
            .expect("sibling");
        storage
            .store_session_atomic(
                session.clone(),
                vec![fixture_unacked(session.stream_id.as_str(), 11)],
            )
            .await
            .expect("prior snapshot");
        let before = snapshot(&snapshot_storage, &session.stream_id).await;
        let intent = IngressEffectIntent::RouteDirect {
            recipient: session.jid.to_bare(),
            fanout: vec![session.jid.clone(), sibling.clone()],
            route_identity: EffectMessageIdentity::capture_ordinal(1),
        };
        let mut submission = fixture.submission(None, "handoff replaces unknown send");
        waddle_xmpp::xep::xep0334::add_hint(
            &mut submission.plan.sanitized_message,
            waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
        );
        submission.plan.intents = vec![intent.clone()];
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("canonical authority");
        let obligation = SendObligation {
            message: decision.message_key.expect("key"),
            receipt: receipt_key(&intent).expect("receipt"),
            recipient: session.jid.clone(),
        };
        let mut claim = fixture.uow.begin().await.expect("claim transaction");
        let SendClaim::Acquired(lease) = SendAttemptRepository::claim(
            &mut claim,
            &obligation,
            &NodeIdentity::new("sender", "before-crash"),
            Duration::from_secs(60),
        )
        .await
        .expect("claim") else {
            panic!("fresh claim");
        };
        assert!(SendAttemptRepository::start(&mut claim, &lease)
            .await
            .expect("start"));
        claim.commit().await.expect("start commit");
        fixture
            .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
            .await;
        let mut append = append_for(&session);
        append.key = SmIngressAppendKey {
            message_key: obligation.message,
            kind: SmIngressReceiptKind::from_storage(obligation.receipt.kind.to_storage()),
            semantic_identity_hash: obligation.receipt.semantic_identity_hash,
            resource: session.jid.clone(),
        };
        let context = crate::server::routes::interpret::SmIngressAppendContext {
            message_key: obligation.message,
            receipt: obligation.receipt.clone(),
            received_at: None,
            archive_positions: vec![],
            dispatch_stream: None,
            authority: crate::ingress::append_authority::AppendAuthority::Verified,
        };
        assert_eq!(
            crate::ingress::live_delivery::live_delivery_status(
                &fixture.uow,
                None,
                &context,
                &session.jid
            )
            .await
            .expect("stale executor status"),
            None
        );
        let mut handoff = fixture.uow.begin().await.expect("handoff transaction");
        assert!(
            CanonicalMessageRepository::lock(&mut handoff, obligation.message)
                .await
                .expect("handoff canonical lock")
        );
        let pending_row = PendingRow {
            id: PendingRowId::new(obligation.message.to_storage().to_string()),
            recipient: session.jid.to_bare(),
            original_receipt_at: fixed_time(),
            payload: PendingPayload::Transient(Box::new(submission.plan.sanitized_message.clone())),
            flushed_in_session: None,
            outbound_sequence: None,
        };
        PendingReceiptRepository::insert(&mut handoff, &pending_row, QuotaPolicy::Unlimited)
            .await
            .expect("offline custody");
        if aggregate {
            // An aggregate settlement is independently authoritative even if
            // individual progress is absent (e.g. a route policy discard).
            EffectReceiptRepository::record_receipt(
                &mut handoff,
                obligation.message,
                obligation.receipt.kind,
                &obligation.receipt.semantic_identity_hash,
            )
            .await
            .expect("aggregate settlement");
        } else {
            DeliveryProgressRepository::record(
                &mut handoff,
                obligation.message,
                &obligation.receipt,
                std::slice::from_ref(&session.jid),
            )
            .await
            .expect("handoff progress");
        }
        SendAttemptRepository::retire_expired_attempt(&mut handoff, &obligation)
            .await
            .expect("retire handed-off attempt");
        session.outbound_count = 13;
        let attempted = storage.store_session_atomic_with_ingress_delivery(
            session.clone(),
            vec![fixture_unacked(session.stream_id.as_str(), 13)],
            append.clone(),
        );
        tokio::pin!(attempted);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut attempted)
                .await
                .is_err(),
            "stale append must wait for the handoff canonical transaction"
        );
        handoff.commit().await.expect("handoff commit");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), &mut attempted)
                .await
                .expect("stale append resolves")
                .expect("settlement is not an outage"),
            KeyedSnapshotOutcome::ObligationAlreadyResolved
        );
        assert_eq!(
            snapshot(&snapshot_storage, &session.stream_id).await,
            before,
            "rejected speculative clone cannot replace prior queue/counters"
        );
        assert!(storage
            .get_ingress_append(&append.key)
            .await
            .expect("ledger")
            .is_none());
        assert_eq!(fixture.count("ingress_send_attempts").await, 0);
        assert_eq!(
            pending
                .list(&session.jid.to_bare())
                .await
                .expect("offline custody list")
                .iter()
                .filter(|row| row.id == pending_row.id)
                .count(),
            1
        );
        // A per-resource proof must not suppress a different frozen resource;
        // an aggregate proof does suppress every resource of the obligation.
        let mut sibling_session = fixture_session(&format!("settled-handoff-sibling-{aggregate}"));
        sibling_session.jid = sibling;
        let mut sibling_append = append_for(&sibling_session);
        sibling_append.key = SmIngressAppendKey {
            resource: sibling_session.jid.clone(),
            ..append.key.clone()
        };
        assert_eq!(
            storage
                .store_session_atomic_with_ingress_delivery(sibling_session, vec![], sibling_append)
                .await
                .expect("sibling outcome"),
            if aggregate {
                KeyedSnapshotOutcome::ObligationAlreadyResolved
            } else {
                KeyedSnapshotOutcome::Committed
            }
        );
        // Already counted live frames still retain their independent custody
        // path; this check only suppresses NEW delivery allocations.
        assert_eq!(
            storage
                .store_session_atomic_with_ingress_append(
                    session.clone(),
                    vec![fixture_unacked(session.stream_id.as_str(), 13)],
                    append
                )
                .await
                .expect("accepted frame custody"),
            KeyedSnapshotOutcome::Committed
        );
    }
    drop(pending);
    drop(snapshot_storage);
    drop(storage);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_completed_handoff_blocks_stale_detached_append() {
    let fixture = crate::ingress::test_support::IngressFixture::sqlite().await;
    let storage = DatabaseSmPersistence::from_database(fixture.db.clone())
        .await
        .expect("storage");
    completed_handoff_blocks_stale_detached_append(fixture, Box::new(storage)).await;
}

#[tokio::test]
async fn postgres_completed_handoff_blocks_stale_detached_append() {
    if let Some(fixture) =
        crate::ingress::test_support::IngressFixture::postgres("sm_completed_handoff").await
    {
        let storage = DatabaseSmPersistence::from_database(fixture.db.clone())
            .await
            .expect("storage");
        completed_handoff_blocks_stale_detached_append(fixture, Box::new(storage)).await;
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_fenced_completed_handoff_blocks_stale_detached_append() {
    use crate::clustering::claims::PostgresClaimStore;
    use crate::sm_persistence_fenced::PostgresFencedSmPersistence;
    use waddle_xmpp::ownership::{ClaimStore, NodeIdentity, SharedNodeIdentity};
    if let Some(fixture) =
        crate::ingress::test_support::IngressFixture::postgres("sm_fenced_handoff").await
    {
        let fenced_db = Database::from_config(
            "sm-fenced-handoff",
            &DatabaseConfig::new(
                DatabaseDriver::Postgres,
                fixture.db.database_url().to_owned(),
            )
            .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("fenced db");
        let claims = Arc::new(PostgresClaimStore::new(fenced_db.clone()));
        claims.ensure_schema().await.expect("claims");
        let storage = PostgresFencedSmPersistence::open(
            fenced_db,
            claims,
            SharedNodeIdentity::new(NodeIdentity::new("handoff", "fenced-owner")),
        )
        .await
        .expect("fenced storage");
        completed_handoff_blocks_stale_detached_append(fixture, Box::new(storage)).await;
    }
}
