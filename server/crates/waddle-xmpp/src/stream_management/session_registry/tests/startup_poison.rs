use super::{
    realistic_test_session, EnsureClaimTestAction, GatedGetSessionPersistence,
    HangingReleaseClaimStore, InMemorySmSessionRegistry, SmSessionRegistry,
};
use crate::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};
use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::SmPersistenceStorage;
use std::sync::atomic::Ordering;
use std::sync::Arc;

async fn poison_storage(stream: &str, corrupt_queue: bool) -> Arc<GatedGetSessionPersistence> {
    let storage = Arc::new(GatedGetSessionPersistence::new(stream));
    let initial = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    initial
        .store_session(realistic_test_session(stream))
        .await
        .unwrap();
    if corrupt_queue {
        storage.corrupt_unacked.store(true, Ordering::SeqCst);
    } else {
        storage.corrupt.store(true, Ordering::SeqCst);
    }
    storage
}

#[tokio::test]
async fn startup_quarantines_poison_omitted_from_bulk_restore() {
    for corrupt_queue in [false, true] {
        let stream = "startup-omitted-poison";
        let storage = poison_storage(stream, corrupt_queue).await;
        let claims = Arc::new(InProcessClaimStore::new());
        let owner = NodeIdentity::new("startup", "incarnation");
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claims.clone(), SharedNodeIdentity::new(owner));

        assert!(storage
            .list_all_sessions_with_unacked()
            .await
            .unwrap()
            .is_empty());
        assert_eq!(registry.restore_from_persistence().await.unwrap(), 0);
        let id = SmSessionId::new(stream);
        assert!(storage.inner.get_session(&id).await.unwrap().is_none());
        assert!(storage.inner.list_unacked(&id).await.unwrap().is_empty());
        assert!(claims
            .current_claim(&Entity::new(EntityType::SmSession, stream))
            .await
            .unwrap()
            .is_none());
        assert_eq!(registry.pending_reclaimed_hydration_count(), 0);
    }
}

#[tokio::test]
async fn startup_does_not_quarantine_poison_owned_by_another_node() {
    let stream = "startup-foreign-poison";
    let storage = poison_storage(stream, false).await;
    let claims = Arc::new(InProcessClaimStore::new());
    let entity = Entity::new(EntityType::SmSession, stream);
    let foreign = NodeIdentity::new("foreign", "incarnation");
    let epoch = claims.acquire(&entity, &foreign).await.unwrap();
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(
            claims.clone(),
            SharedNodeIdentity::new(NodeIdentity::new("startup", "incarnation")),
        );

    assert_eq!(registry.restore_from_persistence().await.unwrap(), 0);
    assert!(storage
        .inner
        .get_session(&SmSessionId::new(stream))
        .await
        .unwrap()
        .is_some());
    assert!(!storage.quarantined.load(Ordering::SeqCst));
    assert!(claims.fence(&entity, &foreign, epoch).await.unwrap());
    assert_eq!(registry.pending_reclaimed_hydration_count(), 0);
}

#[tokio::test]
async fn startup_retains_poison_responsibility_after_transient_read_failure() {
    let stream = "startup-transient-poison";
    let storage = poison_storage(stream, false).await;
    let claims = Arc::new(InProcessClaimStore::new());
    let registry = InMemorySmSessionRegistry::with_capacity(1)
        .with_persistence(storage.clone())
        .with_claim_store(
            claims.clone(),
            SharedNodeIdentity::new(NodeIdentity::new("startup", "incarnation")),
        );
    storage.fail_get_once.store(true, Ordering::SeqCst);

    assert_eq!(registry.restore_from_persistence().await.unwrap(), 0);
    assert!(storage
        .inner
        .get_session(&SmSessionId::new(stream))
        .await
        .unwrap()
        .is_some());
    assert_eq!(registry.pending_reclaimed_hydration_count(), 1);
    assert!(registry
        .reserve_reclaimed_claim_capacity(&Entity::new(EntityType::SmSession, "other"))
        .is_none());
    assert_eq!(registry.retry_pending_reclaimed_hydrations(1).await, 1);
    assert!(storage
        .inner
        .get_session(&SmSessionId::new(stream))
        .await
        .unwrap()
        .is_none());
    assert!(claims
        .current_claim(&Entity::new(EntityType::SmSession, stream))
        .await
        .unwrap()
        .is_none());
    assert_eq!(registry.pending_reclaimed_hydration_count(), 0);
}

#[tokio::test]
async fn cancelled_startup_poison_claim_is_resolved_by_existing_lookup_retry() {
    let stream = "startup-cancelled-poison";
    let storage = poison_storage(stream, false).await;
    let reached = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
    let claims = Arc::new(HangingReleaseClaimStore {
        inner: InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(Some(
            EnsureClaimTestAction::Pause {
                reached: reached.clone(),
                proceed,
            },
        )),
    });
    let registry = Arc::new(
        InMemorySmSessionRegistry::with_capacity(1)
            .with_persistence(storage.clone())
            .with_claim_store(
                claims.clone(),
                SharedNodeIdentity::new(NodeIdentity::new("startup", "incarnation")),
            ),
    );
    let restoring = registry.clone();
    let task = tokio::spawn(async move { restoring.restore_from_persistence().await });
    reached.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        registry
            .pending_reclaimed_claim_lookups
            .read()
            .unwrap()
            .len(),
        1
    );
    assert!(claims
        .current_claim(&Entity::new(EntityType::SmSession, stream))
        .await
        .unwrap()
        .is_some());

    assert_eq!(registry.retry_pending_reclaimed_hydrations(1).await, 1);
    assert!(storage
        .inner
        .get_session(&SmSessionId::new(stream))
        .await
        .unwrap()
        .is_none());
    assert!(claims
        .current_claim(&Entity::new(EntityType::SmSession, stream))
        .await
        .unwrap()
        .is_none());
    assert!(registry
        .pending_reclaimed_claim_lookups
        .read()
        .unwrap()
        .is_empty());
    assert!(registry
        .reclaimed_claim_reservations
        .read()
        .unwrap()
        .is_empty());
}
