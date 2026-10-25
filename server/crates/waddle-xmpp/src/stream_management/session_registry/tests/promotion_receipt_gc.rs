use super::{make_test_session, GatedGetSessionPersistence, InMemorySmSessionRegistry};
use crate::pending_delivery::storage::InMemoryPendingDeliveryStorage;
use crate::pending_delivery::{QuotaPolicy, SmSessionId};
use crate::stream_management::persistence::{InMemorySmPersistence, SmPersistenceStorage};

#[tokio::test]
async fn orphan_receipts_require_proven_durable_source_absence() {
    let stream = SmSessionId::new("orphan-receipt-source");
    let pending = InMemoryPendingDeliveryStorage::new(QuotaPolicy::Unlimited);
    let memory_only = InMemorySmSessionRegistry::new();
    assert!(!memory_only
        .retire_orphaned_promotion_receipts(&stream, &pending)
        .await
        .expect("unknown source retained"));
    let persistence = std::sync::Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(persistence.clone());
    let session = make_test_session(stream.as_str());
    persistence
        .upsert_session(
            super::super::persistence_codec::detached_to_persisted(&session)
                .expect("persisted metadata"),
        )
        .await
        .expect("canonical source");
    assert!(!registry
        .retire_orphaned_promotion_receipts(&stream, &pending)
        .await
        .expect("live source retained"));
    persistence
        .delete_session(&stream)
        .await
        .expect("retire source");
    assert!(registry
        .retire_orphaned_promotion_receipts(&stream, &pending)
        .await
        .expect("orphan receipts retired"));
}

#[tokio::test]
async fn orphan_receipts_preserve_off_map_retry_custody() {
    let stream = SmSessionId::new("off-map-receipt-source");
    let pending = InMemoryPendingDeliveryStorage::new(QuotaPolicy::Unlimited);
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(std::sync::Arc::new(InMemorySmPersistence::new()));
    registry
        .pending_promotion_retries
        .write()
        .expect("retry map")
        .insert(
            stream.as_str().to_owned(),
            make_test_session(stream.as_str()),
        );
    assert!(!registry
        .retire_orphaned_promotion_receipts(&stream, &pending)
        .await
        .expect("retry custody retained"));
}

#[tokio::test]
async fn orphan_receipts_preserve_storage_errors_and_corrupt_sources() {
    let stream = SmSessionId::new("uncertain-receipt-source");
    let pending = InMemoryPendingDeliveryStorage::new(QuotaPolicy::Unlimited);
    let persistence = std::sync::Arc::new(GatedGetSessionPersistence::new(stream.as_str()));
    let registry = InMemorySmSessionRegistry::new().with_persistence(persistence.clone());
    persistence
        .fail_get_once
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(registry
        .retire_orphaned_promotion_receipts(&stream, &pending)
        .await
        .is_err());
    persistence
        .corrupt
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(registry
        .retire_orphaned_promotion_receipts(&stream, &pending)
        .await
        .is_err());
}

#[tokio::test]
async fn orphan_receipts_recheck_custody_published_during_source_read() {
    let stream = SmSessionId::new("receipt-source-read-race");
    let pending = std::sync::Arc::new(InMemoryPendingDeliveryStorage::new(QuotaPolicy::Unlimited));
    let persistence = std::sync::Arc::new(GatedGetSessionPersistence::new(stream.as_str()));
    persistence
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let registry =
        std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
    let check = {
        let registry = registry.clone();
        let pending = pending.clone();
        let stream = stream.clone();
        tokio::spawn(async move {
            registry
                .retire_orphaned_promotion_receipts(&stream, pending.as_ref())
                .await
        })
    };
    persistence.reached.notified().await;
    let session = make_test_session(stream.as_str());
    registry
        .pending_promotions
        .write()
        .expect("pending map")
        .insert(
            stream.as_str().to_owned(),
            (session.jid, session.occupancy_session),
        );
    persistence.proceed.notify_one();
    assert!(!check
        .await
        .expect("check task")
        .expect("new custody retained"));
}
