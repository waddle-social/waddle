//! Rewrites must not renew the original database-clock resume window.

use super::{
    async_trait, current_claim_epoch, fixed_occupancy_session, fixture, fixture_principal,
    fixture_session, fixture_unacked, full, live_stealer, node_identity, seed_node,
    sm_session_entity, stale_caller_supplied_time, Arc, ClaimEpoch, ClaimError, ClaimStore,
    DateTime, Entity, Fixture, IngressCustodyDisposition, KeyedSnapshotOutcome, NodeIdentity,
    PersistedDetachedAppend, PersistedIngressAppend, PostgresClaimStore,
    PostgresFencedSmPersistence, SmClaimFence, SmPersistenceError, SmPersistenceStorage,
    SmSessionId, StalePredicate, Utc,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration as StdDuration;
use waddle_xmpp::ownership::{ClaimSnapshot, ExactReleaseOutcome, ResumeIdentityProof};
use waddle_xmpp::stream_management::SmSessionRegistry;
use waddle_xmpp::stream_management::{
    DetachedSession, InMemorySmSessionRegistry, PromotionCaptureCustody, SmIngressAppendKey,
    SmIngressReceiptKind, SmKeyedAppendOutcome,
};

/// Only the registry-side detach acquisition fails. The persistence backend
/// already committed its real Postgres claim with the initial snapshot.
struct FailFirstDetachClaim {
    inner: PostgresClaimStore,
    first: AtomicBool,
    timeout_first: bool,
}

#[async_trait]
impl ClaimStore for FailFirstDetachClaim {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        self.inner.ensure_schema().await
    }

    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        self.inner.acquire(entity, me).await
    }

    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        if self.first.swap(false, Ordering::SeqCst) {
            if self.timeout_first {
                return std::future::pending().await;
            }
            return Err(ClaimError::Backend("injected detach claim failure".into()));
        }
        self.inner.ensure_claimed(entity, me).await
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.inner
            .steal_stale(entity, observed, staleness, me)
            .await
    }

    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }

    async fn current_claim(&self, entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.inner.current_claim(entity).await
    }

    async fn current_claim_after_pending_writes(
        &self,
        entity: &Entity,
    ) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.inner.current_claim_after_pending_writes(entity).await
    }

    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        self.inner.fence(entity, me, mine).await
    }

    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        self.inner.release(entity, me, mine).await
    }

    async fn release_exact(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<ExactReleaseOutcome, ClaimError> {
        self.inner.release_exact(entity, me, mine).await
    }

    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

fn fresh_detached_session(stream: &str) -> DetachedSession {
    DetachedSession {
        promotion_custody: PromotionCaptureCustody::never_claimed(),
        stream_id: stream.into(),
        user_id: "alice".into(),
        jid: full("alice@example.com/web"),
        occupancy_session: fixed_occupancy_session(),
        inbound_count: 7,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(60),
        detached_at: std::time::Instant::now(),
        carbons_enabled: true,
        roster_interested: true,
        blocklist_interested: true,
        presence_available: true,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

async fn registry_with_failed_detach_claim(
    f: &Fixture,
    stream: &str,
    timeout_first: bool,
) -> Arc<InMemorySmSessionRegistry> {
    let persistence = PostgresFencedSmPersistence::open(
        f.claims_db.clone(),
        Arc::new(PostgresClaimStore::new(f.claims_db.clone())),
        f.shared_identity.clone(),
    )
    .await
    .expect("open registry persistence");
    let registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(Arc::new(persistence))
            .with_claim_store(
                Arc::new(FailFirstDetachClaim {
                    inner: PostgresClaimStore::new(f.claims_db.clone()),
                    first: AtomicBool::new(true),
                    timeout_first,
                }),
                f.shared_identity.clone(),
            ),
    );
    registry
        .store_session_with_principal(fresh_detached_session(stream), fixture_principal())
        .await
        .expect("best-effort detach must preserve the session");
    assert_eq!(registry.current_sm_claim_fence(stream), None);
    assert!(registry
        .peek_session(stream)
        .await
        .expect("peek session")
        .expect("detached session")
        .promotion_custody
        .captured_fence()
        .is_none());
    registry
}

async fn assert_registry_append_recovers_detach_claim(keyed: bool, timeout_first: bool) {
    let Some(f) = fixture().await else { return };
    let stream = SmSessionId::new("recover-fenceless-detach");
    let registry = registry_with_failed_detach_claim(&f, stream.as_str(), timeout_first).await;
    let initial = f.fenced.get_session(&stream).await.unwrap().unwrap();
    let entity = sm_session_entity(&stream);
    let epoch = current_claim_epoch(&f, &entity).await;
    let entry = fixture_unacked(stream.as_str(), 1);
    let receipt = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let key = SmIngressAppendKey {
        message_key: waddle_xmpp::ingress::MessageKey::new(),
        kind: SmIngressReceiptKind::from_storage(3),
        semantic_identity_hash: [9; 32],
        resource: initial.jid.clone(),
    };
    if keyed {
        assert_eq!(
            registry
                .record_keyed_outbound_for_detached_stream_at(
                    stream.as_str(),
                    1,
                    &entry.stanza,
                    receipt,
                    key.clone(),
                )
                .await
                .expect("keyed append recovers the initial durable claim"),
            SmKeyedAppendOutcome::Appended {
                accepting_stream: stream.clone()
            }
        );
        let proof = f.fenced.get_ingress_append(&key).await.unwrap().unwrap();
        assert_eq!(proof.accepting_stream, stream);
        assert_eq!(proof.sequence, 1);
        assert_eq!(proof.payload.to_element(), entry.stanza.to_element());
        assert_eq!(proof.original_receipt_at, receipt);
        assert_eq!(proof.disposition, IngressCustodyDisposition::Pending);
    } else {
        assert!(registry
            .record_outbound_for_detached_stream(stream.as_str(), &entry.stanza, receipt)
            .await
            .expect("ordinary append recovers the initial durable claim"));
    }
    let stored = f.fenced.get_session(&stream).await.unwrap().unwrap();
    assert_eq!(stored.outbound_count, 1);
    assert_eq!(stored.inbound_count, initial.inbound_count);
    assert_eq!(stored.detached_at, initial.detached_at);
    let queue = f.fenced.list_unacked(&stream).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].sequence, 1);
    assert_eq!(queue[0].stanza.to_element(), entry.stanza.to_element());
    assert_eq!(queue[0].original_receipt_at, receipt);
    let expected = SmClaimFence::new(f.identity.clone(), epoch);
    assert_eq!(
        registry.current_sm_claim_fence(stream.as_str()),
        Some(expected.clone())
    );
    assert_eq!(
        registry
            .peek_session(stream.as_str())
            .await
            .unwrap()
            .unwrap()
            .promotion_custody
            .captured_fence(),
        Some(&expected)
    );
    assert_eq!(current_claim_epoch(&f, &entity).await, epoch);
}

#[tokio::test]
async fn registry_unkeyed_append_recovers_fence_after_detach_claim_failure_postgres() {
    assert_registry_append_recovers_detach_claim(false, false).await;
}

#[tokio::test]
async fn registry_keyed_append_recovers_fence_after_detach_claim_failure_postgres() {
    assert_registry_append_recovers_detach_claim(true, false).await;
}

#[tokio::test]
async fn registry_unkeyed_append_recovers_fence_after_detach_claim_timeout_postgres() {
    assert_registry_append_recovers_detach_claim(false, true).await;
}

#[tokio::test]
async fn registry_keyed_append_recovers_fence_after_detach_claim_timeout_postgres() {
    assert_registry_append_recovers_detach_claim(true, true).await;
}

#[derive(Clone, Copy)]
enum LostDetachAuthority {
    ReleasedClaim,
    ForeignOwner,
    RotatedIdentity,
    DeletedSource,
}

async fn assert_fenceless_append_cannot_recreate_or_adopt_source(keyed: bool) {
    for lost in [
        LostDetachAuthority::ReleasedClaim,
        LostDetachAuthority::ForeignOwner,
        LostDetachAuthority::RotatedIdentity,
        LostDetachAuthority::DeletedSource,
    ] {
        let Some(f) = fixture().await else { return };
        let stream = SmSessionId::new("reject-fenceless-detach");
        let registry = registry_with_failed_detach_claim(&f, stream.as_str(), false).await;
        let entity = sm_session_entity(&stream);
        let epoch = current_claim_epoch(&f, &entity).await;
        let initial = f.fenced.get_session(&stream).await.unwrap().unwrap();
        match lost {
            LostDetachAuthority::ReleasedClaim | LostDetachAuthority::ForeignOwner => {
                f.claims
                    .release_exact(&entity, &f.identity, epoch)
                    .await
                    .unwrap();
                if matches!(lost, LostDetachAuthority::ForeignOwner) {
                    f.claims
                        .ensure_claimed(&entity, &node_identity())
                        .await
                        .unwrap();
                }
            }
            LostDetachAuthority::RotatedIdentity => {
                f.shared_identity.rotate(node_identity()).await;
            }
            LostDetachAuthority::DeletedSource => {
                f.claims_db
                    .guard()
                    .await
                    .unwrap()
                    .execute(
                        "DELETE FROM sm_sessions WHERE stream_id = ?",
                        crate::db_params![stream.as_str().to_owned()],
                    )
                    .await
                    .unwrap();
            }
        }
        let claim_before = f.claims.current_claim(&entity).await.unwrap();
        let entry = fixture_unacked(stream.as_str(), 1);
        let key = SmIngressAppendKey {
            message_key: waddle_xmpp::ingress::MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [10; 32],
            resource: initial.jid.clone(),
        };
        if keyed {
            assert!(
                registry
                    .record_keyed_outbound_for_detached_stream_at(
                        stream.as_str(),
                        1,
                        &entry.stanza,
                        Utc::now(),
                        key.clone(),
                    )
                    .await
                    .is_err(),
                "missing original authority must reject a keyed append"
            );
            assert!(f.fenced.get_ingress_append(&key).await.unwrap().is_none());
        } else {
            assert!(
                registry
                    .record_outbound_for_detached_stream(stream.as_str(), &entry.stanza, Utc::now())
                    .await
                    .is_err(),
                "missing original authority must reject an ordinary append"
            );
        }
        assert_eq!(
            f.claims.current_claim(&entity).await.unwrap(),
            claim_before,
            "append recovery must not create or steal a claim"
        );
        assert!(f.fenced.list_unacked(&stream).await.unwrap().is_empty());
        let after = f.fenced.get_session(&stream).await.unwrap();
        if matches!(lost, LostDetachAuthority::DeletedSource) {
            assert!(
                after.is_none(),
                "append recovery must not recreate a completed source"
            );
        } else {
            let after = after.unwrap();
            assert_eq!(after.outbound_count, initial.outbound_count);
            assert_eq!(after.detached_at, initial.detached_at);
            assert!(registry
                .peek_session(stream.as_str())
                .await
                .unwrap()
                .unwrap()
                .promotion_custody
                .captured_fence()
                .is_none());
        }
    }
}

#[tokio::test]
async fn registry_unkeyed_append_rejects_lost_fenceless_detach_authority_postgres() {
    assert_fenceless_append_cannot_recreate_or_adopt_source(false).await;
}

#[tokio::test]
async fn registry_keyed_append_rejects_lost_fenceless_detach_authority_postgres() {
    assert_fenceless_append_cannot_recreate_or_adopt_source(true).await;
}

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
