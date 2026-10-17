//! A paused server batch cannot outlive its captured ownership incarnation.

use super::*;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;
use waddle_xmpp::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::SmPersistenceStorage;
use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};
use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError, InMemoryBlockingStorage};

struct PauseFirstBlocklist {
    armed: AtomicBool,
    reached: Notify,
    resume: Notify,
}

#[async_trait]
impl BlockingStorage for PauseFirstBlocklist {
    async fn list_blocked_jids(
        &self,
        _user: &BareJid,
    ) -> Result<Vec<BareJid>, BlockingStorageError> {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.reached.notify_one();
            self.resume.notified().await;
        }
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn displaced_batch_cannot_recreate_sources_after_demotion_successor_and_recovery() {
    // Distinct durable databases exercise the portable receipt cleanup that
    // otherwise could hide a duplicate promoted from a stale off-map capture.
    let directory = tempfile::tempdir().expect("portable databases");
    let source_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("source.db").display()
    );
    let pending_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("pending.db").display()
    );
    let source = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(&source_url))
            .await
            .expect("canonical source database"),
    );
    let pending_store = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            Some(&pending_url),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("independent pending database"),
    );
    let pending: Arc<dyn PendingDeliveryStorage> = pending_store.clone();
    let claims = Arc::new(InProcessClaimStore::new());
    let original_owner = NodeIdentity::new("stale-batch", "before-fence");
    let original_identity = SharedNodeIdentity::new(original_owner.clone());
    let original = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(source.clone())
            .with_claim_store(claims.clone(), original_identity.clone()),
    );
    let streams = ["paused-batch-first", "paused-batch-second"];
    let recipients = ["alice@example.com/web", "carol@example.com/web"];
    for (stream, recipient) in streams.iter().zip(recipients) {
        let mut session = detached_session_with_unacked(
            stream,
            full(recipient),
            vec![dm_xml("bob@example.com/web", recipient, stream)],
        );
        session.max_resume_time = Some(0);
        assert!(original
            .store_session(session)
            .await
            .expect("persist source")
            .is_empty());
    }
    let fences: Vec<_> = streams
        .iter()
        .map(|stream| {
            original
                .current_sm_claim_fence(stream)
                .expect("original captured fence")
        })
        .collect();
    let mut captures = original
        .drain_expired()
        .await
        .expect("capture expired batch");
    captures.sort_by(|left, right| left.stream_id.cmp(&right.stream_id));
    assert_eq!(captures.len(), 2);
    let connections = Arc::new(ConnectionRegistry::new());
    let users = test_user_registry();
    let blocking = Arc::new(PauseFirstBlocklist {
        armed: AtomicBool::new(true),
        reached: Notify::new(),
        resume: Notify::new(),
    });
    let task = {
        let original = original.clone();
        let pending = pending.clone();
        let connections = connections.clone();
        let users = users.clone();
        let blocking = blocking.clone();
        tokio::spawn(async move {
            promote_displaced_sessions(
                captures,
                DisplacedPromotionDeps {
                    sm_registry: original.as_ref(),
                    connection_registry: connections.as_ref(),
                    user_registry: &users,
                    pending_storage: &pending,
                    blocking_storage: blocking.as_ref(),
                    server_domain: "example.com",
                },
            )
            .await
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        blocking.reached.notified(),
    )
    .await
    .expect("first source registered; unstarted source remains off-map");
    assert_eq!(
        pending_store
            .count(&bare("alice@example.com"))
            .await
            .expect("before handoff"),
        0
    );

    original_identity.disable().await;
    for (stream, fence) in streams.iter().zip(&fences) {
        assert!(
            original.forget_claim_locally(stream).await,
            "demote local custody"
        );
        claims
            .release(
                &Entity::new(EntityType::SmSession, (*stream).to_owned()),
                &original_owner,
                fence.epoch(),
            )
            .await
            .expect("release exact demoted claim");
    }
    let successor = InMemorySmSessionRegistry::new()
        .with_persistence(source.clone())
        .with_claim_store(
            claims.clone(),
            SharedNodeIdentity::new(NodeIdentity::new("successor", "active")),
        );
    assert_eq!(
        successor
            .restore_from_persistence()
            .await
            .expect("successor hydration"),
        2
    );
    let successor_captures = successor.drain_expired().await.expect("successor batch");
    assert_eq!(successor_captures.len(), 2);
    let outcome = promote_displaced_sessions(
        successor_captures,
        DisplacedPromotionDeps {
            sm_registry: &successor,
            connection_registry: connections.as_ref(),
            user_registry: &users,
            pending_storage: &pending,
            blocking_storage: &InMemoryBlockingStorage::new(),
            server_domain: "example.com",
        },
    )
    .await;
    assert_eq!(outcome.completed_stream_ids().count(), 2);
    assert!(pending_store
        .promotion_receipt_sources_before(i64::MAX, None, 8)
        .await
        .expect("successor receipt cleanup")
        .is_empty());
    for stream in streams {
        assert!(source
            .get_session(&SmSessionId::new(stream))
            .await
            .expect("successor source retirement")
            .is_none());
        assert!(claims
            .current_claim(&Entity::new(EntityType::SmSession, stream.to_owned()))
            .await
            .expect("successor exact release")
            .is_none());
    }

    // Recovery opens fresh admission, but cannot authorize either old capture.
    original_identity
        .rotate(NodeIdentity::new("stale-batch", "recovered-active"))
        .await;
    assert!(original_identity.current().is_active());
    blocking.resume.notify_one();
    let old_outcome = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("old batch terminates")
        .expect("old batch task");
    assert_eq!(old_outcome.completed_stream_ids().count(), 0);
    for recipient in recipients {
        assert_eq!(
            pending_store
                .count(&full(recipient).to_bare())
                .await
                .expect("single successor delivery"),
            1,
            "stale batch must not duplicate the successor handoff"
        );
    }
    for stream in streams {
        assert!(source
            .get_session(&SmSessionId::new(stream))
            .await
            .expect("no source reconstruction")
            .is_none());
        assert!(source
            .list_unacked(&SmSessionId::new(stream))
            .await
            .expect("no replay reconstruction")
            .is_empty());
        assert!(claims
            .current_claim(&Entity::new(EntityType::SmSession, stream.to_owned()))
            .await
            .expect("no stale reacquisition")
            .is_none());
    }
    assert!(original
        .locally_owned_claim_ids()
        .expect("no stale custody or GC marker")
        .is_empty());
    assert!(original
        .drain_expired()
        .await
        .expect("no retained stale retries")
        .is_empty());
    assert!(pending_store
        .promotion_receipt_sources_before(i64::MAX, None, 8)
        .await
        .expect("no stale receipts")
        .is_empty());
}
