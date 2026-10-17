//! Completion expiry retains the canonical queue and its exact ownership.
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};
use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::{
    PersistedUnackedStanza, SmClaimFence, SmPersistenceStorage,
};

use super::{
    realistic_test_session, Duration, GatedSnapshotPersistence, InMemorySmSessionRegistry,
    SmClaimCompletion, SmRegistryError,
};

async fn claimed_registry(
    stream: &str,
) -> (
    Arc<InMemorySmSessionRegistry>,
    Arc<GatedSnapshotPersistence>,
    Arc<InProcessClaimStore>,
    SharedNodeIdentity,
    SmClaimFence,
) {
    let storage = Arc::new(GatedSnapshotPersistence::new(stream));
    let claims = Arc::new(InProcessClaimStore::new());
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node-a", "incarnation-a"));
    let registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claims.clone(), identity.clone()),
    );
    registry
        .store_session(realistic_test_session(stream))
        .await
        .unwrap();
    registry.claim_session(stream).await.unwrap().unwrap();
    let fence = registry.current_sm_claim_fence(stream).unwrap();
    (registry, storage, claims, identity, fence)
}

fn expire_claim(registry: &InMemorySmSessionRegistry, stream: &str) {
    registry
        .claimed_sessions
        .write()
        .unwrap()
        .get_mut(stream)
        .unwrap()
        .max_resume_time = Some(0);
}

async fn assert_claim_held(claims: &InProcessClaimStore, stream: &str, fence: &SmClaimFence) {
    let held = claims
        .current_claim(&Entity::new(EntityType::SmSession, stream.to_owned()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&held.owner, fence.owner());
    assert_eq!(held.claim_epoch, fence.epoch());
}

fn assert_queue_equal(actual: &[PersistedUnackedStanza], expected: &[PersistedUnackedStanza]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.stream_id, expected.stream_id);
        assert_eq!(actual.sequence, expected.sequence);
        assert_eq!(actual.stanza.to_element(), expected.stanza.to_element());
        assert_eq!(actual.original_receipt_at, expected.original_receipt_at);
        assert_eq!(actual.ingress_receipts, expected.ingress_receipts);
    }
}

#[tokio::test]
async fn failed_expiry_conversion_retains_source_and_drain_retry() {
    let stream = "failed-expiry-conversion";
    let (registry, storage, claims, _, fence) = claimed_registry(stream).await;
    let id = SmSessionId::new(stream);
    let queue = storage.inner.list_unacked(&id).await.unwrap();
    expire_claim(&registry, stream);
    storage.fail_after_gate.store(true, Ordering::SeqCst);

    assert!(registry.complete_claim(stream).await.is_err());
    assert!(registry.defer_claimed_resume_release(stream));
    assert_claim_held(&claims, stream, &fence).await;
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert!(storage.inner.get_session(&id).await.unwrap().is_some());
    assert_queue_equal(&storage.inner.list_unacked(&id).await.unwrap(), &queue);

    storage.fail_after_gate.store(false, Ordering::SeqCst);
    let drained = registry.drain_expired().await.unwrap();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].unacked_stanzas.len(), queue.len());
    assert_eq!(drained[0].promotion_custody.captured_fence(), Some(&fence));
    assert!(registry.drain_expired().await.unwrap().is_empty());
}

#[tokio::test]
async fn cancelled_expiry_conversion_retains_custody_before_and_after_commit() {
    for commit_first in [false, true] {
        let stream = if commit_first {
            "expiry-cancel-after-commit"
        } else {
            "expiry-cancel-before-commit"
        };
        let (registry, storage, claims, _, fence) = claimed_registry(stream).await;
        let id = SmSessionId::new(stream);
        let queue = storage.inner.list_unacked(&id).await.unwrap();
        expire_claim(&registry, stream);
        storage.gate_nonresumable.store(true, Ordering::SeqCst);
        storage
            .commit_then_gate
            .store(commit_first, Ordering::SeqCst);
        let completing_registry = registry.clone();
        let completion =
            tokio::spawn(async move { completing_registry.complete_claim(stream).await });
        storage.reached.notified().await;
        completion.abort();
        assert!(completion.await.unwrap_err().is_cancelled());
        storage.gate_nonresumable.store(false, Ordering::SeqCst);

        assert!(registry.defer_claimed_resume_release(stream));
        assert_claim_held(&claims, stream, &fence).await;
        assert_eq!(registry.pending_claim_release_count(), 0);
        let source = storage.inner.get_session(&id).await.unwrap().unwrap();
        if commit_first {
            assert_eq!(source.max_resume_time, Some(0));
            assert_eq!(source.max_resume_duration, Duration::ZERO);
        }
        assert_queue_equal(&storage.inner.list_unacked(&id).await.unwrap(), &queue);
        let drained = registry.drain_expired().await.unwrap();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].promotion_custody.captured_fence(), Some(&fence));
        assert_eq!(drained[0].unacked_stanzas.len(), queue.len());
        assert!(registry.drain_expired().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn expiry_conversion_and_retry_never_recreate_missing_source() {
    let stream = "expiry-missing-source";
    let (registry, storage, claims, _, fence) = claimed_registry(stream).await;
    let id = SmSessionId::new(stream);
    expire_claim(&registry, stream);
    storage.inner.delete_session(&id).await.unwrap();

    assert!(registry.complete_claim(stream).await.is_err());
    assert!(registry.drain_expired().await.is_err());
    assert!(storage.inner.get_session(&id).await.unwrap().is_none());
    assert!(storage.inner.list_unacked(&id).await.unwrap().is_empty());
    assert_claim_held(&claims, stream, &fence).await;
    assert_eq!(registry.pending_claim_release_count(), 0);
}

#[tokio::test]
async fn successful_resume_is_not_reclassified_after_awaited_delete() {
    let stream = "resume-crosses-expiry-during-delete";
    let (registry, storage, claims, _, fence) = claimed_registry(stream).await;
    storage.gate_delete.store(true, Ordering::SeqCst);
    let completing_registry = registry.clone();
    let completion = tokio::spawn(async move { completing_registry.complete_claim(stream).await });
    storage.reached.notified().await;
    // Deterministically cross the expiry boundary while deletion is pending.
    expire_claim(&registry, stream);
    storage.proceed.notify_one();

    assert!(matches!(
        completion.await.unwrap().unwrap(),
        Some(SmClaimCompletion::Resumed(_))
    ));
    assert!(storage
        .inner
        .get_session(&SmSessionId::new(stream))
        .await
        .unwrap()
        .is_none());
    assert_claim_held(&claims, stream, &fence).await;
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert!(registry.drain_expired().await.unwrap().is_empty());
}

#[tokio::test]
async fn authoritative_expiry_completion_finishes_before_queued_rotation() {
    let stream = "authoritative-expiry-with-rotation";
    let (registry, storage, _, identity, fence) = claimed_registry(stream).await;
    expire_claim(&registry, stream);
    let operation = registry.lock_session_operation(stream).await.unwrap();
    let authority = identity.guard_if_current(fence.owner()).await.unwrap();
    let rotating_identity = identity.clone();
    let rotation = tokio::spawn(async move {
        rotating_identity
            .rotate(NodeIdentity::new("node-a", "incarnation-b"))
            .await;
    });
    tokio::task::yield_now().await;

    let completion = tokio::time::timeout(
        Duration::from_secs(1),
        registry.complete_claim_with_authority(operation, &authority),
    )
    .await
    .expect("expiry conversion must reuse the held identity authority")
    .unwrap();
    assert!(matches!(completion, Some(SmClaimCompletion::Expired(_))));
    assert_eq!(
        storage
            .inner
            .get_session(&SmSessionId::new(stream))
            .await
            .unwrap()
            .unwrap()
            .max_resume_time,
        Some(0)
    );
    drop(authority);
    tokio::time::timeout(Duration::from_secs(1), rotation)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn stolen_exact_claim_cannot_convert_or_release_successor_source() {
    let stream = "expiry-stolen-exact-fence";
    let (registry, storage, claims, _, fence) = claimed_registry(stream).await;
    let id = SmSessionId::new(stream);
    let original = storage.inner.get_session(&id).await.unwrap().unwrap();
    expire_claim(&registry, stream);
    let entity = Entity::new(EntityType::SmSession, stream.to_owned());
    claims
        .release(&entity, fence.owner(), fence.epoch())
        .await
        .unwrap();
    let successor = NodeIdentity::new("node-b", "incarnation-b");
    let successor_epoch = claims.ensure_claimed(&entity, &successor).await.unwrap();

    assert!(matches!(
        registry.complete_claim(stream).await,
        Err(SmRegistryError::PromotionClaimLost(_))
    ));
    assert!(registry.drain_expired().await.unwrap().is_empty());
    assert_eq!(
        storage
            .inner
            .get_session(&id)
            .await
            .unwrap()
            .unwrap()
            .max_resume_time,
        original.max_resume_time
    );
    assert_claim_held(
        &claims,
        stream,
        &SmClaimFence::new(successor, successor_epoch),
    )
    .await;
    assert_eq!(registry.pending_claim_release_count(), 0);
}
