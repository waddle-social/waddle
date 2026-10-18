//! Incremental detached writes preserve the durable detach envelope.

use super::*;
use waddle_xmpp::stream_management::persistence::PersistedDetachedAppend;

#[tokio::test]
async fn detached_append_preserves_envelope_and_untouched_replay_entries() {
    let storage = DatabaseSmPersistence::open(None).await.expect("database");
    let mut original = fixture_session("incremental-envelope");
    original.outbound_count = 2;
    original.last_acked = 0;
    original.replay_gap_through = None;
    let principal = fixture_principal();
    storage
        .store_session_atomic_with_principal(
            &principal,
            original.clone(),
            vec![
                fixture_unacked(original.stream_id.as_str(), 1),
                fixture_unacked(original.stream_id.as_str(), 2),
            ],
        )
        .await
        .expect("initial detached snapshot");
    let retained = storage
        .list_unacked(&original.stream_id)
        .await
        .expect("initial queue")
        .remove(1);
    // Retained row 2 is an observable replay invariant. Make a full-queue
    // replacement fail even if it would reconstruct identical bytes afterward.
    for sql in [
        "CREATE TRIGGER retained_replay_cannot_be_deleted BEFORE DELETE ON sm_unacked WHEN OLD.stream_id = 'incremental-envelope' AND OLD.sequence = 2 BEGIN SELECT RAISE(ABORT, 'retained replay row deleted'); END",
        "CREATE TRIGGER retained_replay_cannot_be_reinserted BEFORE INSERT ON sm_unacked WHEN NEW.stream_id = 'incremental-envelope' AND NEW.sequence = 2 BEGIN SELECT RAISE(ABORT, 'retained replay row reinserted'); END",
    ] {
        storage.execute(sql, ()).await.expect("guard retained replay row");
    }
    let mut update = original.clone();
    update.inbound_count = 9;
    update.outbound_count = 3;
    update.last_acked = 1;
    update.replay_gap_through = Some(1);
    update.detached_at += chrono::Duration::seconds(30);
    update.max_resume_duration = Duration::from_secs(900);
    update.occupancy_session = waddle_xmpp_core::OccupancySessionGeneration::mint();
    update.user_id = "untrusted replacement".into();
    update.carbons_enabled = false;
    update.presence_status = Some("must not replace the detach envelope".into());

    assert_eq!(
        storage
            .append_detached_stanza_atomic(
                PersistedDetachedAppend {
                    session: update,
                    stanza: fixture_unacked(original.stream_id.as_str(), 3),
                    evicted_sequences: vec![1],
                    ingress_append: None,
                    new_delivery: false,
                },
                None,
            )
            .await
            .expect("atomic delta"),
        KeyedSnapshotOutcome::Committed,
    );

    let loaded = storage
        .get_session(&original.stream_id)
        .await
        .expect("session")
        .expect("source remains");
    assert_eq!(
        (
            loaded.inbound_count,
            loaded.outbound_count,
            loaded.last_acked,
            loaded.replay_gap_through
        ),
        (9, 3, 1, Some(1))
    );
    assert_eq!(loaded.detached_at, original.detached_at);
    assert_eq!(loaded.max_resume_duration, original.max_resume_duration);
    assert_eq!(loaded.user_id, original.user_id);
    assert_eq!(loaded.occupancy_session, original.occupancy_session);
    assert_eq!(loaded.carbons_enabled, original.carbons_enabled);
    assert_eq!(loaded.presence_status, original.presence_status);
    assert_eq!(
        storage
            .get_session_principal(&original.stream_id)
            .await
            .expect("principal"),
        Some(principal)
    );
    let queue = storage
        .list_unacked(&original.stream_id)
        .await
        .expect("updated queue");
    assert_eq!(
        queue.iter().map(|entry| entry.sequence).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(queue[0].stanza.to_element(), retained.stanza.to_element());
    assert_eq!(queue[0].original_receipt_at, retained.original_receipt_at);
}

async fn assert_first_detach_timestamp_survives_refresh(storage: &dyn SmPersistenceStorage) {
    let mut session = fixture_session("stable-first-detach");
    storage
        .store_session_atomic_with_principal(&fixture_principal(), session.clone(), Vec::new())
        .await
        .expect("initial detach");
    let first = storage
        .get_session(&session.stream_id)
        .await
        .expect("load initial detach")
        .expect("initial source")
        .detached_at;
    session.detached_at += chrono::Duration::seconds(30);
    session.outbound_count += 1;
    storage
        .upsert_session(session.clone())
        .await
        .expect("upsert existing source");
    assert_eq!(
        storage
            .get_session(&session.stream_id)
            .await
            .expect("load upsert")
            .expect("source")
            .detached_at,
        first
    );
    storage
        .store_session_atomic(session.clone(), Vec::new())
        .await
        .expect("refresh snapshot");
    assert_eq!(
        storage
            .get_session(&session.stream_id)
            .await
            .expect("load snapshot")
            .expect("source")
            .detached_at,
        first
    );
    storage
        .store_session_atomic_with_principal(&fixture_principal(), session.clone(), Vec::new())
        .await
        .expect("refresh principal snapshot");
    assert_eq!(
        storage
            .get_session(&session.stream_id)
            .await
            .expect("load principal snapshot")
            .expect("source")
            .detached_at,
        first
    );
    let entry = fixture_unacked(session.stream_id.as_str(), session.outbound_count);
    let proof = append_proof(&session, &entry);
    storage
        .store_session_atomic_with_ingress_append(session.clone(), vec![entry], proof)
        .await
        .expect("refresh keyed snapshot");
    assert_eq!(
        storage
            .get_session(&session.stream_id)
            .await
            .expect("load keyed snapshot")
            .expect("source")
            .detached_at,
        first
    );
    assert!(storage
        .list_expired_sessions(first + chrono::Duration::seconds(61))
        .await
        .expect("expiry scan")
        .iter()
        .any(|expired| expired.stream_id == session.stream_id));
}

#[tokio::test]
async fn first_detach_timestamp_survives_all_portable_snapshot_refreshes() {
    let storage = DatabaseSmPersistence::open(None).await.expect("database");
    assert_first_detach_timestamp_survives_refresh(&storage).await;
}

#[tokio::test]
async fn first_detach_timestamp_survives_all_memory_snapshot_refreshes() {
    assert_first_detach_timestamp_survives_refresh(
        &waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new(),
    )
    .await;
}

fn append_proof(
    session: &PersistedSession,
    stanza: &PersistedUnackedStanza,
) -> PersistedIngressAppend {
    PersistedIngressAppend {
        key: waddle_xmpp::stream_management::SmIngressAppendKey {
            message_key: waddle_xmpp::ingress::MessageKey::new(),
            kind: waddle_xmpp::stream_management::SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [7; 32],
            resource: session.jid.clone(),
        },
        accepting_stream: session.stream_id.clone(),
        sequence: stanza.sequence,
        appended_at: fixed_time(),
        payload: *stanza.stanza.clone(),
        original_receipt_at: stanza.original_receipt_at,
        disposition: IngressCustodyDisposition::Pending,
    }
}

async fn seed_rollback_source(storage: &DatabaseSmPersistence, stream: &str) -> PersistedSession {
    let mut session = fixture_session(stream);
    session.outbound_count = 2;
    session.last_acked = 0;
    session.replay_gap_through = None;
    storage
        .store_session_atomic_with_principal(
            &fixture_principal(),
            session.clone(),
            vec![fixture_unacked(stream, 1), fixture_unacked(stream, 2)],
        )
        .await
        .expect("initial source");
    session
}

#[tokio::test]
async fn duplicate_incremental_custody_rolls_back_counters_and_evictions() {
    let storage = DatabaseSmPersistence::open(None).await.expect("database");
    let mut session = seed_rollback_source(&storage, "delta-duplicate").await;
    let principal = storage
        .get_session_principal(&session.stream_id)
        .await
        .expect("principal");
    session.outbound_count = 3;
    let stanza = fixture_unacked(session.stream_id.as_str(), 3);
    let proof = append_proof(&session, &stanza);
    let request = PersistedDetachedAppend {
        session: session.clone(),
        stanza,
        evicted_sequences: Vec::new(),
        ingress_append: Some(proof.clone()),
        new_delivery: false,
    };
    assert_eq!(
        storage
            .append_detached_stanza_atomic(request, None)
            .await
            .expect("first allocation"),
        KeyedSnapshotOutcome::Committed
    );
    let mut retry = session.clone();
    retry.outbound_count = 4;
    retry.inbound_count = 999;
    retry.replay_gap_through = Some(2);
    let mut duplicate_proof = proof.clone();
    duplicate_proof.sequence = 4;
    duplicate_proof.payload = *fixture_unacked(session.stream_id.as_str(), 4).stanza;
    assert_eq!(
        storage
            .append_detached_stanza_atomic(
                PersistedDetachedAppend {
                    session: retry,
                    stanza: fixture_unacked(session.stream_id.as_str(), 4),
                    evicted_sequences: vec![1, 2],
                    ingress_append: Some(duplicate_proof),
                    new_delivery: false
                },
                None
            )
            .await
            .expect("standing winner"),
        KeyedSnapshotOutcome::ObligationAlreadyAllocated {
            accepting_stream: session.stream_id.clone()
        }
    );
    let loaded = storage
        .get_session(&session.stream_id)
        .await
        .expect("session")
        .expect("source");
    assert_eq!(
        (
            loaded.inbound_count,
            loaded.outbound_count,
            loaded.replay_gap_through
        ),
        (7, 3, None)
    );
    assert_eq!(loaded.detached_at, session.detached_at);
    assert_eq!(
        storage
            .get_session_principal(&session.stream_id)
            .await
            .expect("principal"),
        principal
    );
    assert_eq!(
        storage.get_ingress_append(&proof.key).await.expect("proof"),
        Some(proof)
    );
    assert_eq!(
        storage
            .list_unacked(&session.stream_id)
            .await
            .expect("queue")
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[tokio::test]
async fn replay_constraint_failure_rolls_back_delta_and_new_custody() {
    let storage = DatabaseSmPersistence::open(None).await.expect("database");
    let original = seed_rollback_source(&storage, "delta-constraint").await;
    let principal = storage
        .get_session_principal(&original.stream_id)
        .await
        .expect("principal");
    let stanza = fixture_unacked(original.stream_id.as_str(), 2);
    let proof = append_proof(&original, &stanza);
    let mut update = original.clone();
    update.outbound_count = 3;
    update.inbound_count = 999;
    update.replay_gap_through = Some(1);
    assert!(storage
        .append_detached_stanza_atomic(
            PersistedDetachedAppend {
                session: update,
                stanza,
                evicted_sequences: vec![1],
                ingress_append: Some(proof.clone()),
                new_delivery: false
            },
            None
        )
        .await
        .is_err());
    let loaded = storage
        .get_session(&original.stream_id)
        .await
        .expect("session")
        .expect("source");
    assert_eq!(
        (
            loaded.inbound_count,
            loaded.outbound_count,
            loaded.replay_gap_through
        ),
        (7, 2, None)
    );
    assert_eq!(loaded.detached_at, original.detached_at);
    assert_eq!(
        storage
            .get_session_principal(&original.stream_id)
            .await
            .expect("principal"),
        principal
    );
    assert_eq!(
        storage.get_ingress_append(&proof.key).await.expect("proof"),
        None
    );
    assert_eq!(
        storage
            .list_unacked(&original.stream_id)
            .await
            .expect("queue")
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[tokio::test]
async fn immediately_evicted_append_keeps_custody_but_no_replay_copy() {
    let storage = DatabaseSmPersistence::open(None).await.expect("database");
    let mut session = seed_rollback_source(&storage, "delta-immediate-eviction").await;
    session.outbound_count = 3;
    session.replay_gap_through = Some(3);
    let stanza = fixture_unacked(session.stream_id.as_str(), 3);
    let proof = append_proof(&session, &stanza);
    assert_eq!(
        storage
            .append_detached_stanza_atomic(
                PersistedDetachedAppend {
                    session: session.clone(),
                    stanza,
                    evicted_sequences: vec![1, 3],
                    ingress_append: Some(proof.clone()),
                    new_delivery: false
                },
                None
            )
            .await
            .expect("evicted append"),
        KeyedSnapshotOutcome::Committed
    );
    assert_eq!(
        storage
            .list_unacked(&session.stream_id)
            .await
            .expect("queue")
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!(
        storage
            .get_ingress_append(&proof.key)
            .await
            .expect("custody"),
        Some(proof)
    );
}

#[tokio::test]
async fn incremental_append_cannot_recreate_a_deleted_source() {
    let storage = DatabaseSmPersistence::open(None).await.expect("database");
    let session = seed_rollback_source(&storage, "delta-missing-source").await;
    storage
        .delete_session(&session.stream_id)
        .await
        .expect("remove source");
    let stanza = fixture_unacked(session.stream_id.as_str(), 3);
    let proof = append_proof(&session, &stanza);
    assert!(storage
        .append_detached_stanza_atomic(
            PersistedDetachedAppend {
                session: session.clone(),
                stanza,
                evicted_sequences: Vec::new(),
                ingress_append: Some(proof.clone()),
                new_delivery: false
            },
            None
        )
        .await
        .is_err());
    assert!(storage
        .get_session(&session.stream_id)
        .await
        .expect("source")
        .is_none());
    assert!(storage
        .list_unacked(&session.stream_id)
        .await
        .expect("queue")
        .is_empty());
    assert!(storage
        .get_ingress_append(&proof.key)
        .await
        .expect("proof")
        .is_none());
}

#[tokio::test]
async fn first_detach_timestamp_and_expiry_survive_reopen_then_append() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("stable-detach.sqlite");
    let url = path.to_str().expect("UTF-8 database path");
    let session = fixture_session("stable-detach-reopen");
    {
        let storage = DatabaseSmPersistence::open(Some(url))
            .await
            .expect("initial database");
        storage
            .store_session_atomic_with_principal(&fixture_principal(), session.clone(), Vec::new())
            .await
            .expect("initial detach");
    }
    {
        let storage = DatabaseSmPersistence::open(Some(url))
            .await
            .expect("reopened database");
        let mut restored = storage
            .get_session(&session.stream_id)
            .await
            .expect("restore session")
            .expect("durable source");
        restored.detached_at += chrono::Duration::seconds(30);
        storage
            .store_session_atomic(restored.clone(), Vec::new())
            .await
            .expect("snapshot after restore");
        restored.outbound_count = 13;
        storage
            .append_detached_stanza_atomic(
                PersistedDetachedAppend {
                    session: restored,
                    stanza: fixture_unacked(session.stream_id.as_str(), 13),
                    evicted_sequences: Vec::new(),
                    ingress_append: None,
                    new_delivery: false,
                },
                None,
            )
            .await
            .expect("append after restore");
    }
    let storage = DatabaseSmPersistence::open(Some(url))
        .await
        .expect("verify persisted database");
    let loaded = storage
        .get_session(&session.stream_id)
        .await
        .expect("load refreshed source")
        .expect("source");
    assert_eq!(loaded.detached_at, session.detached_at);
    assert_eq!(loaded.outbound_count, 13);
    assert_eq!(
        storage
            .list_unacked(&session.stream_id)
            .await
            .expect("restored queue")
            .len(),
        1
    );
    assert!(storage
        .list_expired_sessions(session.detached_at + chrono::Duration::seconds(61))
        .await
        .expect("original expiry")
        .iter()
        .any(|expired| expired.stream_id == session.stream_id));
}
