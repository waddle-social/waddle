use super::*;

#[tokio::test]
async fn fresh_detached_append_recovers_only_an_existing_claim_without_acquiring() {
    use crate::ownership::{
        ClaimStore as _, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };
    use crate::pending_delivery::SmSessionId;
    use crate::stream_management::persistence::{InMemorySmPersistence, SmPersistenceStorage as _};
    use std::sync::{atomic::AtomicBool, Arc, Mutex};

    for existing_claim in [true, false] {
        let stream = "cancelled-before-detach-acquire";
        let entity = Entity::new(EntityType::SmSession, stream);
        let owner = NodeIdentity::new("detach-node", "incarnation");
        let claims = Arc::new(HangingReleaseClaimStore {
            inner: InProcessClaimStore::new(),
            hang_release: AtomicBool::new(false),
            hang_ensure: AtomicBool::new(false),
            commit_then_hang_ensure_once: AtomicBool::new(false),
            poison_fence_cache_after_ensure: Mutex::new(None),
        });
        let storage = Arc::new(InMemorySmPersistence::new());
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claims.clone(), SharedNodeIdentity::new(owner.clone()));
        registry
            .store_session(realistic_test_session(stream))
            .await
            .unwrap();
        let original_fence = registry.current_sm_claim_fence(stream).unwrap();
        // A cancelled initial snapshot can leave a published fresh capture
        // before acquire-on-detach records its first attempted owner.
        registry
            .sessions
            .write()
            .unwrap()
            .get_mut(stream)
            .unwrap()
            .promotion_custody = PromotionCaptureCustody::never_claimed();
        registry.claim_fences.write().unwrap().remove(stream);
        if !existing_claim {
            claims
                .inner
                .release_exact(&entity, &owner, original_fence.epoch())
                .await
                .unwrap();
        }
        let claim_before = claims.current_claim(&entity).await.unwrap();
        let reached = Arc::new(tokio::sync::Notify::new());
        *claims.poison_fence_cache_after_ensure.lock().unwrap() =
            Some(EnsureClaimTestAction::Pause {
                reached: reached.clone(),
                proceed: Arc::new(tokio::sync::Notify::new()),
            });
        let stanza = message_stanza_with_id("readonly-detach-recovery".into());
        let appended = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                biased;
                () = reached.notified() => {
                    panic!("append recovery must never call ensure_claimed for a fresh capture")
                }
                outcome = registry.record_outbound_for_detached_stream(stream, &stanza, Utc::now()) => outcome,
            }
        })
        .await
        .expect("read-only append recovery must complete")
        .expect("portable append can retain a source without a claim");
        assert!(appended);
        assert_eq!(claims.current_claim(&entity).await.unwrap(), claim_before);
        let session = registry.peek_session(stream).await.unwrap().unwrap();
        if existing_claim {
            assert_eq!(
                session.promotion_custody.captured_fence(),
                Some(&original_fence)
            );
            assert_eq!(
                registry.current_sm_claim_fence(stream),
                Some(original_fence)
            );
        } else {
            assert!(session.promotion_custody.captured_fence().is_none());
            assert!(session.promotion_custody.first_claim_owner().is_none());
            assert_eq!(registry.current_sm_claim_fence(stream), None);
        }
        let stored = storage
            .get_session(&SmSessionId::new(stream))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.outbound_count, 8);
        let queue = storage
            .list_unacked(&SmSessionId::new(stream))
            .await
            .unwrap();
        assert_eq!(queue.last().unwrap().sequence, 8);
        assert_eq!(
            queue.last().unwrap().stanza.to_element(),
            stanza.to_element()
        );
    }
}

/// Two historical shard colliders must serialize only their own stream.
#[tokio::test]
async fn a_busy_detached_stream_does_not_block_an_unrelated_stream_operation() {
    use std::hash::{Hash, Hasher};
    fn historical_shard(stream: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stream.hash(&mut hasher);
        hasher.finish() % 256
    }
    let registry = InMemorySmSessionRegistry::new();
    let busy = "busy-detached-stream";
    let other = (0..10_000)
        .map(|index| format!("unrelated-detached-stream-{index}"))
        .find(|candidate| historical_shard(candidate) == historical_shard(busy))
        .expect("historical shard collision");
    let busy_operation = registry.lock_session_operation(busy).await.unwrap();
    let other_operation = tokio::time::timeout(
        Duration::from_millis(100),
        registry.lock_session_operation(&other),
    )
    .await
    .expect("an unrelated stream must progress while the busy stream is locked")
    .unwrap();
    drop(other_operation);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            registry.lock_session_operation(busy),
        )
        .await
        .is_err(),
        "the same stream must still serialize"
    );
    drop(busy_operation);
    registry.lock_session_operation(busy).await.unwrap();
}

#[tokio::test]
async fn detached_fanout_uses_incremental_writes_and_keeps_the_original_restart_deadline() {
    use crate::stream_management::persistence::SmPersistenceStorage;
    let storage = std::sync::Arc::new(FailingSnapshotPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let mut session = realistic_test_session("incremental-deadline");
    session.detached_at = Instant::now() - Duration::from_secs(40);
    session.max_resume_time = Some(60);
    registry.store_session(session).await.unwrap();
    let stream = crate::pending_delivery::SmSessionId::new("incremental-deadline");
    let original = storage.inner.get_session(&stream).await.unwrap().unwrap();
    // Reject any complete replacement after initial detach. Fanout still works.
    storage
        .fail_snapshots
        .store(true, std::sync::atomic::Ordering::SeqCst);
    for id in ["first", "second", "third"] {
        assert!(registry
            .record_outbound_for_detached_stream(
                stream.as_str(),
                &message_stanza_with_id(id.into()),
                Utc::now(),
            )
            .await
            .unwrap());
    }
    assert_eq!(
        storage
            .inner
            .get_session(&stream)
            .await
            .unwrap()
            .unwrap()
            .detached_at,
        original.detached_at
    );
    let restarted = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    assert_eq!(restarted.restore_from_persistence().await.unwrap(), 1);
    let resumed = restarted
        .claim_session(stream.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(
        resumed.remaining_time() <= Duration::from_secs(20),
        "fanout/restart must not grant a fresh max resume window"
    );
    assert_eq!(
        resumed.outbound_count,
        original.outbound_count.wrapping_add(3)
    );
}

#[tokio::test]
async fn cancelled_unkeyed_append_keeps_same_stream_serialized_until_commit() {
    use crate::stream_management::persistence::SmPersistenceStorage;
    let storage = std::sync::Arc::new(GatedSnapshotPersistence::new("cancelled-incremental"));
    let registry =
        std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    registry
        .store_session(realistic_test_session("cancelled-incremental"))
        .await
        .unwrap();
    storage
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let appending = tokio::spawn({
        let registry = registry.clone();
        async move {
            registry
                .record_outbound_for_detached_stream(
                    "cancelled-incremental",
                    &message_stanza_with_id("cancelled".into()),
                    Utc::now(),
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), storage.reached.notified())
        .await
        .unwrap();
    appending.abort();
    assert!(appending.await.unwrap_err().is_cancelled());
    let mut same_stream = Box::pin(registry.lock_session_operation("cancelled-incremental"));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut same_stream)
            .await
            .is_err()
    );
    // Unrelated work remains possible while the cancelled append still commits.
    registry
        .lock_session_operation("unrelated-incremental")
        .await
        .unwrap();
    storage
        .armed
        .store(false, std::sync::atomic::Ordering::SeqCst);
    storage.proceed.notify_one();
    let guard = tokio::time::timeout(Duration::from_secs(5), same_stream)
        .await
        .unwrap()
        .unwrap();
    drop(guard);
    assert!(registry
        .record_outbound_for_detached_stream(
            "cancelled-incremental",
            &message_stanza_with_id("following".into()),
            Utc::now(),
        )
        .await
        .unwrap());
    let stream = crate::pending_delivery::SmSessionId::new("cancelled-incremental");
    assert_eq!(
        storage
            .inner
            .get_session(&stream)
            .await
            .unwrap()
            .unwrap()
            .outbound_count,
        9
    );
    assert_eq!(
        storage
            .inner
            .list_unacked(&stream)
            .await
            .unwrap()
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![6, 7, 8, 9]
    );
}
