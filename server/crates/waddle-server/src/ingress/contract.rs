//! Implementation behind the parent module facade.

use super::commit;
use super::execute;
use super::frame_receipt_retry;
use super::gc;
use super::CancellationToken;
use super::DatabaseDriver;
use super::DeliveryExecutionContext;
use super::Deps;
use super::Duration;
use super::ExecutionReport;
use super::HashMap;
use super::ImmediateSink;
use super::IngressStreamIdentity;
use super::IngressSubmission;
use super::RecoveryBinding;
use super::RecoveryEnvironment;
#[cfg(feature = "clustering")]
use super::SharedNodeIdentity;
use super::SmSessionId;
use super::{AliasOutcomeClass, IngressDecision, IngressDecisionClass};
use super::{Arc, StdMutex, Weak};
use super::{Database, DatabaseConfig, DatabaseError};
use super::{IngressConfig, IngressConfigError, LineageConfig};
use super::{IngressUnitOfWork, IngressUowError, SmIngressRepository, SmIngressStreamRepository};
use super::{Mutex, RwLock};
use super::{SmIngressId, WireHandledCount};

#[derive(Debug, thiserror::Error)]
pub enum IngressStartupError {
    #[error(transparent)]
    Config(#[from] IngressConfigError),
    #[error("clustered ingress requires the canonical node identity")]
    NodeIdentityMissing,
    #[error("failed to open the dedicated ingress pool")]
    Pool(#[source] DatabaseError),
    #[error("an attested PostgreSQL ingress lane is required for this constructor")]
    AttestedIngressPoolRequired,
    #[error(transparent)]
    UnitOfWork(#[from] IngressUowError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressRetirementOutcome {
    Deleted,
    DeferredClaim,
    StreamMissing,
}

pub(in crate::ingress) const RETIREMENT_BATCH_SIZE: u32 = 256;

// SQLite resume still needs BEGIN IMMEDIATE to observe prior checkpoint writes
// and retirement. Give this single acquisition more room under writer contention;
// this mitigates starvation without guaranteeing fairness. The resume claim and
// authority admission remain held during this wait.
const SQLITE_RESUME_CHECKPOINT_ACQUISITION_TIMEOUT: Duration = Duration::from_secs(1);

/// Boot-owned handle. Shutdown blocks new work and joins admitted operations and GC.
pub struct IngressAuthority {
    pub(in crate::ingress) observers:
        std::sync::OnceLock<Arc<crate::room_observation::RoomObservationActors>>,
    pub(in crate::ingress) database: Database,
    pub(in crate::ingress) uow: IngressUnitOfWork,
    pub(in crate::ingress) config: IngressConfig,
    pub(in crate::ingress) gc: gc::RetentionGcCoordinator,
    pub(in crate::ingress) recovery: RecoveryBinding,
    pub(in crate::ingress) cancellation: CancellationToken,
    pub(in crate::ingress) force_stop: CancellationToken,
    pub(in crate::ingress) gc_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(in crate::ingress) admission: Arc<RwLock<bool>>,
    pub(in crate::ingress) streams: StdMutex<HashMap<SmSessionId, Weak<RwLock<()>>>>,
    pub(in crate::ingress) retirement_cursor: Mutex<Option<SmSessionId>>,
    pub(in crate::ingress) frame_receipt_retries:
        StdMutex<frame_receipt_retry::FrameReceiptRetries>,
    /// Test-only: fires when a commit is about to wait for its stream lock.
    #[cfg(test)]
    pub(in crate::ingress) stream_wait_observer: StdMutex<Option<Arc<tokio::sync::Notify>>>,
    #[cfg(test)]
    pub(in crate::ingress) retirement_batch_gate: StdMutex<Option<Arc<tokio::sync::Notify>>>,
}

impl IngressAuthority {
    pub(crate) fn bind_room_observers(
        &self,
        actors: Arc<crate::room_observation::RoomObservationActors>,
    ) {
        let _ = self.observers.set(actors);
        self.uow.enable_room_observations();
    }

    pub(crate) fn room_observers(
        &self,
    ) -> Option<&Arc<crate::room_observation::RoomObservationActors>> {
        self.observers.get()
    }

    pub(crate) async fn observation_transaction(
        &self,
    ) -> Result<crate::ingress_uow::IngressUowTransaction<'_>, IngressUowError> {
        self.uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await
    }

    pub(crate) fn observation_cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Resolve a provider's configured room authority without minting a grant.
    pub(crate) async fn active_extension_room_grant(
        &self,
        plugin: &waddle_extensions::PluginId,
        room: &jid::BareJid,
    ) -> Result<Option<waddle_xmpp::auth::ExtensionGrantRef>, IngressUowError> {
        let mut transaction = self.uow.begin().await?;
        let grant = crate::ingress_uow::ExtensionGrantRepository::active_room_grant(
            &mut transaction,
            plugin,
            room,
        )
        .await?;
        transaction.commit().await?;
        Ok(grant)
    }

    /// Resolve configured authority in a short transaction before host planning.
    pub(crate) async fn active_extension_send_grant(
        &self,
        plugin: &waddle_extensions::PluginId,
    ) -> Result<Option<waddle_xmpp::auth::ExtensionGrantRef>, IngressUowError> {
        let mut transaction = self.uow.begin().await?;
        let grant = crate::ingress_uow::ExtensionGrantRepository::active_send_grant(
            &mut transaction,
            plugin,
        )
        .await?;
        transaction.commit().await?;
        Ok(grant)
    }

    /// Request a bounded maintenance pass.
    pub fn trigger_maintenance(&self) {
        self.gc.trigger();
    }

    /// Bind the boot-created environment without retaining the WebSocket state.
    pub fn bind_recovery_environment(&self, environment: Weak<dyn RecoveryEnvironment>) {
        self.recovery.bind(environment);
    }

    pub async fn try_new(
        config: IngressConfig,
        database: Database,
        lineage: LineageConfig,
        #[cfg(feature = "clustering")] node_identity: Option<SharedNodeIdentity>,
    ) -> Result<Self, IngressStartupError> {
        config.validate()?;
        // This legacy constructor rebuilds PostgreSQL pools from the retained
        // DSN, so it must never detach an attested pool's identity from its
        // checkout hooks. Shared attested pools use the reserved-lane
        // constructor below.
        if database.attested_pg_writer_capability().is_some() {
            return Err(IngressStartupError::AttestedIngressPoolRequired);
        }
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
        Self::finish_startup(
            config,
            database,
            lineage,
            #[cfg(feature = "clustering")]
            node_identity,
        )
        .await
    }

    /// Construct ingress over the reserved lane of a hook-bearing PostgreSQL
    /// pool. This remains an assembly API until startup can pass trusted owner
    /// identity configuration into `attested_pool`; it never opens or retargets
    /// a pool itself.
    pub async fn try_new_with_attested_database(
        config: IngressConfig,
        database: Database,
        lineage: LineageConfig,
        #[cfg(feature = "clustering")] node_identity: Option<SharedNodeIdentity>,
    ) -> Result<Self, IngressStartupError> {
        config.validate()?;
        if database.driver() != DatabaseDriver::Postgres
            || !database.is_ingress_lane()
            || database.ingress_pool_size() != Some(config.pool_size)
            || database.attested_pg_writer_capability().is_none()
        {
            return Err(IngressStartupError::AttestedIngressPoolRequired);
        }
        Self::finish_startup(
            config,
            database,
            lineage,
            #[cfg(feature = "clustering")]
            node_identity,
        )
        .await
    }

    async fn finish_startup(
        config: IngressConfig,
        database: Database,
        lineage: LineageConfig,
        #[cfg(feature = "clustering")] node_identity: Option<SharedNodeIdentity>,
    ) -> Result<Self, IngressStartupError> {
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
        let recovery = RecoveryBinding::default();
        let gc = gc::RetentionGcCoordinator::new(database.clone(), uow.clone(), recovery.clone());
        let cancellation = CancellationToken::new();
        let force_stop = CancellationToken::new();
        let gc_task = tokio::spawn(gc::run_retention_gc_coordinator(
            gc.clone(),
            cancellation.clone(),
            force_stop.clone(),
        ));
        Ok(Self {
            observers: std::sync::OnceLock::new(),
            database,
            uow,
            config,
            gc,
            recovery,
            cancellation,
            force_stop,
            gc_task: Mutex::new(Some(gc_task)),
            admission: Arc::new(RwLock::new(true)),
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
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn for_test(database: Database) -> Self {
        let lineage = super::test_lineage_config();
        crate::db::lineage::enroll(&database, &lineage)
            .await
            .expect("enroll test ingress lineage");
        let uow = IngressUnitOfWork::open(database.clone(), lineage)
            .expect("open test ingress unit of work");
        let recovery = RecoveryBinding::default();
        Self {
            observers: std::sync::OnceLock::new(),
            gc: gc::RetentionGcCoordinator::new(database.clone(), uow.clone(), recovery.clone()),
            recovery,
            database,
            uow,
            config: IngressConfig::default(),
            cancellation: CancellationToken::new(),
            force_stop: CancellationToken::new(),
            gc_task: Mutex::new(None),
            admission: Arc::new(RwLock::new(true)),
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

    pub(in crate::ingress) fn stream_activity(&self, stream_id: &SmSessionId) -> Arc<RwLock<()>> {
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
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
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = Some(observer.clone());
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
            let Some(id) = SmIngressStreamRepository::lookup_unclaimed_for_retirement(
                &mut transaction,
                stream_id,
            )
            .await?
            else {
                transaction.commit().await?;
                return Ok(IngressRetirementOutcome::StreamMissing);
            };
            let refs = SmIngressRepository::refs_for_stream_batch(
                &mut transaction,
                id,
                super::RETIREMENT_BATCH_SIZE,
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
                execute::terminalize_if_complete_in_transaction(
                    &mut transaction,
                    key,
                    DeliveryExecutionContext::StreamRetirement.into(),
                )
                .await?;
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
    pub(in crate::ingress) async fn wait_after_retirement_batch(&self) {
        let gate = self
            .retirement_batch_gate
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .clone();
        if let Some(gate) = gate {
            gate.notify_one();
            std::future::pending::<()>().await;
        }
    }

    pub async fn commit(&self, submission: &IngressSubmission) -> IngressDecision {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return super::non_advancing(IngressDecisionClass::Storage);
        }
        self.commit_admitted(submission).await
    }

    /// Caller retains admission for the entire operation.
    pub(in crate::ingress) async fn commit_admitted(
        &self,
        submission: &IngressSubmission,
    ) -> IngressDecision {
        let _stream_guard = match &submission.identity {
            IngressStreamIdentity::Resumable { stream_id, .. } => {
                let activity = self.stream_activity(stream_id);
                #[cfg(test)]
                if let Some(observer) = self
                    .stream_wait_observer
                    .lock()
                    .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
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
            return ExecutionReport::new(deps.delivery_execution_context.into());
        }
        let mut execution_deps = deps.clone();
        execution_deps.ingress_delivery_stop = Some(self.force_stop.clone());
        execute::execute_effects(
            &self.uow,
            &self.database,
            decision,
            sink,
            &execution_deps,
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
        let acquisition_timeout = match self.uow.database().driver() {
            DatabaseDriver::Sqlite => SQLITE_RESUME_CHECKPOINT_ACQUISITION_TIMEOUT,
            DatabaseDriver::Postgres => Duration::from_millis(100),
        };
        let mut transaction = self
            .uow
            .begin_with_timeouts(acquisition_timeout, Duration::from_millis(250))
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
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                .task
                .take();
            if let Some(task) = receipt_task {
                if task.await.is_err() {
                    return false;
                }
            }
            if let Some(observers) = self.room_observers() {
                observers.join().await;
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

pub(in crate::ingress) fn non_advancing(class: IngressDecisionClass) -> IngressDecision {
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

#[cfg(any(test, feature = "test-support"))]
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
