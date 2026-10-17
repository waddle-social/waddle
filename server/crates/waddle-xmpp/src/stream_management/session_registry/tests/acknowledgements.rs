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

/// Controls the independent pending commit while the SM header is unreadable.
struct GatedAckPending {
    inner: InMemoryPendingDeliveryStorage,
    fail: std::sync::atomic::AtomicBool,
    gate: std::sync::atomic::AtomicBool,
    commit_before_gate: std::sync::atomic::AtomicBool,
    reached: tokio::sync::Notify,
    proceed: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl PendingDeliveryStorage for GatedAckPending {
    fn quota_policy(&self) -> crate::pending_delivery::QuotaPolicy {
        self.inner.quota_policy()
    }

    async fn notification_recovery_high_water(
        &self,
    ) -> Result<
        Option<crate::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        crate::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.notification_recovery_high_water().await
    }

    async fn list_unoutboxed_archived_after(
        &self,
        after: Option<crate::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        through: Option<crate::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        limit: usize,
    ) -> Result<
        Vec<crate::pending_delivery::storage::PendingNotificationRecoveryRow>,
        crate::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoutboxed_archived_after(after, through, limit)
            .await
    }

    async fn insert(
        &self,
        row: PendingRow,
    ) -> Result<
        crate::pending_delivery::InsertOutcome,
        crate::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.insert(row).await
    }
    async fn list(
        &self,
        recipient: &jid::BareJid,
    ) -> Result<Vec<PendingRow>, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.list(recipient).await
    }
    async fn claim_for_session(
        &self,
        recipient: &jid::BareJid,
        session: &SmSessionId,
    ) -> Result<Vec<PendingRow>, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.claim_for_session(recipient, session).await
    }
    async fn claim_batch_for_session(
        &self,
        recipient: &jid::BareJid,
        session: &SmSessionId,
        after: Option<&PendingRowId>,
        limit: usize,
    ) -> Result<Vec<PendingRow>, crate::pending_delivery::storage::PendingStorageError> {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }
    async fn delete_claimed(
        &self,
        session: &SmSessionId,
    ) -> Result<u64, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }
    async fn delete_row(
        &self,
        id: &PendingRowId,
    ) -> Result<u64, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }
    async fn release_claim(
        &self,
        session: &SmSessionId,
    ) -> Result<u64, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }
    async fn release_row(
        &self,
        id: &PendingRowId,
    ) -> Result<u64, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }
    async fn record_pushed_at(
        &self,
        id: &PendingRowId,
        sequence: u32,
    ) -> Result<u64, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }
    async fn delete_acked_in_window(
        &self,
        session: &SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, crate::pending_delivery::storage::PendingStorageError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(
                crate::pending_delivery::storage::PendingStorageError::Other(
                    "injected pending outage".to_owned(),
                ),
            );
        }
        let commit_first = self.commit_before_gate.load(Ordering::SeqCst);
        let deleted = if commit_first {
            self.inner
                .delete_acked_in_window(session, from_exclusive, to_inclusive)
                .await?
        } else {
            0
        };
        if self.gate.load(Ordering::SeqCst) {
            self.reached.notify_one();
            self.proceed.notified().await;
        }
        if commit_first {
            Ok(deleted)
        } else {
            self.inner
                .delete_acked_in_window(session, from_exclusive, to_inclusive)
                .await
        }
    }
    async fn list_orphaned_claims(
        &self,
        live_sessions: &[SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(PendingRowId, SmSessionId)>,
        crate::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }
    async fn count(
        &self,
        recipient: &jid::BareJid,
    ) -> Result<u32, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }
    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }
    async fn scrub_for_tombstone(
        &self,
        target: &crate::tombstone::TombstoneTarget,
    ) -> Result<u64, crate::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

struct PoisonAckFixture {
    registry: Arc<InMemorySmSessionRegistry>,
    storage: Arc<super::GatedGetSessionPersistence>,
    pending: Arc<GatedAckPending>,
    claims: Arc<InProcessClaimStore>,
    identity: SharedNodeIdentity,
    entity: Entity,
    fence: crate::stream_management::persistence::SmClaimFence,
    reservation: super::super::ReclaimedClaimReservation,
    recipient: jid::BareJid,
}

impl PoisonAckFixture {
    async fn new(stream: &str, corrupt_queue: bool, attach_pending: bool) -> Self {
        let storage = Arc::new(super::GatedGetSessionPersistence::new(stream));
        let initial = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        initial
            .store_session(realistic_test_session(stream))
            .await
            .unwrap();
        let captured = initial.claim_session(stream).await.unwrap().unwrap();
        let pending = Arc::new(GatedAckPending {
            inner: pending_copy(&captured).await,
            fail: std::sync::atomic::AtomicBool::new(false),
            gate: std::sync::atomic::AtomicBool::new(false),
            commit_before_gate: std::sync::atomic::AtomicBool::new(false),
            reached: tokio::sync::Notify::new(),
            proceed: tokio::sync::Notify::new(),
        });
        initial
            .acknowledge_claimed_resume(&captured, 6)
            .await
            .unwrap();
        drop(initial);
        if corrupt_queue {
            storage.corrupt_unacked.store(true, Ordering::SeqCst);
        } else {
            storage.corrupt.store(true, Ordering::SeqCst);
        }
        let claims = Arc::new(InProcessClaimStore::new());
        let identity = SharedNodeIdentity::new(NodeIdentity::new("poison-node", "incarnation-1"));
        let entity = Entity::new(EntityType::SmSession, stream);
        let epoch = claims.acquire(&entity, &identity.current()).await.unwrap();
        let fence =
            crate::stream_management::persistence::SmClaimFence::new(identity.current(), epoch);
        let mut registry = InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claims.clone(), identity.clone());
        if attach_pending {
            registry = registry.with_pending_delivery_storage(pending.clone());
        }
        let reservation = registry.reserve_reclaimed_claim_capacity(&entity).unwrap();
        Self {
            registry: Arc::new(registry),
            storage,
            pending,
            claims,
            identity,
            entity,
            fence,
            reservation,
            recipient: captured.jid.to_bare(),
        }
    }

    async fn hydrate(&self) -> super::super::ReclaimedHydrationOutcome {
        self.registry
            .hydrate_reclaimed_typed(&self.entity, &self.fence, self.reservation)
            .await
            .unwrap()
    }

    async fn assert_retained(&self, pending_count: u32) {
        let stream = SmSessionId::new(&self.entity.id);
        assert!(self
            .storage
            .inner
            .get_session(&stream)
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            self.storage
                .inner
                .pending_acknowledgements(&stream)
                .await
                .unwrap(),
            vec![SmAckWindow {
                from_exclusive: 5,
                to_inclusive: 6
            }]
        );
        assert_eq!(
            self.pending.count(&self.recipient).await.unwrap(),
            pending_count
        );
    }
}

#[tokio::test]
async fn poison_quarantine_settles_ack_without_decoding_header_or_queue() {
    for corrupt_queue in [false, true] {
        let fixture = PoisonAckFixture::new("poison-ack-settlement", corrupt_queue, true).await;
        assert_eq!(
            fixture.hydrate().await,
            super::super::ReclaimedHydrationOutcome::PoisonReleased
        );
        let stream = SmSessionId::new(&fixture.entity.id);
        assert!(fixture
            .storage
            .inner
            .get_session(&stream)
            .await
            .unwrap()
            .is_none());
        assert!(fixture
            .storage
            .inner
            .pending_acknowledgements(&stream)
            .await
            .unwrap()
            .is_empty());
        let rows = fixture.pending.list(&fixture.recipient).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outbound_sequence, Some(7));
    }
}

#[tokio::test]
async fn poison_quarantine_without_pending_dependency_retains_ack_and_claim() {
    let fixture = PoisonAckFixture::new("poison-ack-no-pending", false, false).await;
    assert_eq!(
        fixture.hydrate().await,
        super::super::ReclaimedHydrationOutcome::TransientFailure
    );
    fixture.assert_retained(2).await;
    assert!(fixture
        .claims
        .fence(
            &fixture.entity,
            fixture.fence.owner(),
            fixture.fence.epoch()
        )
        .await
        .unwrap());
    assert_eq!(
        fixture
            .registry
            .pending_reclaimed_hydrations
            .read()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn poison_quarantine_retries_pending_failure_without_releasing_custody() {
    let fixture = PoisonAckFixture::new("poison-ack-pending-retry", true, true).await;
    fixture.pending.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture.hydrate().await,
        super::super::ReclaimedHydrationOutcome::TransientFailure
    );
    fixture.assert_retained(2).await;
    fixture.pending.fail.store(false, Ordering::SeqCst);
    assert_eq!(
        fixture.hydrate().await,
        super::super::ReclaimedHydrationOutcome::PoisonReleased
    );
    assert_eq!(fixture.pending.count(&fixture.recipient).await.unwrap(), 1);
}

#[tokio::test]
async fn cancelled_poison_ack_settlement_retries_before_or_after_pending_commit() {
    for committed in [false, true] {
        let fixture = Arc::new(PoisonAckFixture::new("poison-ack-cancel", false, true).await);
        fixture
            .pending
            .commit_before_gate
            .store(committed, Ordering::SeqCst);
        fixture.pending.gate.store(true, Ordering::SeqCst);
        let task_fixture = fixture.clone();
        let task = tokio::spawn(async move { task_fixture.hydrate().await });
        fixture.pending.reached.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        fixture.assert_retained(if committed { 1 } else { 2 }).await;
        assert_eq!(
            fixture
                .registry
                .pending_reclaimed_hydrations
                .read()
                .unwrap()
                .len(),
            1
        );
        fixture.pending.gate.store(false, Ordering::SeqCst);
        assert_eq!(
            fixture.hydrate().await,
            super::super::ReclaimedHydrationOutcome::PoisonReleased
        );
        assert_eq!(fixture.pending.count(&fixture.recipient).await.unwrap(), 1);
    }
}

#[tokio::test]
async fn poison_ack_settlement_rechecks_epoch_and_identity_after_pending_await() {
    for rotate_identity in [false, true] {
        let fixture = Arc::new(PoisonAckFixture::new("poison-ack-stale", false, true).await);
        fixture
            .pending
            .commit_before_gate
            .store(true, Ordering::SeqCst);
        fixture.pending.gate.store(true, Ordering::SeqCst);
        let task_fixture = fixture.clone();
        let task = tokio::spawn(async move { task_fixture.hydrate().await });
        fixture.pending.reached.notified().await;
        let next = NodeIdentity::new("poison-node", "incarnation-2");
        if rotate_identity {
            // This must finish while pending I/O is paused: no identity guard
            // may cross that backend's await or self-fencing would deadlock.
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                fixture.identity.rotate(next),
            )
            .await
            .unwrap();
        } else {
            fixture
                .claims
                .release(
                    &fixture.entity,
                    fixture.fence.owner(),
                    fixture.fence.epoch(),
                )
                .await
                .unwrap();
            fixture
                .claims
                .acquire(&fixture.entity, &next)
                .await
                .unwrap();
        }
        fixture.pending.proceed.notify_one();
        assert_eq!(
            task.await.unwrap(),
            if rotate_identity {
                super::super::ReclaimedHydrationOutcome::StaleIdentity
            } else {
                super::super::ReclaimedHydrationOutcome::LostClaim
            }
        );
        fixture.assert_retained(1).await;
    }
}
