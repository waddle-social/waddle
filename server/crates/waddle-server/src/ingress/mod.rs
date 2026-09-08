//! Durable ingress authority: immutable planning, atomic commit, bounded execution.
mod capture;
#[cfg(test)]
pub(crate) use capture::TEST_CAPTURE_LIMIT;
pub use capture::{IngressEffectCapture, IngressEffectCaptureSnapshot};
pub mod commit;
mod commit_room;
mod commit_stream;
pub mod decision;
mod durable;
pub(crate) use durable::receipt_key;
pub mod execute;
mod execute_uow;
mod frame_receipt_retry;
pub(crate) mod gc;
pub mod identity;
pub(crate) mod maintenance;
mod receipts;
mod recorded;
pub use recorded::RouteProgress;
mod rejection;
pub mod restamp;
pub mod submission;
pub mod suppression;

pub use crate::server::routes::interpret::{effects, Deps};
pub use decision::{AliasOutcomeClass, EffectReceiptKey, IngressDecision, IngressDecisionClass};
pub use effects::{
    DurableEffect, ExternalEffect, ImmediateSink, IngressPlan, PlanSuppressionPolicy,
    PlannedEffect, RoomExecutionPath,
};
pub use execute::{ExecutionReport, ExternalOutcome, FrameObligation};
pub use identity::{IngressCanonicalRef, IngressStreamIdentity};
pub use submission::IngressSubmission;

use crate::db::DatabaseDriver;
use crate::{
    config::{IngressConfig, IngressConfigError, LineageConfig},
    db::{Database, DatabaseConfig, DatabaseError},
    ingress_uow::{
        IngressUnitOfWork, IngressUowError, SmIngressRepository, SmIngressStreamRepository,
    },
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, Weak},
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::SharedNodeIdentity;
use waddle_xmpp::{
    ingress::{SmIngressId, WireHandledCount},
    pending_delivery::SmSessionId,
};

#[derive(Debug, thiserror::Error)]
pub enum IngressStartupError {
    #[error(transparent)]
    Config(#[from] IngressConfigError),
    #[error("clustered ingress requires the canonical node identity")]
    NodeIdentityMissing,
    #[error("failed to open the dedicated ingress pool")]
    Pool(#[source] DatabaseError),
    #[error(transparent)]
    UnitOfWork(#[from] IngressUowError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressRetirementOutcome {
    Deleted,
    DeferredClaim,
    StreamMissing,
}

const RETIREMENT_BATCH_SIZE: u32 = 256;

/// Boot-owned handle. Shutdown blocks new work and joins admitted operations and GC.
pub struct IngressAuthority {
    database: Database,
    uow: IngressUnitOfWork,
    config: IngressConfig,
    gc: gc::RetentionGcCoordinator,
    cancellation: CancellationToken,
    force_stop: CancellationToken,
    gc_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    admission: RwLock<bool>,
    streams: StdMutex<HashMap<SmSessionId, Weak<RwLock<()>>>>,
    retirement_cursor: Mutex<Option<SmSessionId>>,
    frame_receipt_retries: StdMutex<frame_receipt_retry::FrameReceiptRetries>,
    /// Test-only: fires when a commit is about to wait for its stream lock.
    #[cfg(test)]
    stream_wait_observer: StdMutex<Option<Arc<tokio::sync::Notify>>>,
    #[cfg(test)]
    retirement_batch_gate: StdMutex<Option<Arc<tokio::sync::Notify>>>,
}

impl IngressAuthority {
    pub async fn new(
        config: IngressConfig,
        database: Database,
        lineage: LineageConfig,
        #[cfg(feature = "clustering")] node_identity: Option<SharedNodeIdentity>,
    ) -> Result<Self, IngressStartupError> {
        config.validate()?;
        // SQLite is a single-writer database: the authority shares the global
        // handle (which also keeps a private in-memory database reachable).
        // PostgreSQL gets a dedicated, bounded pool so ingress transactions
        // never starve or are starved by the general-purpose pool.
        let database = match database.driver() {
            DatabaseDriver::Sqlite => database,
            DatabaseDriver::Postgres => {
                let mut pool_config =
                    DatabaseConfig::new(database.driver(), database.database_url());
                pool_config.pool_size = config.pool_size;
                Database::from_config("ingress", &pool_config)
                    .await
                    .map_err(IngressStartupError::Pool)?
            }
        };
        // A canonical node identity is present exactly when clustering is
        // enabled: claim fences are then asserted inside every transaction.
        // Single-node deployments (SQLite, or PostgreSQL without clustering)
        // have no claims to assert and run with single-node fencing.
        #[cfg(feature = "clustering")]
        let uow = match node_identity {
            Some(node_identity) => IngressUnitOfWork::open_with_node_identity(
                database.clone(),
                lineage,
                node_identity,
            )?,
            None => IngressUnitOfWork::open(database.clone(), lineage)?,
        };
        #[cfg(not(feature = "clustering"))]
        let uow = IngressUnitOfWork::open(database.clone(), lineage)?;
        // Probe the epoch and lineage before publishing a usable authority. A
        // lineage attestation failure is not fatal at boot: the readiness gate
        // holds the node unready and every ingress transaction keeps failing
        // closed with a typed lineage error until the operator resolves it.
        match uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await
        {
            Ok(transaction) => transaction.commit().await?,
            Err(IngressUowError::Lineage(error)) => {
                tracing::warn!(
                    %error,
                    "ingress authority boots unattested; transactions fail closed until lineage verifies"
                );
            }
            Err(error) => return Err(error.into()),
        }
        let gc = gc::RetentionGcCoordinator::new(database.clone(), uow.clone());
        let cancellation = CancellationToken::new();
        let force_stop = CancellationToken::new();
        let gc_task = tokio::spawn(gc::run_retention_gc_coordinator(
            gc.clone(),
            cancellation.clone(),
            force_stop.clone(),
        ));
        Ok(Self {
            database,
            uow,
            config,
            gc,
            cancellation,
            force_stop,
            gc_task: Mutex::new(Some(gc_task)),
            admission: RwLock::new(true),
            streams: StdMutex::new(HashMap::new()),
            #[cfg(test)]
            stream_wait_observer: StdMutex::new(None),
            #[cfg(test)]
            retirement_batch_gate: StdMutex::new(None),
            retirement_cursor: Mutex::new(None),
            frame_receipt_retries: StdMutex::new(
                frame_receipt_retry::FrameReceiptRetries::default(),
            ),
        })
    }

    /// Real database-backed fixture, enrolled through the production lineage repository.
    /// Fixtures share a fixed deployment UUID so reusing a test pool re-attests its row.
    #[cfg(test)]
    pub(crate) async fn for_test(database: Database) -> Self {
        let lineage = test_lineage_config();
        crate::db::lineage::enroll(&database, &lineage)
            .await
            .expect("enroll test ingress lineage");
        let uow = IngressUnitOfWork::open(database.clone(), lineage)
            .expect("open test ingress unit of work");
        Self {
            gc: gc::RetentionGcCoordinator::new(database.clone(), uow.clone()),
            database,
            uow,
            config: IngressConfig::default(),
            cancellation: CancellationToken::new(),
            force_stop: CancellationToken::new(),
            gc_task: Mutex::new(None),
            admission: RwLock::new(true),
            streams: StdMutex::new(HashMap::new()),
            #[cfg(test)]
            stream_wait_observer: StdMutex::new(None),
            #[cfg(test)]
            retirement_batch_gate: StdMutex::new(None),
            retirement_cursor: Mutex::new(None),
            frame_receipt_retries: StdMutex::new(
                frame_receipt_retry::FrameReceiptRetries::default(),
            ),
        }
    }

    fn stream_activity(&self, stream_id: &SmSessionId) -> Arc<RwLock<()>> {
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        streams.retain(|_, activity| activity.strong_count() > 0);
        if let Some(activity) = streams.get(stream_id).and_then(Weak::upgrade) {
            return activity;
        }
        let activity = Arc::new(RwLock::new(()));
        streams.insert(stream_id.clone(), Arc::downgrade(&activity));
        activity
    }

    #[cfg(test)]
    pub(crate) async fn hold_test_commit(
        &self,
        stream_id: &SmSessionId,
    ) -> (
        tokio::sync::RwLockReadGuard<'_, bool>,
        tokio::sync::OwnedRwLockReadGuard<()>,
    ) {
        (
            self.admission.read().await,
            self.stream_activity(stream_id).read_owned().await,
        )
    }

    #[cfg(test)]
    pub(crate) async fn block_test_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.stream_activity(stream_id).write_owned().await
    }

    /// Test-only: returns a notifier that fires when a commit starts waiting
    /// for a stream lock, so tests pause time only once the wait is real.
    #[cfg(test)]
    pub(crate) fn observe_stream_wait(&self) -> Arc<tokio::sync::Notify> {
        let observer = Arc::new(tokio::sync::Notify::new());
        *self
            .stream_wait_observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observer.clone());
        observer
    }

    pub async fn wait_for_stream_idle(&self, stream_id: &SmSessionId, budget: Duration) -> bool {
        tokio::time::timeout(budget, self.stream_activity(stream_id).write_owned())
            .await
            .is_ok()
    }

    pub async fn enroll_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<SmIngressId, IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let _stream_guard = self.stream_activity(stream_id).write_owned().await;
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        let id = SmIngressStreamRepository::mint(&mut transaction, stream_id).await?;
        transaction.commit().await?;
        Ok(id)
    }

    pub async fn lookup_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<SmIngressId>, IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        let id = SmIngressStreamRepository::lookup_unclaimed(&mut transaction, stream_id).await?;
        transaction.commit().await?;
        Ok(id)
    }

    /// Rotate through durable retirement candidates without retaining a work queue.
    pub async fn next_retirement_candidates(&self) -> Result<Vec<SmSessionId>, IngressUowError> {
        const PAGE_SIZE: u32 = 64;
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut cursor = self.retirement_cursor.lock().await;
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        let mut streams = SmIngressStreamRepository::retirement_candidates(
            &mut transaction,
            cursor.as_ref(),
            PAGE_SIZE,
        )
        .await?;
        if streams.is_empty() && cursor.is_some() {
            streams =
                SmIngressStreamRepository::retirement_candidates(&mut transaction, None, PAGE_SIZE)
                    .await?;
        }
        transaction.commit().await?;
        if streams.is_empty() {
            *cursor = None;
        }
        Ok(streams)
    }

    /// Advance immediately before attempting a candidate, never past an unattempted row.
    /// A cancelled attempt remains durable and will be retried after the cursor wraps.
    pub async fn mark_retirement_candidate_attempted(&self, stream: &SmSessionId) {
        *self.retirement_cursor.lock().await = Some(stream.clone());
    }

    pub async fn forget_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<IngressRetirementOutcome, IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let _stream_guard = self.stream_activity(stream_id).write_owned().await;
        loop {
            let mut transaction = self
                .uow
                .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
                .await?;
            #[cfg(feature = "clustering")]
            if self.database.driver() == DatabaseDriver::Postgres
                && !SmIngressStreamRepository::fence_claim_absence_for_retirement(
                    &mut transaction,
                    stream_id,
                )
                .await?
            {
                transaction.commit().await?;
                return Ok(IngressRetirementOutcome::DeferredClaim);
            }
            let Some(id) =
                SmIngressStreamRepository::lookup_unclaimed(&mut transaction, stream_id).await?
            else {
                transaction.commit().await?;
                return Ok(IngressRetirementOutcome::StreamMissing);
            };
            let refs = SmIngressRepository::refs_for_stream_batch(
                &mut transaction,
                id,
                RETIREMENT_BATCH_SIZE,
            )
            .await?;
            // The stream is locked, so a short page contains all remaining refs.
            let finished = refs.len() < RETIREMENT_BATCH_SIZE as usize;
            let mut keys: Vec<_> = refs.iter().map(|(_, key)| *key).collect();
            // Different streams can reference the same canonical keys in
            // opposite ordinal order. Lock each bounded page in UUID order.
            keys.sort_unstable_by_key(waddle_xmpp::ingress::MessageKey::to_storage);
            keys.dedup();
            for key in keys {
                execute::terminalize_if_complete_in_transaction(&mut transaction, key).await?;
            }
            for (ordinal, _) in refs {
                SmIngressRepository::delete_stream_ref(&mut transaction, id, ordinal).await?;
            }
            if finished {
                SmIngressStreamRepository::delete_unclaimed(&mut transaction, stream_id).await?;
            }
            transaction.commit().await?;
            self.gc.trigger();
            if finished {
                return Ok(IngressRetirementOutcome::Deleted);
            }
            #[cfg(test)]
            self.wait_after_retirement_batch().await;
        }
    }

    #[cfg(test)]
    async fn wait_after_retirement_batch(&self) {
        let gate = self
            .retirement_batch_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(gate) = gate {
            gate.notify_one();
            std::future::pending::<()>().await;
        }
    }

    pub async fn commit(&self, submission: &IngressSubmission) -> IngressDecision {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return non_advancing(IngressDecisionClass::Storage);
        }
        let _stream_guard = match &submission.identity {
            IngressStreamIdentity::Resumable { stream_id, .. } => {
                let activity = self.stream_activity(stream_id);
                #[cfg(test)]
                if let Some(observer) = self
                    .stream_wait_observer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                {
                    observer.notify_one();
                }
                Some(activity.read_owned().await)
            }
            _ => None,
        };
        match commit::commit_submission(&self.uow, submission, self.config.retry_attempts).await {
            Ok(decision) => {
                self.gc.trigger();
                decision
            }
            Err(failure) => non_advancing(failure.class()),
        }
    }

    pub async fn execute(
        &self,
        decision: &IngressDecision,
        sink: &ImmediateSink,
        deps: &Deps<'_>,
    ) -> ExecutionReport {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return ExecutionReport::default();
        }
        execute::execute_effects(
            &self.uow,
            &self.database,
            decision,
            sink,
            deps,
            Duration::from_secs(5),
        )
        .await
    }

    /// Confirm all report frames only after the transport has successfully written them.
    pub async fn complete_frame_obligations(
        &self,
        report: &mut ExecutionReport,
    ) -> Result<bool, execute::ExecutionPersistenceFailure> {
        tokio::time::timeout(Duration::from_secs(5), async {
            // Nested local-owner replies share this authority. Complete them before
            // taking the admission read lock: a queued drain writer must never
            // cause recursive read acquisition to deadlock.
            #[cfg(feature = "clustering")]
            report.complete_relay_frame_obligations().await?;
            let admission = self.admission.read().await;
            if self.cancellation.is_cancelled() || !*admission {
                return Err(IngressUowError::AuthorityStopped.into());
            }
            report
                .complete_frame_obligations(&self.uow, &self.database, Duration::from_secs(5))
                .await
        })
        .await
        .map_err(|_| execute::ExecutionPersistenceFailure::BudgetExhausted)?
    }

    pub async fn flush_checkpoint(
        &self,
        stream: SmIngressId,
        h: WireHandledCount,
    ) -> Result<(), IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        SmIngressStreamRepository::flush_checkpoint(&mut transaction, stream, h).await?;
        transaction.commit().await
    }

    pub async fn load_resume_checkpoint(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<WireHandledCount>, IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        let checkpoint =
            match SmIngressStreamRepository::lookup_unclaimed(&mut transaction, stream_id).await? {
                Some(id) => {
                    SmIngressStreamRepository::load_stream_checkpoint(&mut transaction, id).await?
                }
                None => None,
            };
        transaction.commit().await?;
        Ok(checkpoint)
    }

    pub async fn drain_and_join(&self, budget: Duration) -> bool {
        self.cancellation.cancel();
        let drained = tokio::time::timeout(budget, async {
            let receipt_task = self
                .frame_receipt_retries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .task
                .take();
            if let Some(task) = receipt_task {
                if task.await.is_err() {
                    return false;
                }
            }
            *self.admission.write().await = false;
            let mut task = self.gc_task.lock().await;
            if let Some(handle) = task.as_mut() {
                if handle.await.is_err() {
                    return false;
                }
            }
            task.take();
            true
        })
        .await;
        if let Ok(result) = drained {
            return result;
        }
        self.force_stop.cancel();
        false
    }
}

impl Drop for IngressAuthority {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.force_stop.cancel();
    }
}

fn non_advancing(class: IngressDecisionClass) -> IngressDecision {
    IngressDecision {
        class,
        message_key: None,
        ordinal: None,
        alias: AliasOutcomeClass::NoOrigin,
        verdict: None,
        archive_ids: Vec::new(),
        applied_durable: Default::default(),
        external_dependencies: Vec::new(),
        external: Vec::new(),
        external_receipts: Vec::new(),
        arm_owned_receipts: Vec::new(),
        route_progress: Vec::new(),
        receipts_pending: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) fn test_lineage_config() -> LineageConfig {
    LineageConfig {
        deployment_uuid: Some(
            "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001"
                .parse()
                .expect("fixture deployment UUID"),
        ),
        action: None,
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    async fn authority() -> IngressAuthority {
        let database = Database::in_memory("ingress-lifecycle")
            .await
            .expect("open test database");
        IngressAuthority::for_test(database).await
    }

    #[tokio::test]
    async fn stream_idle_waits_for_commit_and_releases_on_cancellation() {
        let authority = authority().await;
        let stream = SmSessionId::new("pending-commit");
        let held = authority.hold_test_commit(&stream).await;
        assert!(
            !authority
                .wait_for_stream_idle(&stream, Duration::from_millis(1))
                .await
        );
        assert!(
            authority
                .wait_for_stream_idle(&SmSessionId::new("other"), Duration::from_millis(10))
                .await
        );
        drop(held);
        assert!(
            authority
                .wait_for_stream_idle(&stream, Duration::from_millis(10))
                .await
        );
    }

    async fn seed_retirement_message(
        authority: &IngressAuthority,
        stream: SmIngressId,
        position: u32,
        receipt_complete: bool,
    ) -> waddle_xmpp::ingress::MessageKey {
        use crate::ingress_uow::{
            CanonicalMessageRepository, EffectIntentRepository, EffectReceiptRepository,
        };
        use waddle_xmpp::ingress::{
            IngressEffectIntent, IngressOrdinal, MessageKey, SemanticDigest,
        };
        let key = MessageKey::new();
        let intent = IngressEffectIntent::RouteOccupantPm {
            recipient: "juliet@example.com/phone".parse().expect("recipient"),
            sender: "romeo@example.com/phone".parse().expect("sender"),
        };
        let mut transaction = authority
            .uow
            .begin()
            .await
            .expect("seed message transaction");
        CanonicalMessageRepository::record_message(
            &mut transaction,
            key,
            &SemanticDigest::from_storage(1, [1; 32]).expect("digest"),
            None,
        )
        .await
        .expect("record canonical message");
        EffectIntentRepository::reconcile(
            &mut transaction,
            key,
            std::slice::from_ref(&intent),
            false,
        )
        .await
        .expect("record pending intent");
        if receipt_complete {
            let receipt = super::durable::receipt_key(&intent).expect("receipt identity");
            EffectReceiptRepository::record_receipt(
                &mut transaction,
                key,
                receipt.kind,
                &receipt.semantic_identity_hash,
            )
            .await
            .expect("record completed effect");
        }
        SmIngressRepository::insert_sm_ref(
            &mut transaction,
            stream,
            IngressOrdinal::from_storage(u64::from(position)).expect("ordinal"),
            WireHandledCount::from_storage(position),
            key,
        )
        .await
        .expect("record stream reference");
        transaction
            .commit()
            .await
            .expect("commit retirement fixture");
        key
    }

    async fn assert_retired_message(
        authority: &IngressAuthority,
        key: waddle_xmpp::ingress::MessageKey,
        terminal: bool,
    ) {
        let guard = authority.database.guard().await.expect("database guard");
        let mut rows = guard.query(
            "SELECT terminal_at IS NOT NULL FROM ingress_messages WHERE CAST(message_key AS TEXT) = ?",
            crate::db_params![key.to_storage().to_string()],
        ).await.expect("read retired canonical message");
        assert_eq!(
            rows.next()
                .await
                .expect("row read")
                .expect("canonical message retained")
                .get::<bool>(0)
                .expect("terminal state"),
            terminal
        );
    }

    async fn enrollment_checkpoint_and_retirement_round_trip(authority: &IngressAuthority) {
        let stream = SmSessionId::new("enrolled-stream");
        let id = authority.enroll_stream(&stream).await.expect("enroll");
        assert_eq!(
            authority
                .enroll_stream(&stream)
                .await
                .expect("enroll twice"),
            id
        );
        assert_eq!(
            authority.lookup_stream(&stream).await.expect("lookup"),
            Some(id)
        );
        assert_eq!(
            authority
                .next_retirement_candidates()
                .await
                .expect("candidate page"),
            vec![stream.clone()]
        );
        authority.mark_retirement_candidate_attempted(&stream).await;
        assert_eq!(
            authority
                .next_retirement_candidates()
                .await
                .expect("candidate cursor wraps"),
            vec![stream.clone()]
        );
        authority
            .flush_checkpoint(id, WireHandledCount::from_storage(3))
            .await
            .expect("flush checkpoint");
        assert_eq!(
            authority
                .load_resume_checkpoint(&stream)
                .await
                .expect("checkpoint"),
            Some(WireHandledCount::from_storage(3))
        );
        #[cfg(feature = "clustering")]
        if authority.database.driver() == DatabaseDriver::Postgres {
            authority.database.execute("INSERT INTO clustering_claims (entity, entity_type) VALUES ('sm_session:enrolled-stream', 'sm_session')").await.expect("retain promotion claim");
            assert_eq!(
                authority
                    .forget_stream(&stream)
                    .await
                    .expect("retirement while claimed"),
                IngressRetirementOutcome::DeferredClaim
            );
            assert_eq!(
                authority
                    .lookup_stream(&stream)
                    .await
                    .expect("retained ingress stream"),
                Some(id)
            );
            authority
                .database
                .execute(
                    "DELETE FROM clustering_claims WHERE entity = 'sm_session:enrolled-stream'",
                )
                .await
                .expect("confirm promotion and release claim");
        }
        let pending = seed_retirement_message(authority, id, 1, false).await;
        let complete = seed_retirement_message(authority, id, 2, true).await;
        assert_eq!(
            authority.forget_stream(&stream).await.expect("retire"),
            IngressRetirementOutcome::Deleted
        );
        assert_retired_message(authority, pending, false).await;
        assert_retired_message(authority, complete, true).await;
        let mut transaction = authority.uow.begin().await.expect("read retired refs");
        assert!(SmIngressRepository::refs_for_stream_batch(
            &mut transaction,
            id,
            RETIREMENT_BATCH_SIZE
        )
        .await
        .expect("retired refs")
        .is_empty());
        transaction.commit().await.expect("close retired refs read");
        assert_eq!(
            authority
                .lookup_stream(&stream)
                .await
                .expect("lookup retired"),
            None
        );
        assert_eq!(
            authority
                .forget_stream(&stream)
                .await
                .expect("retire absent"),
            IngressRetirementOutcome::StreamMissing
        );
    }

    async fn retirement_preserves_committed_batches(authority: &IngressAuthority) {
        let stream = SmSessionId::new("batched-retirement");
        let id = authority.enroll_stream(&stream).await.expect("enroll");
        let mut keys = Vec::new();
        for position in 1..=RETIREMENT_BATCH_SIZE * 2 + 3 {
            keys.push(seed_retirement_message(authority, id, position, true).await);
        }
        // A same-origin retry can bind another position to the same canonical
        // key. Its reference must survive until its own bounded page is retired.
        let position = RETIREMENT_BATCH_SIZE * 2 + 4;
        let mut transaction = authority
            .uow
            .begin()
            .await
            .expect("duplicate ref transaction");
        SmIngressRepository::insert_sm_ref(
            &mut transaction,
            id,
            waddle_xmpp::ingress::IngressOrdinal::from_storage(u64::from(position))
                .expect("duplicate ordinal"),
            WireHandledCount::from_storage(position),
            keys[0],
        )
        .await
        .expect("duplicate canonical ref");
        transaction.commit().await.expect("commit duplicate ref");
        let mut transaction = authority.uow.begin().await.expect("inspect first page");
        let first_page =
            SmIngressRepository::refs_for_stream_batch(&mut transaction, id, RETIREMENT_BATCH_SIZE)
                .await
                .expect("first page");
        assert!(first_page.iter().any(|(_, key)| *key == keys[0]));
        assert!(first_page
            .iter()
            .all(|(ordinal, _)| ordinal.to_storage() != u64::from(position)));
        transaction.commit().await.expect("close first page read");
        // Pause after the first transaction committed so the deadline cancels
        // the actual multi-batch operation at a deterministic boundary.
        let gate = Arc::new(tokio::sync::Notify::new());
        *authority
            .retirement_batch_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(gate.clone());
        {
            let retirement = authority.forget_stream(&stream);
            tokio::pin!(retirement);
            tokio::select! {
                outcome = &mut retirement => panic!("retirement passed the batch gate: {outcome:?}"),
                () = gate.notified() => {},
            }
            assert!(tokio::time::timeout(Duration::from_millis(1), retirement)
                .await
                .is_err());
        }
        *authority
            .retirement_batch_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        assert_eq!(
            authority
                .lookup_stream(&stream)
                .await
                .expect("retained stream"),
            Some(id)
        );
        let mut transaction = authority.uow.begin().await.expect("inspect progress");
        let remaining = SmIngressRepository::refs_for_stream_batch(
            &mut transaction,
            id,
            RETIREMENT_BATCH_SIZE * 3,
        )
        .await
        .expect("remaining refs");
        assert_eq!(remaining.len(), RETIREMENT_BATCH_SIZE as usize + 4);
        assert!(
            remaining.iter().any(
                |(ordinal, key)| ordinal.to_storage() == u64::from(position) && *key == keys[0]
            ),
            "duplicate reference outside the selected page must survive"
        );
        assert!(remaining.iter().all(|entry| !first_page.contains(entry)));
        transaction.commit().await.expect("close progress read");
        for key in &keys {
            assert_retired_message(
                authority,
                *key,
                first_page
                    .iter()
                    .any(|(_, selected_key)| selected_key == key),
            )
            .await;
        }
        assert_eq!(
            authority
                .forget_stream(&stream)
                .await
                .expect("resume retirement"),
            IngressRetirementOutcome::Deleted
        );
        assert_eq!(
            authority
                .lookup_stream(&stream)
                .await
                .expect("deleted stream"),
            None
        );
        let mut transaction = authority.uow.begin().await.expect("inspect final refs");
        assert!(SmIngressRepository::refs_for_stream_batch(
            &mut transaction,
            id,
            RETIREMENT_BATCH_SIZE,
        )
        .await
        .expect("final refs")
        .is_empty());
        transaction.commit().await.expect("close final read");
        for key in keys {
            assert_retired_message(authority, key, true).await;
        }
    }

    #[tokio::test]
    async fn sqlite_retirement_preserves_committed_batches_after_cancellation() {
        let authority = authority().await;
        crate::db::MigrationRunner::global()
            .run(&authority.database)
            .await
            .expect("migrate ingress");
        retirement_preserves_committed_batches(&authority).await;
    }

    #[tokio::test]
    async fn retirement_scan_pages_past_live_streams_and_wraps() {
        let authority = authority().await;
        crate::db::MigrationRunner::global()
            .run(&authority.database)
            .await
            .expect("migrate");
        for index in 0..65 {
            authority
                .enroll_stream(&SmSessionId::new(format!("stream-{index:03}")))
                .await
                .expect("enroll");
        }
        let first = authority
            .next_retirement_candidates()
            .await
            .expect("first page");
        assert_eq!(first.len(), 64);
        authority
            .mark_retirement_candidate_attempted(first.last().expect("first page tail"))
            .await;

        assert_eq!(
            authority
                .next_retirement_candidates()
                .await
                .expect("tail page"),
            vec![SmSessionId::new("stream-064")]
        );
        authority
            .mark_retirement_candidate_attempted(&SmSessionId::new("stream-064"))
            .await;
        assert_eq!(
            authority
                .next_retirement_candidates()
                .await
                .expect("wrapped page"),
            first
        );
    }

    #[tokio::test]
    async fn sqlite_enrollment_checkpoint_and_retirement() {
        let authority = authority().await;
        crate::db::MigrationRunner::global()
            .run(&authority.database)
            .await
            .expect("migrate ingress");
        enrollment_checkpoint_and_retirement_round_trip(&authority).await;
    }

    #[tokio::test]
    async fn postgres_enrollment_checkpoint_and_retirement() {
        postgres_retirement_test(false).await;
    }

    #[tokio::test]
    async fn postgres_retirement_preserves_committed_batches_after_cancellation() {
        postgres_retirement_test(true).await;
    }

    async fn postgres_retirement_test(batches: bool) {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping postgres retirement test: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("postgres admin");
        let schema = format!("ingress_lifecycle_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create schema");
        let mut url = url::Url::parse(&database_url).expect("database URL");
        url.query_pairs_mut()
            .append_pair("options", &format!("-c search_path={schema}"));
        let config = DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url.to_string());
        let db = Database::from_config("ingress-lifecycle", &config)
            .await
            .expect("database");
        crate::db::MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrate");
        let lineage = test_lineage_config();
        let mut authority = IngressAuthority::for_test(db.clone()).await;
        #[cfg(feature = "clustering")]
        {
            db.execute("CREATE TABLE IF NOT EXISTS clustering_claims (entity TEXT NOT NULL, entity_type TEXT NOT NULL, PRIMARY KEY (entity, entity_type))").await.expect("claims schema");
            authority.uow = IngressUnitOfWork::open_with_node_identity(
                db,
                lineage,
                SharedNodeIdentity::new(waddle_xmpp::ownership::NodeIdentity::new(
                    "lifecycle",
                    "test",
                )),
            )
            .expect("unit of work");
        }
        #[cfg(not(feature = "clustering"))]
        {
            authority.uow = IngressUnitOfWork::open(db, lineage).expect("unit of work");
        }
        if batches {
            retirement_preserves_committed_batches(&authority).await;
        } else {
            enrollment_checkpoint_and_retirement_round_trip(&authority).await;
        }
        drop(authority);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop schema");
    }

    /// SQLite has one writer: the authority shares the global handle, so even
    /// a private in-memory database (the test fleet and dev mode) boots.
    #[tokio::test]
    async fn boot_shares_the_global_sqlite_handle() {
        let database = Database::in_memory("ingress-boot")
            .await
            .expect("open test database");
        crate::db::MigrationRunner::single()
            .run(&database)
            .await
            .expect("migrate");
        let lineage = test_lineage_config();
        crate::db::lineage::enroll(&database, &lineage)
            .await
            .expect("enroll lineage");
        let authority = IngressAuthority::new(
            IngressConfig::default(),
            database.clone(),
            lineage,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .expect("boot on a shared in-memory database");
        authority.drain_and_join(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn stopped_authority_rejects_checkpoint_operations_before_database_access() {
        let authority = authority().await;
        assert!(authority.drain_and_join(Duration::from_secs(1)).await);
        assert!(matches!(
            authority
                .flush_checkpoint(SmIngressId::new(), WireHandledCount::from_storage(1))
                .await,
            Err(IngressUowError::AuthorityStopped)
        ));
        assert!(matches!(
            authority
                .load_resume_checkpoint(&SmSessionId::new("stopped-stream"))
                .await,
            Err(IngressUowError::AuthorityStopped)
        ));
    }

    #[tokio::test]
    async fn authority_drain_waits_for_admitted_work_and_can_be_rejoined() {
        let authority = authority().await;
        let admitted = authority.admission.read().await;
        assert!(!authority.drain_and_join(Duration::from_millis(1)).await);
        assert!(authority.cancellation.is_cancelled());
        assert!(authority.force_stop.is_cancelled());
        drop(admitted);
        assert!(authority.drain_and_join(Duration::from_secs(1)).await);
    }
}

#[cfg(test)]
mod room_pin_tests;

#[cfg(test)]
pub(crate) mod test_support {
    use crate as waddle_server;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/ingress_support.rs"
    ));
}

#[cfg(test)]
mod subject_receipt_tests;
