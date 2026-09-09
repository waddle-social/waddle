use super::*;
use waddle_xmpp::{
    ingress::MessageKey,
    stream_management::{SmIngressAppendKey, SmIngressReceiptKind},
};

fn append(stream: &SmSessionId) -> PersistedIngressAppend {
    PersistedIngressAppend {
        key: SmIngressAppendKey {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [7; 32],
            resource: full("alice@example.com/web"),
        },
        accepting_stream: stream.clone(),
        appended_at: stale_caller_supplied_time(),
    }
}

// Compare the complete stored rows, including counters, replay gaps, timestamps,
// principal metadata and the serialized queue, without lossy decoding.
async fn snapshot(f: &Fixture, stream: &SmSessionId) -> (String, String) {
    let conn = f.claims_db.guard().await.expect("guard");
    let mut rows = conn.query(
        "SELECT row_to_json(s)::text, COALESCE((SELECT json_agg(q ORDER BY sequence)::text FROM sm_unacked q WHERE q.stream_id = s.stream_id), '[]') FROM sm_sessions s WHERE stream_id = ?",
        crate::db_params![stream.as_str().to_string()],
    ).await.expect("snapshot");
    let row = rows.next().await.expect("row").expect("session");
    (
        row.get(0).expect("session JSON"),
        row.get(1).expect("queue JSON"),
    )
}

#[tokio::test]
async fn keyed_append_commits_snapshot_and_ledger() {
    let Some(f) = fixture().await else { return };
    let session = fixture_session("keyed-fresh");
    let proof = append(&session.stream_id);
    assert_eq!(
        f.fenced
            .store_session_atomic_with_ingress_append(
                session.clone(),
                vec![fixture_unacked(session.stream_id.as_str(), 12)],
                proof.clone(),
            )
            .await
            .expect("commit"),
        KeyedSnapshotOutcome::Committed
    );
    assert_eq!(
        f.fenced
            .get_ingress_append(&proof.key)
            .await
            .expect("lookup"),
        Some(proof)
    );
    assert_eq!(
        f.fenced
            .list_unacked(&session.stream_id)
            .await
            .expect("queue")
            .len(),
        1
    );
    assert_eq!(
        f.fenced
            .get_session(&session.stream_id)
            .await
            .expect("lookup")
            .expect("session")
            .outbound_count,
        12
    );
}

#[tokio::test]
async fn duplicate_obligation_rolls_back_every_snapshot_field() {
    let Some(f) = fixture().await else { return };
    let mut session = fixture_session("keyed-duplicate");
    let proof = append(&session.stream_id);
    f.fenced
        .store_session_atomic_with_ingress_append(
            session.clone(),
            vec![fixture_unacked(session.stream_id.as_str(), 12)],
            proof.clone(),
        )
        .await
        .expect("first commit");
    let before = snapshot(&f, &session.stream_id).await;
    session.outbound_count = 20;
    session.last_acked = 19;
    session.replay_gap_through = Some(18);
    session.inbound_count = 99;
    session.detached_at = Utc::now();
    assert_eq!(
        f.fenced
            .store_session_atomic_with_ingress_append(
                session.clone(),
                vec![fixture_unacked(session.stream_id.as_str(), 20)],
                proof
            )
            .await
            .expect("duplicate"),
        KeyedSnapshotOutcome::ObligationAlreadyAllocated {
            accepting_stream: session.stream_id.clone()
        }
    );
    assert_eq!(snapshot(&f, &session.stream_id).await, before);
}

#[tokio::test]
async fn distinct_obligations_allocate_on_same_stream() {
    let Some(f) = fixture().await else { return };
    let mut session = fixture_session("keyed-distinct");
    let first = append(&session.stream_id);
    let second = PersistedIngressAppend {
        key: SmIngressAppendKey {
            semantic_identity_hash: [8; 32],
            ..first.key.clone()
        },
        ..first.clone()
    };
    f.fenced
        .store_session_atomic_with_ingress_append(
            session.clone(),
            vec![fixture_unacked(session.stream_id.as_str(), 12)],
            first.clone(),
        )
        .await
        .expect("first");
    session.outbound_count = 13;
    assert_eq!(
        f.fenced
            .store_session_atomic_with_ingress_append(
                session.clone(),
                vec![
                    fixture_unacked(session.stream_id.as_str(), 12),
                    fixture_unacked(session.stream_id.as_str(), 13)
                ],
                second.clone()
            )
            .await
            .expect("second"),
        KeyedSnapshotOutcome::Committed
    );
    assert_eq!(
        f.fenced
            .list_unacked(&session.stream_id)
            .await
            .expect("queue")
            .len(),
        2
    );
    for proof in [first, second] {
        assert_eq!(
            f.fenced
                .get_ingress_append(&proof.key)
                .await
                .expect("proof"),
            Some(proof)
        );
    }
}

#[tokio::test]
async fn queue_constraint_failure_is_error_and_rolls_back() {
    let Some(f) = fixture().await else { return };
    let session = fixture_session("keyed-bad-queue");
    f.fenced
        .store_session_atomic(
            session.clone(),
            vec![fixture_unacked(session.stream_id.as_str(), 12)],
        )
        .await
        .expect("baseline");
    let before = snapshot(&f, &session.stream_id).await;
    let proof = append(&session.stream_id);
    let stanza = fixture_unacked(session.stream_id.as_str(), 13);
    assert!(f
        .fenced
        .store_session_atomic_with_ingress_append(
            session.clone(),
            vec![stanza.clone(), stanza],
            proof.clone()
        )
        .await
        .is_err());
    assert_eq!(snapshot(&f, &session.stream_id).await, before);
    assert_eq!(
        f.fenced
            .get_ingress_append(&proof.key)
            .await
            .expect("no proof"),
        None
    );
}

#[tokio::test]
async fn older_stream_proof_survives_delete_and_blocks_rebound_stream() {
    let Some(f) = fixture().await else { return };
    let old = fixture_session("keyed-old");
    let proof = append(&old.stream_id);
    f.fenced
        .store_session_atomic_with_ingress_append(
            old.clone(),
            vec![fixture_unacked(old.stream_id.as_str(), 12)],
            proof.clone(),
        )
        .await
        .expect("allocate");
    f.fenced
        .delete_session(&old.stream_id)
        .await
        .expect("delete");
    assert_eq!(
        f.fenced
            .get_ingress_append(&proof.key)
            .await
            .expect("retained proof"),
        Some(proof.clone())
    );
    let new = fixture_session("keyed-new");
    f.fenced
        .store_session_atomic(new.clone(), vec![])
        .await
        .expect("new session");
    let before = snapshot(&f, &new.stream_id).await;
    let retry = PersistedIngressAppend {
        accepting_stream: new.stream_id.clone(),
        ..proof.clone()
    };
    assert_eq!(
        f.fenced
            .store_session_atomic_with_ingress_append(
                new.clone(),
                vec![fixture_unacked(new.stream_id.as_str(), 12)],
                retry
            )
            .await
            .expect("old winner"),
        KeyedSnapshotOutcome::ObligationAlreadyAllocated {
            accepting_stream: old.stream_id
        }
    );
    assert_eq!(snapshot(&f, &new.stream_id).await, before);
}

#[tokio::test]
async fn malformed_ledger_timestamp_is_error() {
    let Some(f) = fixture().await else { return };
    let session = fixture_session("keyed-corrupt");
    let proof = append(&session.stream_id);
    f.fenced
        .store_session_atomic_with_ingress_append(session, vec![], proof.clone())
        .await
        .expect("allocate");
    f.claims_db
        .guard()
        .await
        .expect("guard")
        .execute(
            "UPDATE sm_ingress_appends SET appended_at_ms = ? WHERE message_key = ?",
            crate::db_params![i64::MAX, proof.key.message_key.to_storage().to_string()],
        )
        .await
        .expect("corrupt timestamp");
    assert!(f.fenced.get_ingress_append(&proof.key).await.is_err());
}

#[tokio::test]
async fn keyed_append_rejects_lost_ownership_without_writing_proof() {
    let Some(f) = fixture().await else { return };
    let session = fixture_session("keyed-stolen");
    let proof = append(&session.stream_id);
    f.fenced
        .store_session_atomic(session.clone(), vec![])
        .await
        .expect("establish claim");
    let before = snapshot(&f, &session.stream_id).await;
    seed_node(&f.claims_db, &f.identity, true).await;
    let entity = sm_session_entity(&session.stream_id);
    let owner_epoch = current_claim_epoch(&f, &entity).await;
    let stealer = live_stealer(&f.claims_db).await;
    f.claims
        .steal_stale(&entity, owner_epoch, StalePredicate::OwnerStale, &stealer)
        .await
        .expect("steal");
    assert!(matches!(
        f.fenced
            .store_session_atomic_with_ingress_append(
                session.clone(),
                vec![fixture_unacked(session.stream_id.as_str(), 12)],
                proof.clone()
            )
            .await,
        Err(SmPersistenceError::NotOwner { .. })
    ));
    assert_eq!(snapshot(&f, &session.stream_id).await, before);
    assert_eq!(
        f.fenced
            .get_ingress_append(&proof.key)
            .await
            .expect("proof absent"),
        None
    );
}
