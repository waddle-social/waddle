//! Rewrites must not renew the original database-clock resume window.

use super::*;

#[tokio::test]
async fn first_database_detach_timestamp_survives_every_fenced_snapshot_refresh() {
    let Some(f) = fixture().await else { return };
    let session = fixture_session("stable-db-detach");
    f.fenced
        .store_session_atomic_with_principal(&fixture_principal(), session.clone(), Vec::new())
        .await
        .expect("initial detach");
    let first = f
        .fenced
        .get_session(&session.stream_id)
        .await
        .expect("initial session")
        .expect("source")
        .detached_at;
    assert_ne!(
        first, session.detached_at,
        "initial detach uses the database clock"
    );
    tokio::time::sleep(StdDuration::from_millis(5)).await;
    f.fenced
        .upsert_session(session.clone())
        .await
        .expect("upsert refresh");
    assert_eq!(
        f.fenced
            .get_session(&session.stream_id)
            .await
            .expect("upsert session")
            .expect("source")
            .detached_at,
        first
    );
    f.fenced
        .store_session_atomic(session.clone(), Vec::new())
        .await
        .expect("snapshot refresh");
    assert_eq!(
        f.fenced
            .get_session(&session.stream_id)
            .await
            .expect("snapshot session")
            .expect("source")
            .detached_at,
        first
    );
    f.fenced
        .store_session_atomic_with_principal(&fixture_principal(), session.clone(), Vec::new())
        .await
        .expect("principal snapshot refresh");
    assert_eq!(
        f.fenced
            .get_session(&session.stream_id)
            .await
            .expect("principal session")
            .expect("source")
            .detached_at,
        first
    );
}

#[tokio::test]
async fn fenced_delta_is_atomic_and_rejects_a_captured_stolen_fence() {
    let Some(f) = fixture().await else { return };
    let mut session = fixture_session("fenced-delta");
    session.outbound_count = 2;
    session.last_acked = 0;
    session.replay_gap_through = None;
    let principal = fixture_principal();
    f.fenced
        .store_session_atomic_with_principal(
            &principal,
            session.clone(),
            vec![
                fixture_unacked(session.stream_id.as_str(), 1),
                fixture_unacked(session.stream_id.as_str(), 2),
            ],
        )
        .await
        .expect("initial source");
    let detached_at = f
        .fenced
        .get_session(&session.stream_id)
        .await
        .expect("session")
        .expect("source")
        .detached_at;
    let entity = sm_session_entity(&session.stream_id);
    let epoch = current_claim_epoch(&f, &entity).await;
    let fence = SmClaimFence::new(f.identity.clone(), epoch);
    session.outbound_count = 3;
    session.replay_gap_through = Some(1);
    let stanza = fixture_unacked(session.stream_id.as_str(), 3);
    let proof = PersistedIngressAppend {
        key: waddle_xmpp::stream_management::SmIngressAppendKey {
            message_key: waddle_xmpp::ingress::MessageKey::new(),
            kind: waddle_xmpp::stream_management::SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [8; 32],
            resource: session.jid.clone(),
        },
        accepting_stream: session.stream_id.clone(),
        sequence: 3,
        appended_at: stale_caller_supplied_time(),
        payload: *stanza.stanza.clone(),
        original_receipt_at: stanza.original_receipt_at,
        disposition: IngressCustodyDisposition::Pending,
    };
    let request = PersistedDetachedAppend {
        session: session.clone(),
        stanza,
        evicted_sequences: vec![1],
        ingress_append: Some(proof.clone()),
        new_delivery: false,
    };
    assert!(matches!(
        f.fenced
            .append_detached_stanza_atomic(request.clone(), None)
            .await,
        Err(SmPersistenceError::NotOwner { .. })
    ));
    assert_eq!(
        f.fenced
            .append_detached_stanza_atomic(request, Some(&fence))
            .await
            .expect("delta"),
        KeyedSnapshotOutcome::Committed
    );
    let loaded = f
        .fenced
        .get_session(&session.stream_id)
        .await
        .expect("delta session")
        .expect("source");
    assert_eq!(loaded.outbound_count, 3);
    assert_eq!(loaded.detached_at, detached_at);
    assert_eq!(
        f.fenced
            .list_unacked(&session.stream_id)
            .await
            .expect("queue")
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(
        f.fenced
            .get_ingress_append(&proof.key)
            .await
            .expect("custody"),
        Some(proof.clone())
    );
    assert_eq!(
        f.fenced
            .get_session_principal(&session.stream_id)
            .await
            .expect("principal"),
        Some(principal)
    );

    session.outbound_count = 4;
    session.inbound_count = 999;
    let stanza = fixture_unacked(session.stream_id.as_str(), 4);
    let mut duplicate = proof.clone();
    duplicate.sequence = 4;
    duplicate.payload = *stanza.stanza.clone();
    let request = PersistedDetachedAppend {
        session: session.clone(),
        stanza,
        evicted_sequences: vec![2],
        ingress_append: Some(duplicate),
        new_delivery: false,
    };
    assert_eq!(
        f.fenced
            .append_detached_stanza_atomic(request.clone(), Some(&fence))
            .await
            .expect("duplicate winner"),
        KeyedSnapshotOutcome::ObligationAlreadyAllocated {
            accepting_stream: session.stream_id.clone()
        }
    );
    let after_duplicate = f
        .fenced
        .get_session(&session.stream_id)
        .await
        .expect("duplicate session")
        .expect("source");
    assert_eq!(
        (
            after_duplicate.inbound_count,
            after_duplicate.outbound_count
        ),
        (7, 3)
    );
    assert_eq!(after_duplicate.detached_at, detached_at);
    assert_eq!(
        f.fenced
            .list_unacked(&session.stream_id)
            .await
            .expect("queue after duplicate")
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );

    seed_node(&f.claims_db, &f.identity, true).await;
    let stealer = live_stealer(&f.claims_db).await;
    f.claims
        .steal_stale(&entity, epoch, StalePredicate::OwnerStale, &stealer)
        .await
        .expect("steal claim");
    assert!(matches!(
        f.fenced
            .append_detached_stanza_atomic(request, Some(&fence))
            .await,
        Err(SmPersistenceError::NotOwner { .. })
    ));
    assert_eq!(
        f.fenced
            .get_session(&session.stream_id)
            .await
            .expect("session after stolen fence")
            .expect("source")
            .outbound_count,
        3
    );
    assert_eq!(
        f.fenced
            .list_unacked(&session.stream_id)
            .await
            .expect("queue after stolen fence")
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
}
