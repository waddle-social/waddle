//! Resume ACK commits remain authoritative when completion and settlement fail.
use std::sync::{atomic::Ordering, Arc};

use crate::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};
use crate::pending_delivery::{
    storage::{InMemoryPendingDeliveryStorage, PendingDeliveryStorage},
    PendingPayload, PendingRow, PendingRowId, SmSessionId,
};
use crate::stream_management::persistence::{SmAckWindow, SmPersistenceStorage};

use super::{
    realistic_test_session, DetachedSession, GatedSnapshotPersistence, InMemorySmSessionRegistry,
    SmSessionRegistry,
};

async fn fixture(
    stream: &str,
) -> (
    Arc<InMemorySmSessionRegistry>,
    Arc<GatedSnapshotPersistence>,
    DetachedSession,
) {
    let storage = Arc::new(GatedSnapshotPersistence::new(stream));
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    registry
        .store_session(realistic_test_session(stream))
        .await
        .unwrap();
    let claimed = registry.claim_session(stream).await.unwrap().unwrap();
    (registry, storage, claimed)
}

async fn pending_copy(session: &DetachedSession) -> InMemoryPendingDeliveryStorage {
    let pending = InMemoryPendingDeliveryStorage::default();
    for entry in &session.unacked_stanzas {
        let element: minidom::Element = entry.stanza_xml.parse().unwrap();
        pending
            .insert(PendingRow {
                id: PendingRowId::fresh(),
                recipient: session.jid.to_bare(),
                original_receipt_at: entry.original_receipt_at,
                payload: PendingPayload::Transient(Box::new(
                    xmpp_parsers::message::Message::try_from(element).unwrap(),
                )),
                flushed_in_session: Some(SmSessionId::new(&session.stream_id)),
                outbound_sequence: Some(entry.sequence),
            })
            .await
            .unwrap();
    }
    pending
}

#[tokio::test]
async fn acknowledged_prefix_survives_failed_completion_and_expiry() {
    let stream = "resume-ack-before-completion";
    let (registry, storage, claimed) = fixture(stream).await;
    let pending = pending_copy(&claimed).await;
    let id = SmSessionId::new(stream);
    let acknowledged = registry
        .acknowledge_claimed_resume(&claimed, 6)
        .await
        .unwrap();
    assert_eq!(acknowledged.last_acked, 6);
    assert_eq!(acknowledged.max_resume_time, claimed.max_resume_time);
    assert_eq!(acknowledged.unacked_stanzas.len(), 1);
    assert_eq!(acknowledged.unacked_stanzas[0].sequence, 7);
    assert_eq!(
        storage.inner.pending_acknowledgements(&id).await.unwrap(),
        vec![SmAckWindow {
            from_exclusive: 5,
            to_inclusive: 6
        }]
    );
    assert_eq!(
        registry.pending_claim_protected_streams().await.unwrap(),
        vec![id.clone()]
    );
    assert!(
        registry
            .complete_claim_if_resumable(stream, 6)
            .await
            .is_err(),
        "unsettled ACK proof prevents canonical deletion"
    );
    assert!(registry.defer_claimed_resume_release(stream));
    {
        let mut sessions = registry.sessions.write().unwrap();
        let expired = sessions.get_mut(stream).unwrap();
        expired.max_resume_time = Some(0);
        expired.detached_at = std::time::Instant::now() - std::time::Duration::from_secs(130);
    }
    let drained = registry.drain_expired().await.unwrap().pop().unwrap();
    assert_eq!(drained.last_acked, 6);
    assert_eq!(drained.unacked_stanzas.len(), 1);
    assert_eq!(drained.unacked_stanzas[0].sequence, 7);
    assert!(!registry.confirm_promotion_session(&drained).await);
    registry
        .settle_session_acknowledgements(&drained, &pending)
        .await
        .unwrap();
    let remaining = pending.list(&claimed.jid.to_bare()).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].outbound_sequence, Some(7));
    assert_eq!(
        registry.pending_claim_protected_streams().await.unwrap(),
        vec![id]
    );
    assert!(registry.confirm_promotion_session(&drained).await);
    assert!(registry
        .pending_claim_protected_streams()
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn cancelled_ack_reconciles_the_committed_boundary_before_expiry() {
    for committed in [false, true] {
        let stream = if committed {
            "resume-ack-cancel-after"
        } else {
            "resume-ack-cancel-before"
        };
        let (registry, storage, claimed) = fixture(stream).await;
        let pending = pending_copy(&claimed).await;
        storage.gate_acknowledgement.store(true, Ordering::SeqCst);
        storage.commit_then_gate.store(committed, Ordering::SeqCst);
        let task_registry = registry.clone();
        let acknowledging =
            tokio::spawn(
                async move { task_registry.acknowledge_claimed_resume(&claimed, 6).await },
            );
        storage.reached.notified().await;
        acknowledging.abort();
        assert!(acknowledging.await.unwrap_err().is_cancelled());
        assert!(registry.defer_claimed_resume_release(stream));
        {
            let mut sessions = registry.sessions.write().unwrap();
            let expired = sessions.get_mut(stream).unwrap();
            expired.max_resume_time = Some(0);
            expired.detached_at = std::time::Instant::now() - std::time::Duration::from_secs(130);
        }
        let drained = registry.drain_expired().await.unwrap().pop().unwrap();
        assert_eq!(drained.last_acked, if committed { 6 } else { 5 });
        assert_eq!(drained.unacked_stanzas.len(), if committed { 1 } else { 2 });
        registry
            .settle_session_acknowledgements(&drained, &pending)
            .await
            .unwrap();
        let rows = pending.list(&drained.jid.to_bare()).await.unwrap();
        assert_eq!(rows.len(), if committed { 1 } else { 2 });
        assert!(registry.confirm_promotion_session(&drained).await);
    }
}

#[tokio::test]
async fn unreadable_ack_evidence_preserves_pending_rows_and_source() {
    let stream = "resume-ack-read-outage";
    let (registry, storage, claimed) = fixture(stream).await;
    let pending = pending_copy(&claimed).await;
    let acknowledged = registry
        .acknowledge_claimed_resume(&claimed, 6)
        .await
        .unwrap();
    storage
        .fail_acknowledgement_reads
        .store(true, Ordering::SeqCst);
    assert!(registry.pending_claim_protected_streams().await.is_err());
    assert!(registry
        .settle_session_acknowledgements(&acknowledged, &pending)
        .await
        .is_err());
    assert_eq!(pending.count(&claimed.jid.to_bare()).await.unwrap(), 2);
    assert!(registry.complete_claim(stream).await.is_err());
    assert!(storage
        .inner
        .get_session(&SmSessionId::new(stream))
        .await
        .unwrap()
        .is_some());
    storage
        .fail_acknowledgement_reads
        .store(false, Ordering::SeqCst);
    registry
        .settle_session_acknowledgements(&acknowledged, &pending)
        .await
        .unwrap();
    assert!(registry.complete_claim(stream).await.unwrap().is_some());
}

#[tokio::test]
async fn restart_recovers_ack_evidence_before_pending_release() {
    let stream = "resume-ack-restart";
    let (registry, storage, claimed) = fixture(stream).await;
    let pending = pending_copy(&claimed).await;
    registry
        .acknowledge_claimed_resume(&claimed, 6)
        .await
        .unwrap();
    drop(registry);
    let restarted = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    assert_eq!(restarted.restore_from_persistence().await.unwrap(), 1);
    let recovered = restarted.claim_session(stream).await.unwrap().unwrap();
    assert_eq!(recovered.last_acked, 6);
    assert_eq!(recovered.unacked_stanzas.len(), 1);
    assert_eq!(recovered.unacked_stanzas[0].sequence, 7);
    assert!(restarted.complete_claim(stream).await.is_err());
    restarted
        .settle_session_acknowledgements(&recovered, &pending)
        .await
        .unwrap();
    assert_eq!(pending.count(&recovered.jid.to_bare()).await.unwrap(), 1);
    assert!(restarted.complete_claim(stream).await.unwrap().is_some());
}

#[tokio::test]
async fn stale_claim_cannot_acknowledge_or_consume_settlement_evidence() {
    let stream = "resume-ack-stale-claim";
    let storage = Arc::new(GatedSnapshotPersistence::new(stream));
    let claims = Arc::new(InProcessClaimStore::new());
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node-a", "incarnation-a"));
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(claims.clone(), identity);
    registry
        .store_session(realistic_test_session(stream))
        .await
        .unwrap();
    let captured = registry.claim_session(stream).await.unwrap().unwrap();
    let pending = pending_copy(&captured).await;
    let acknowledged = registry
        .acknowledge_claimed_resume(&captured, 6)
        .await
        .unwrap();
    let fence = captured.promotion_custody.captured_fence().unwrap();
    let entity = Entity::new(EntityType::SmSession, stream);
    claims
        .release(&entity, fence.owner(), fence.epoch())
        .await
        .unwrap();
    claims
        .ensure_claimed(&entity, &NodeIdentity::new("node-b", "incarnation-b"))
        .await
        .unwrap();
    assert!(registry
        .acknowledge_claimed_resume(&acknowledged, 7)
        .await
        .is_err());
    assert!(registry
        .settle_session_acknowledgements(&acknowledged, &pending)
        .await
        .is_err());
    assert_eq!(pending.count(&captured.jid.to_bare()).await.unwrap(), 2);
    assert_eq!(
        storage
            .inner
            .pending_acknowledgements(&SmSessionId::new(stream))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        storage
            .inner
            .get_session(&SmSessionId::new(stream))
            .await
            .unwrap()
            .unwrap()
            .last_acked,
        6
    );
}

#[tokio::test]
async fn missing_ack_source_is_never_recreated() {
    let stream = "resume-ack-missing-source";
    let (registry, storage, captured) = fixture(stream).await;
    let id = SmSessionId::new(stream);
    storage.inner.delete_session(&id).await.unwrap();
    assert!(registry
        .acknowledge_claimed_resume(&captured, 6)
        .await
        .is_err());
    assert!(storage.inner.get_session(&id).await.unwrap().is_none());
    assert!(storage
        .inner
        .pending_acknowledgements(&id)
        .await
        .unwrap()
        .is_empty());
}
