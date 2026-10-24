use std::sync::{atomic::Ordering, Arc};

use super::{
    make_test_session, realistic_test_session_for_jid, EnsureClaimTestAction,
    FailingSnapshotPersistence, GatedSnapshotPersistence, HangingReleaseClaimStore,
};
use crate::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};
use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::{InMemorySmPersistence, SmPersistenceStorage};
use crate::stream_management::{
    InMemorySmSessionRegistry, PendingPromotionRetryRetention, PromotionCaptureCustody,
    SmRegistryError, SmSessionRegistry,
};

#[tokio::test]
async fn stale_off_map_capture_cannot_recreate_successor_completed_source() {
    for rotate_original in [false, true] {
        let storage = Arc::new(InMemorySmPersistence::new());
        let claims = Arc::new(InProcessClaimStore::new());
        let original = NodeIdentity::new("original", "first");
        let identity = SharedNodeIdentity::new(original.clone());
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claims.clone(), identity.clone());
        let mut session = realistic_test_session_for_jid(
            "stale-off-map",
            "alice@example.com/web".parse().unwrap(),
        );
        session.max_resume_time = Some(0);
        let stream = SmSessionId::new(&session.stream_id);
        registry.store_session(session).await.unwrap();
        let capture = registry.drain_expired().await.unwrap().remove(0);
        let expected = capture.promotion_custody.fence().unwrap().clone();
        let entity = Entity::new(EntityType::SmSession, stream.as_str());

        assert!(registry.forget_claim_locally(stream.as_str()).await);
        claims
            .release_exact(&entity, &original, expected.epoch())
            .await
            .unwrap();
        let successor = InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(
                claims.clone(),
                SharedNodeIdentity::new(NodeIdentity::new("successor", "first")),
            );
        assert_eq!(successor.restore_from_persistence().await.unwrap(), 1);
        let recovered = successor.drain_expired().await.unwrap();
        assert_eq!(
            recovered[0].unacked_stanzas.len(),
            capture.unacked_stanzas.len()
        );
        assert!(successor.confirm_drained(stream.as_str()).await);
        assert!(storage.get_session(&stream).await.unwrap().is_none());
        if rotate_original {
            identity
                .rotate(NodeIdentity::new("original", "second"))
                .await;
        }

        assert!(matches!(
            registry.register_promotion_session(&capture).await,
            Err(SmRegistryError::PromotionClaimLost(_))
        ));
        assert!(storage.get_session(&stream).await.unwrap().is_none());
        assert!(storage.list_unacked(&stream).await.unwrap().is_empty());
        assert!(claims.current_claim(&entity).await.unwrap().is_none());
        assert!(registry.pending_promotions.read().unwrap().is_empty());
        assert_eq!(
            registry
                .retain_pending_promotion_for_retry(capture)
                .unwrap(),
            PendingPromotionRetryRetention::NotTracked,
        );
    }
}

#[tokio::test]
async fn pre_registration_clone_keeps_first_fence_after_source_completion() {
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let fresh = realistic_test_session_for_jid(
        "fresh-capture-clone",
        "alice@example.com/web".parse().unwrap(),
    );
    let old_clone = fresh.clone();
    assert!(old_clone.promotion_custody.fence().is_none());
    let inventory = registry.register_promotion_session(&fresh).await.unwrap();
    assert_eq!(
        old_clone.promotion_custody.fence(),
        inventory.promotion_custody.fence()
    );
    assert!(registry.confirm_drained(&fresh.stream_id).await);
    assert!(matches!(
        registry.register_promotion_session(&old_clone).await,
        Err(SmRegistryError::PromotionClaimLost(_))
    ));
    assert!(storage
        .get_session(&SmSessionId::new(&fresh.stream_id))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn old_capture_cannot_borrow_newer_same_identity_claim_epoch() {
    let registry = InMemorySmSessionRegistry::new();
    let fresh = make_test_session("fresh-epoch-clone");
    let inventory = registry.register_promotion_session(&fresh).await.unwrap();
    let original = inventory.promotion_custody.fence().unwrap().clone();
    assert!(registry.confirm_drained(&fresh.stream_id).await);
    drop(
        registry
            .ensure_session_claim(&fresh.stream_id)
            .await
            .unwrap(),
    );
    let replacement = registry.current_sm_claim_fence(&fresh.stream_id).unwrap();
    assert_ne!(original.epoch(), replacement.epoch());
    assert!(matches!(
        registry.register_promotion_session(&inventory).await,
        Err(SmRegistryError::PromotionClaimLost(_))
    ));
    assert_eq!(
        registry.current_sm_claim_fence(&fresh.stream_id),
        Some(replacement)
    );
}

#[tokio::test]
async fn first_snapshot_failure_retains_exact_fence_and_payload_for_retry() {
    let storage = Arc::new(FailingSnapshotPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let capture = realistic_test_session_for_jid(
        "capture-snapshot-failure",
        "alice@example.com/web".parse().unwrap(),
    );
    let retry_clone = capture.clone();
    storage.fail_snapshots.store(true, Ordering::SeqCst);
    assert!(registry.register_promotion_session(&capture).await.is_err());
    let expected = retry_clone.promotion_custody.fence().unwrap().clone();
    storage.fail_snapshots.store(false, Ordering::SeqCst);
    let recovered = registry
        .register_promotion_session(&retry_clone)
        .await
        .unwrap();
    assert_eq!(recovered.promotion_custody.fence(), Some(&expected));
    assert_eq!(
        recovered.unacked_stanzas.len(),
        capture.unacked_stanzas.len()
    );
    assert_eq!(
        storage
            .list_unacked(&SmSessionId::new(&capture.stream_id))
            .await
            .unwrap()
            .len(),
        capture.unacked_stanzas.len()
    );
}

#[tokio::test]
async fn raw_unproven_capture_cannot_admit_itself_through_empty_cache() {
    let registry = InMemorySmSessionRegistry::new();
    let mut capture = make_test_session("unproven-capture");
    capture.promotion_custody = PromotionCaptureCustody::unproven();
    assert!(matches!(
        registry.register_promotion_session(&capture).await,
        Err(SmRegistryError::StorageUnavailable(_))
    ));
    assert!(registry
        .current_sm_claim_fence(&capture.stream_id)
        .is_none());
    assert!(registry.pending_promotions.read().unwrap().is_empty());
}

#[tokio::test]
async fn stale_registered_capture_cannot_mutate_replacement_inventory() {
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let old = registry
        .register_promotion_session(&realistic_test_session_for_jid(
            "replacement-source",
            "alice@example.com/web".parse().unwrap(),
        ))
        .await
        .unwrap();
    assert!(registry.confirm_promotion_session(&old).await);
    let replacement = registry
        .register_promotion_session(&realistic_test_session_for_jid(
            "replacement-source",
            "alice@example.com/new".parse().unwrap(),
        ))
        .await
        .unwrap();
    assert_ne!(
        old.promotion_custody.fence(),
        replacement.promotion_custody.fence()
    );
    assert!(registry.lock_promotion_operation(&old).await.is_err());
    assert!(registry.refresh_promotion_session(&old).await.is_err());
    assert!(registry
        .record_promotion_session_failure(&old)
        .await
        .is_err());
    assert!(registry
        .delete_promotion_unacked_sequences(&old, &[1, 2])
        .await
        .is_err());
    assert!(!registry.confirm_promotion_session(&old).await);
    let stream = SmSessionId::new(&replacement.stream_id);
    assert_eq!(
        storage.list_unacked(&stream).await.unwrap().len(),
        replacement.unacked_stanzas.len()
    );
    assert_eq!(
        storage.get_session(&stream).await.unwrap().unwrap().jid,
        replacement.jid
    );
    assert_eq!(
        registry
            .current_sm_claim_fence(&replacement.stream_id)
            .as_ref(),
        replacement.promotion_custody.fence()
    );
}

#[tokio::test]
async fn ambiguous_fresh_capture_cannot_reacquire_after_successor_consumption() {
    let storage = Arc::new(InMemorySmPersistence::new());
    let claims = Arc::new(InProcessClaimStore::new());
    let identity = NodeIdentity::new("original", "first");
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(claims.clone(), SharedNodeIdentity::new(identity.clone()));
    let mut capture = make_test_session("ambiguous-fresh-capture");
    capture.max_resume_time = Some(0);
    assert!(capture.promotion_custody.begin_first_claim(&identity));
    let stream = SmSessionId::new(&capture.stream_id);
    let entity = Entity::new(EntityType::SmSession, &capture.stream_id);
    // Model a detach snapshot landing, followed by an acquisition whose
    // response was cancelled before the exact epoch could be recorded.
    storage
        .store_session_atomic(
            super::super::persistence_codec::detached_to_persisted(&capture).unwrap(),
            Vec::new(),
        )
        .await
        .unwrap();
    let epoch = claims.ensure_claimed(&entity, &identity).await.unwrap();
    claims
        .release_exact(&entity, &identity, epoch)
        .await
        .unwrap();
    let successor = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(
            claims.clone(),
            SharedNodeIdentity::new(NodeIdentity::new("successor", "first")),
        );
    assert_eq!(successor.restore_from_persistence().await.unwrap(), 1);
    assert_eq!(successor.drain_expired().await.unwrap().len(), 1);
    assert!(successor.confirm_drained(stream.as_str()).await);
    assert!(registry.register_promotion_session(&capture).await.is_err());
    assert!(claims.current_claim(&entity).await.unwrap().is_none());
    assert!(storage.get_session(&stream).await.unwrap().is_none());
}

#[derive(Clone)]
struct CapturePublicationPause {
    entered: Arc<std::sync::Barrier>,
    proceed: Arc<std::sync::Barrier>,
}

static CAPTURE_PUBLICATION_PAUSES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, CapturePublicationPause>>,
> = std::sync::OnceLock::new();

pub(crate) fn pause_capture_publication(stream_id: &str) {
    let pause = CAPTURE_PUBLICATION_PAUSES
        .get()
        .and_then(|pauses| pauses.lock().unwrap().get(stream_id).cloned());
    if let Some(pause) = pause {
        pause.entered.wait();
        pause.proceed.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_publication_serializes_demotion_and_same_occupancy_epoch_reuse() {
    for registration in [false, true] {
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let old = registry
            .register_promotion_session(&realistic_test_session_for_jid(
                if registration {
                    "atomic-register"
                } else {
                    "atomic-retry"
                },
                "alice@example.com/web".parse().unwrap(),
            ))
            .await
            .unwrap();
        let mut stale_tail = old.clone();
        let mut extra = stale_tail.unacked_stanzas[0].clone();
        extra.sequence = stale_tail.outbound_count.wrapping_add(1);
        stale_tail.outbound_count = extra.sequence;
        stale_tail.unacked_stanzas.push(extra);
        let pause = CapturePublicationPause {
            entered: Arc::new(std::sync::Barrier::new(2)),
            proceed: Arc::new(std::sync::Barrier::new(2)),
        };
        CAPTURE_PUBLICATION_PAUSES
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(old.stream_id.clone(), pause.clone());
        let publishing_registry = registry.clone();
        let captured = stale_tail.clone();
        let publication = tokio::task::spawn_blocking(move || {
            if registration {
                tokio::runtime::Handle::current().block_on(async {
                    let result = publishing_registry
                        .register_promotion_session(&captured)
                        .await;
                    assert!(
                        result.is_ok()
                            || matches!(result, Err(SmRegistryError::PromotionClaimLost(_)))
                    );
                });
            } else {
                assert_eq!(
                    publishing_registry
                        .retain_pending_promotion_for_retry(captured)
                        .unwrap(),
                    PendingPromotionRetryRetention::Retained
                );
            }
        });
        let entered = pause.entered.clone();
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .unwrap();
        assert!(
            registry.pending_promotions.try_write().is_err(),
            "capture validation must retain the pending inventory gate until mutation"
        );
        if !registration {
            assert!(
                registry.pending_promotion_retries.try_read().is_err(),
                "retry transfer endpoint must remain locked through validation and merge"
            );
        }
        let replacement_registry = registry.clone();
        let source = old.clone();
        let (started, pending_demotion) = tokio::sync::oneshot::channel();
        let replacement = tokio::spawn(async move {
            started.send(()).unwrap();
            replacement_registry
                .forget_claim_locally(&source.stream_id)
                .await;
            let expected = source.promotion_custody.fence().unwrap();
            let entity = Entity::new(EntityType::SmSession, &source.stream_id);
            replacement_registry
                .claim_store
                .release_exact(&entity, expected.owner(), expected.epoch())
                .await
                .unwrap();
            drop(
                replacement_registry
                    .ensure_session_claim(&source.stream_id)
                    .await
                    .unwrap(),
            );
            let mut next = source;
            next.promotion_custody = PromotionCaptureCustody::claimed(
                replacement_registry
                    .current_sm_claim_fence(&next.stream_id)
                    .unwrap(),
            );
            replacement_registry
                .register_promotion_session(&next)
                .await
                .unwrap();
            replacement_registry
                .retain_pending_promotion_for_retry(next.clone())
                .unwrap();
            next
        });
        pending_demotion.await.unwrap();
        tokio::task::yield_now().await;
        assert!(
            !replacement.is_finished(),
            "demotion must wait for capture publication"
        );
        CAPTURE_PUBLICATION_PAUSES
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .remove(&old.stream_id);
        let proceed = pause.proceed.clone();
        tokio::task::spawn_blocking(move || proceed.wait())
            .await
            .unwrap();
        publication.await.unwrap();
        let next = replacement.await.unwrap();
        assert_eq!(
            registry
                .retain_pending_promotion_for_retry(stale_tail)
                .unwrap(),
            PendingPromotionRetryRetention::NotTracked
        );
        let retries = registry.pending_promotion_retries.read().unwrap();
        let retained = retries.get(&next.stream_id).unwrap();
        assert_eq!(
            retained.promotion_custody.fence(),
            next.promotion_custody.fence()
        );
        assert_eq!(
            retained.unacked_stanzas.len(),
            next.unacked_stanzas.len(),
            "old-only tail must never merge into the new epoch's retry capture"
        );
    }
}

#[tokio::test]
async fn shutdown_and_expiry_reuse_one_authoritative_inventory_registration() {
    for shutdown in [false, true] {
        let storage = Arc::new(FailingSnapshotPersistence::new());
        let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        let mut session = realistic_test_session_for_jid(
            "single-drain-registration",
            "alice@example.com/web".parse().unwrap(),
        );
        if !shutdown {
            session.max_resume_time = Some(0);
        }
        registry.store_session(session).await.unwrap();
        storage.get_session_calls.store(0, Ordering::SeqCst);
        storage.list_unacked_calls.store(0, Ordering::SeqCst);
        let drained = if shutdown {
            registry.drain_all_for_shutdown().await.unwrap().sessions
        } else {
            registry.drain_expired().await.unwrap()
        };
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].unacked_stanzas.len(), 2);
        assert_eq!(
            storage.get_session_calls.load(Ordering::SeqCst),
            2,
            "one metadata read plus the authoritative post-registration read"
        );
        assert_eq!(
            storage.list_unacked_calls.load(Ordering::SeqCst),
            1,
            "the handed-out queue must reuse the registration's authoritative rows"
        );
        assert!(registry.confirm_promotion_session(&drained[0]).await);
    }
}

#[tokio::test]
async fn reserved_ambiguous_resume_resolves_its_capture_without_replaying_acquisition() {
    for completed_by_successor in [false, true] {
        let storage = Arc::new(InMemorySmPersistence::new());
        let claims = Arc::new(HangingReleaseClaimStore {
            inner: InProcessClaimStore::new(),
            hang_release: std::sync::atomic::AtomicBool::new(false),
            hang_ensure: std::sync::atomic::AtomicBool::new(false),
            commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
            poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
        });
        let owner = NodeIdentity::new("ambiguous-capture", "first");
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claims.clone(), SharedNodeIdentity::new(owner.clone()));
        let source = realistic_test_session_for_jid(
            "reserved-ambiguous-capture",
            "alice@example.com/web".parse().unwrap(),
        );
        let stream = SmSessionId::new(&source.stream_id);
        registry.store_session(source).await.unwrap();
        let earlier = registry
            .sessions
            .read()
            .unwrap()
            .get(stream.as_str())
            .unwrap()
            .clone();
        *claims.poison_fence_cache_after_ensure.lock().unwrap() =
            Some(EnsureClaimTestAction::ReplaceClaimThenBackendError);
        assert!(registry.claim_session(stream.as_str()).await.is_err());
        let reserved = registry
            .sessions
            .read()
            .unwrap()
            .get(stream.as_str())
            .unwrap()
            .clone();
        assert!(matches!(
            reserved.promotion_custody,
            PromotionCaptureCustody::Acquiring(_)
        ));
        assert!(reserved.promotion_custody.fence().is_none());
        let entity = Entity::new(EntityType::SmSession, stream.as_str());
        let acquired = claims.current_claim(&entity).await.unwrap().unwrap();
        assert_ne!(
            earlier.promotion_custody.fence().unwrap().epoch(),
            acquired.claim_epoch
        );
        // A second same-owner CAS would execute this mutation. Both a new
        // resume and pending reconciliation must leave it untouched.
        *claims.poison_fence_cache_after_ensure.lock().unwrap() =
            Some(EnsureClaimTestAction::ReplaceClaimThenBackendError);
        assert!(registry.claim_session(stream.as_str()).await.is_err());
        if completed_by_successor {
            storage.delete_session(&stream).await.unwrap();
            claims
                .release_exact(&entity, &owner, acquired.claim_epoch)
                .await
                .unwrap();
        }
        registry.retry_pending_claim_releases(1).await;
        assert!(
            claims
                .poison_fence_cache_after_ensure
                .lock()
                .unwrap()
                .is_some(),
            "neither overlap nor reconciliation may issue another acquisition"
        );
        if completed_by_successor {
            assert!(reserved.promotion_custody.fence().is_none());
            assert!(registry
                .register_promotion_session(&reserved)
                .await
                .is_err());
            assert!(claims.current_claim(&entity).await.unwrap().is_none());
            assert!(storage.get_session(&stream).await.unwrap().is_none());
        } else {
            assert_eq!(
                reserved.promotion_custody.fence().unwrap().epoch(),
                acquired.claim_epoch
            );
            assert!(matches!(
                registry.register_promotion_session(&earlier).await,
                Err(SmRegistryError::PromotionClaimLost(_))
            ));
            assert!(registry.register_promotion_session(&reserved).await.is_ok());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_known_resume_result_retains_displaced_payload_before_cache_publication() {
    let original_id = "known-result-before-cache";
    let probe = InMemorySmSessionRegistry::new();
    let old_shard = probe.stream_lock(original_id).unwrap();
    let replacement_id = (0..1024)
        .map(|index| format!("known-result-replacement-{index}"))
        .find(|candidate| !Arc::ptr_eq(&old_shard, &probe.stream_lock(candidate).unwrap()))
        .unwrap();
    let storage = Arc::new(GatedSnapshotPersistence::new(&replacement_id));
    let claims = Arc::new(InProcessClaimStore::new());
    let owner = NodeIdentity::new("known-result", "first");
    let identity = SharedNodeIdentity::new(owner.clone());
    let registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claims.clone(), identity.clone()),
    );
    let jid: jid::FullJid = "alice@example.com/web".parse().unwrap();
    registry
        .store_session(realistic_test_session_for_jid(original_id, jid.clone()))
        .await
        .unwrap();
    let predecessor = registry.current_sm_claim_fence(original_id).unwrap();
    let entity = Entity::new(EntityType::SmSession, original_id);
    claims
        .release_exact(&entity, &owner, predecessor.epoch())
        .await
        .unwrap();

    // Queue an exclusive publication transition behind this held authority.
    // The successful resume CAS may return B, but publishing B must wait.
    let authority = identity.guard_if_current(&owner).await.unwrap();
    let writer = identity.with_publications_blocked(|| ());
    tokio::pin!(writer);
    assert!(
        futures::poll!(&mut writer).is_pending(),
        "exclusive publication must be queued before resume starts"
    );
    let resuming = registry.clone();
    let resume = tokio::spawn(async move { resuming.claim_session(original_id).await });
    let acquired = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let result = registry
                .pending_resume_claim_custody
                .read()
                .unwrap()
                .get(original_id)
                .and_then(|proof| proof.fence().cloned());
            if let Some(result) = result {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resume binds its known result before awaiting publication");
    assert_ne!(acquired.epoch(), predecessor.epoch());
    assert_eq!(
        registry.current_sm_claim_fence(original_id),
        Some(predecessor)
    );

    storage.armed.store(true, Ordering::SeqCst);
    let replacing = registry.clone();
    let replacement = tokio::spawn(async move {
        replacing
            .store_session(realistic_test_session_for_jid(&replacement_id, jid))
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        storage.reached.notified(),
    )
    .await
    .expect("other-shard store displaces the token-bearing source");
    replacement.abort();
    assert!(replacement.await.unwrap_err().is_cancelled());
    resume.abort();
    assert!(resume.await.unwrap_err().is_cancelled());
    let retained = registry
        .pending_promotion_retries
        .read()
        .unwrap()
        .get(original_id)
        .expect("cancellation must preserve the displaced payload")
        .clone();
    assert_eq!(retained.promotion_custody.fence(), Some(&acquired));
    assert_eq!(retained.unacked_stanzas.len(), 2);
    assert!(
        registry.lock_promotion_operation(&retained).await.is_err(),
        "retention permission must not authorize effects before exact cache publication"
    );

    drop(authority);
    writer.await;
    registry.retry_pending_claim_releases(1).await;
    assert_eq!(registry.current_sm_claim_fence(original_id), Some(acquired));
    let drained = registry.drain_expired().await.unwrap();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].stream_id, original_id);
    assert_eq!(drained[0].unacked_stanzas.len(), 2);
}
