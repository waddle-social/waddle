mod capture;
mod observe;

pub use capture::{
    IngressEffectCapture, IngressEffectCaptureSnapshot, IngressShadowRoomFence,
    ShadowAuthorizationDeniedReason, ShadowDecisionMarker, ShadowSemanticRejectedReason,
};
pub use observe::{
    observe, IngressShadowAliasOutcome, IngressShadowCommitKind, IngressShadowDecisionClass,
    IngressShadowDropReason, IngressShadowObservation, IngressShadowRequestKind,
};

use crate::config::IngressShadowConfig;
use crate::config::LineageConfig;
use crate::db::{Database, DatabaseDriver};
#[cfg(feature = "clustering")]
use crate::ingress_uow::{
    run_with_retry, CanonicalMessageRepository, ClaimRepository, EffectIntentRepository,
    IngressUowError, PostgresIngressUnitOfWork, PrincipalAssertion, PrincipalRepository,
    ShadowFrontierOutcome, SmIngressRepository, SmIngressStreamRepository,
};
#[cfg(feature = "clustering")]
use jid::BareJid;
#[cfg(feature = "clustering")]
use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(feature = "clustering")]
use std::future::Future;
#[cfg(feature = "clustering")]
use std::pin::Pin;
#[cfg(all(feature = "clustering", test))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(feature = "clustering")]
use std::time::Duration;
#[cfg(feature = "clustering")]
use tokio::sync::mpsc;
#[cfg(feature = "clustering")]
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
#[cfg(feature = "clustering")]
use tokio_util::sync::CancellationToken;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
#[cfg(feature = "clustering")]
use waddle_xmpp::ingress::{
    digest::v1 as digest_v1, AliasOutcome, AliasResolution, DigestContext, DigestInput,
    DigestInputError, MessageKey,
};
use waddle_xmpp::ingress::{ConnectionGeneration, IngressOrdinal, NormalizedTarget};
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity, SharedNodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use xmpp_parsers::message::Message;

#[cfg(feature = "clustering")]
const DEFAULT_LOCK_TIMEOUT_MS: u64 = 250;
#[cfg(feature = "clustering")]
const DEFAULT_STATEMENT_TIMEOUT_MS: u64 = 1_500;
#[cfg(feature = "clustering")]
const DEFAULT_TX_DEADLINE: Duration = Duration::from_millis(2_500);
#[cfg(feature = "clustering")]
const ENROLLMENT_RETRY_DELAY: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, PartialEq)]
pub struct IngressShadowSubmission {
    pub stream_id: SmSessionId,
    pub owner: NodeIdentity,
    pub claim_epoch: ClaimEpoch,
    pub handled_ordinal: IngressOrdinal,
    pub principal: AuthenticatedPrincipalRef,
    pub target: NormalizedTarget,
    pub message: Message,
    pub capture: IngressEffectCaptureSnapshot,
    pub connection_generation: Option<ConnectionGeneration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressShadowDisposition {
    Disabled,
    Enqueued,
    QueueFull,
    Closed,
}

#[derive(Debug, Clone)]
pub struct IngressShadowHandle {
    inner: Arc<IngressShadowInner>,
}

#[derive(Debug)]
enum IngressShadowInner {
    Disabled,
    #[cfg(feature = "clustering")]
    Worker {
        tx: mpsc::UnboundedSender<QueuedIngressShadowTask>,
        /// One admission slot reserved for fresh SM-stream enrollment. A
        /// saturated submission queue must not make an otherwise healthy
        /// newly-enabled stream permanently unenrolled.
        enrollment_capacity: Arc<Semaphore>,
        enrollment_retries: Arc<PendingEnrollmentRetries>,
        capacity: Arc<Semaphore>,
        shutdown: Arc<IngressShadowShutdown>,
    },
}

#[cfg(feature = "clustering")]
#[derive(Debug, Default)]
struct IngressShadowShutdown {
    cancellation: CancellationToken,
    #[cfg(test)]
    complete: AtomicBool,
    #[cfg(test)]
    complete_notify: Notify,
}

#[cfg(feature = "clustering")]
impl IngressShadowShutdown {
    fn cancel(&self) {
        self.cancellation.cancel();
    }

    fn mark_complete(&self) {
        #[cfg(test)]
        {
            self.complete.store(true, Ordering::Release);
            self.complete_notify.notify_waiters();
        }
    }

    #[cfg(test)]
    async fn wait_for_completion(&self) {
        loop {
            let notified = self.complete_notify.notified();
            if self.complete.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(feature = "clustering")]
impl Drop for IngressShadowInner {
    fn drop(&mut self) {
        if let Self::Worker { shutdown, .. } = self {
            shutdown.cancel();
        }
    }
}

#[derive(Debug)]
enum IngressShadowTask {
    Enroll { stream_id: SmSessionId },
    Submit(Box<IngressShadowSubmission>),
}

#[cfg(feature = "clustering")]
struct QueuedIngressShadowTask {
    task: IngressShadowTask,
    permit: OwnedSemaphorePermit,
}

#[cfg(feature = "clustering")]
type IngressShadowExecuteFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
#[cfg(feature = "clustering")]
type IngressShadowExecutor =
    Arc<dyn Fn(IngressShadowTask) -> IngressShadowExecuteFuture + Send + Sync>;

#[cfg(feature = "clustering")]
#[derive(Debug, Default)]
struct PendingEnrollmentRetries {
    pending: std::sync::Mutex<HashSet<SmSessionId>>,
    notify: Notify,
}

#[cfg(feature = "clustering")]
impl PendingEnrollmentRetries {
    fn schedule(&self, stream_id: SmSessionId) {
        let mut pending = self
            .pending
            .lock()
            .expect("pending enrollment retry mutex must not be poisoned");
        if pending.insert(stream_id) {
            self.notify.notify_one();
        }
    }

    async fn run(
        self: Arc<Self>,
        tx: mpsc::UnboundedSender<QueuedIngressShadowTask>,
        enrollment_capacity: Arc<Semaphore>,
        capacity: Arc<Semaphore>,
        shutdown: CancellationToken,
    ) {
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = self.notify.notified() => {},
            }
            loop {
                let batch = {
                    let mut pending = self
                        .pending
                        .lock()
                        .expect("pending enrollment retry mutex must not be poisoned");
                    if pending.is_empty() {
                        break;
                    }
                    pending.drain().collect::<Vec<_>>()
                };
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(ENROLLMENT_RETRY_DELAY) => {},
                }
                for stream_id in batch {
                    if shutdown.is_cancelled() {
                        return;
                    }
                    if matches!(
                        try_send_worker_task(
                            &tx,
                            &enrollment_capacity,
                            &capacity,
                            IngressShadowTask::Enroll {
                                stream_id: stream_id.clone(),
                            },
                        ),
                        IngressShadowDisposition::QueueFull
                    ) {
                        self.schedule(stream_id);
                    }
                }
            }
        }
    }
}

impl IngressShadowTask {
    fn kind(&self) -> IngressShadowRequestKind {
        match self {
            Self::Enroll { .. } => IngressShadowRequestKind::Enroll,
            Self::Submit(_) => IngressShadowRequestKind::Submit,
        }
    }

    fn stream_id(&self) -> &SmSessionId {
        match self {
            Self::Enroll { stream_id } => stream_id,
            Self::Submit(submission) => &submission.stream_id,
        }
    }
}

impl IngressShadowHandle {
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(IngressShadowInner::Disabled),
        }
    }

    pub async fn new(
        config: IngressShadowConfig,
        database: Database,
        lineage: LineageConfig,
        node_identity: Option<SharedNodeIdentity>,
    ) -> Self {
        if !config.enabled || database.driver() != DatabaseDriver::Postgres {
            return Self::disabled();
        }
        #[cfg(not(feature = "clustering"))]
        {
            let _ = (config, database, lineage, node_identity);
            Self::disabled()
        }
        #[cfg(feature = "clustering")]
        {
            let Some(node_identity) = node_identity else {
                return Self::disabled();
            };
            let mut shadow_database_config = crate::db::DatabaseConfig::new(
                database.driver(),
                database.database_url().to_owned(),
            );
            shadow_database_config.pool_size = config.pool_size;
            let database = match Database::from_config("ingress-shadow", &shadow_database_config)
                .await
            {
                Ok(database) => database,
                Err(error) => {
                    tracing::warn!(%error, "ingress shadow disabled because its dedicated database pool could not open");
                    return Self::disabled();
                }
            };
            let worker = IngressShadowProcessor {
                database,
                lineage,
                node_identity,
                retry_attempts: config.retry_attempts,
                #[cfg(test)]
                forced_alias_serialization_failures: Arc::new(std::sync::atomic::AtomicUsize::new(
                    0,
                )),
            };
            Self::spawn_worker(
                config.queue_capacity,
                ingress_shadow_max_concurrency(config.queue_capacity, config.pool_size),
                Arc::new(move |task| {
                    let worker = worker.clone();
                    Box::pin(async move {
                        worker.execute(task).await;
                    })
                }),
            )
        }
    }

    pub fn try_enroll_stream(&self, stream_id: SmSessionId) -> IngressShadowDisposition {
        self.try_send(IngressShadowTask::Enroll { stream_id })
    }

    pub fn ensure_stream_enrollment(&self, stream_id: SmSessionId) -> IngressShadowDisposition {
        let disposition = self.try_enroll_stream(stream_id.clone());
        if matches!(disposition, IngressShadowDisposition::QueueFull) {
            #[cfg(feature = "clustering")]
            if let IngressShadowInner::Worker {
                enrollment_retries, ..
            } = self.inner.as_ref()
            {
                enrollment_retries.schedule(stream_id);
            }
        }
        disposition
    }

    pub fn is_enabled(&self) -> bool {
        #[cfg(feature = "clustering")]
        {
            matches!(self.inner.as_ref(), IngressShadowInner::Worker { .. })
        }
        #[cfg(not(feature = "clustering"))]
        {
            false
        }
    }

    pub fn try_submit(&self, submission: IngressShadowSubmission) -> IngressShadowDisposition {
        self.try_send(IngressShadowTask::Submit(Box::new(submission)))
    }

    fn try_send(&self, task: IngressShadowTask) -> IngressShadowDisposition {
        let kind = task.kind();
        let stream_id = task.stream_id().clone();
        let disposition = match self.inner.as_ref() {
            IngressShadowInner::Disabled => IngressShadowDisposition::Disabled,
            #[cfg(feature = "clustering")]
            IngressShadowInner::Worker {
                tx,
                enrollment_capacity,
                enrollment_retries: _,
                capacity,
                ..
            } => try_send_worker_task(tx, enrollment_capacity, capacity, task),
        };
        observe_disposition(kind, stream_id, disposition)
    }

    #[cfg(feature = "clustering")]
    fn spawn_worker(
        queue_capacity: usize,
        max_concurrency: usize,
        execute: IngressShadowExecutor,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let capacity = Arc::new(Semaphore::new(queue_capacity));
        let enrollment_capacity = Arc::new(Semaphore::new(1));
        let enrollment_retries = Arc::new(PendingEnrollmentRetries::default());
        let shutdown = Arc::new(IngressShadowShutdown::default());
        let scheduler =
            tokio::spawn(IngressShadowScheduler::new(rx, max_concurrency, execute).run());
        let retry = tokio::spawn(enrollment_retries.clone().run(
            tx.clone(),
            enrollment_capacity.clone(),
            capacity.clone(),
            shutdown.cancellation.clone(),
        ));
        let shutdown_completion = shutdown.clone();
        tokio::spawn(async move {
            shutdown_completion.cancellation.cancelled().await;
            let _ = retry.await;
            let _ = scheduler.await;
            shutdown_completion.mark_complete();
        });
        Self {
            inner: Arc::new(IngressShadowInner::Worker {
                tx,
                enrollment_capacity,
                enrollment_retries,
                capacity,
                shutdown,
            }),
        }
    }

    #[cfg(test)]
    fn shutdown(&self) -> Option<Arc<IngressShadowShutdown>> {
        match self.inner.as_ref() {
            IngressShadowInner::Disabled => None,
            IngressShadowInner::Worker { shutdown, .. } => Some(shutdown.clone()),
        }
    }
}

#[cfg(feature = "clustering")]
fn try_send_worker_task(
    tx: &mpsc::UnboundedSender<QueuedIngressShadowTask>,
    enrollment_capacity: &Arc<Semaphore>,
    capacity: &Arc<Semaphore>,
    task: IngressShadowTask,
) -> IngressShadowDisposition {
    let permit = match capacity.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(tokio::sync::TryAcquireError::NoPermits) => {
            if matches!(&task, IngressShadowTask::Enroll { .. }) {
                match enrollment_capacity.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(tokio::sync::TryAcquireError::NoPermits) => {
                        return IngressShadowDisposition::QueueFull;
                    }
                    Err(tokio::sync::TryAcquireError::Closed) => {
                        return IngressShadowDisposition::Closed;
                    }
                }
            } else {
                return IngressShadowDisposition::QueueFull;
            }
        }
        Err(tokio::sync::TryAcquireError::Closed) => {
            return IngressShadowDisposition::Closed;
        }
    };
    match tx.send(QueuedIngressShadowTask { task, permit }) {
        Ok(()) => IngressShadowDisposition::Enqueued,
        Err(_closed) => IngressShadowDisposition::Closed,
    }
}

fn observe_disposition(
    kind: IngressShadowRequestKind,
    stream_id: SmSessionId,
    disposition: IngressShadowDisposition,
) -> IngressShadowDisposition {
    match disposition {
        IngressShadowDisposition::Enqueued => {
            observe(IngressShadowObservation::Accepted { kind, stream_id })
        }
        IngressShadowDisposition::Disabled => observe(IngressShadowObservation::Dropped {
            kind,
            stream_id,
            reason: IngressShadowDropReason::Disabled,
        }),
        IngressShadowDisposition::QueueFull => observe(IngressShadowObservation::Dropped {
            kind,
            stream_id,
            reason: IngressShadowDropReason::QueueFull,
        }),
        IngressShadowDisposition::Closed => observe(IngressShadowObservation::Dropped {
            kind,
            stream_id,
            reason: IngressShadowDropReason::Closed,
        }),
    }
    disposition
}

#[cfg(feature = "clustering")]
fn ingress_shadow_max_concurrency(queue_capacity: usize, pool_size: u32) -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1)
        .min(queue_capacity)
        .min(pool_size as usize)
        .max(1)
}

#[cfg(feature = "clustering")]
#[derive(Clone)]
struct IngressShadowProcessor {
    database: Database,
    lineage: LineageConfig,
    node_identity: SharedNodeIdentity,
    retry_attempts: usize,
    /// Test-only deterministic fault point inside the transaction, after
    /// fences and digest evaluation but before alias persistence.  PostgreSQL
    /// serialization failures are otherwise difficult to force reliably at
    /// the processor's READ COMMITTED boundary.
    #[cfg(test)]
    forced_alias_serialization_failures: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "clustering")]
impl IngressShadowProcessor {
    async fn execute(&self, task: IngressShadowTask) {
        let kind = task.kind();
        let stream_id = task.stream_id().clone();
        let claim_epoch = match &task {
            IngressShadowTask::Enroll { .. } => None,
            IngressShadowTask::Submit(submission) => Some(submission.claim_epoch),
        };
        let handled_ordinal = match &task {
            IngressShadowTask::Enroll { .. } => None,
            IngressShadowTask::Submit(submission) => Some(submission.handled_ordinal),
        };
        match task {
            IngressShadowTask::Enroll {
                stream_id: enroll_stream,
            } => {
                let mut attempts = 0_usize;
                let result = run_with_retry(self.retry_attempts, || {
                    attempts += 1;
                    self.execute_enrollment(&enroll_stream)
                })
                .await;
                match result {
                    Ok(kind) => {
                        if attempts > 1 {
                            waddle_xmpp::telemetry::reliability::increment_ingress_shadow_tx_retry(
                                waddle_xmpp::telemetry::attributes::IngressRetryOutcome::Retried,
                            );
                        }
                        observe(IngressShadowObservation::Committed {
                            stream_id,
                            claim_epoch,
                            handled_ordinal,
                            kind,
                        });
                    }
                    Err(_error) => observe(IngressShadowObservation::Failed {
                        kind,
                        stream_id,
                        claim_epoch,
                        handled_ordinal,
                    }),
                }
            }
            IngressShadowTask::Submit(submission) => {
                let mut attempts = 0_usize;
                let timed = tokio::time::timeout(
                    DEFAULT_TX_DEADLINE,
                    run_with_retry(self.retry_attempts, || {
                        attempts += 1;
                        self.execute_submission(&submission)
                    }),
                )
                .await;
                match timed {
                    Ok(Ok(outcome)) => {
                        if attempts > 1 {
                            waddle_xmpp::telemetry::reliability::increment_ingress_shadow_tx_retry(
                                waddle_xmpp::telemetry::attributes::IngressRetryOutcome::Retried,
                            );
                        }
                        if let Some(kind) = outcome.commit_kind {
                            observe(IngressShadowObservation::Committed {
                                stream_id: stream_id.clone(),
                                claim_epoch,
                                handled_ordinal,
                                kind,
                            });
                        }
                        observe(IngressShadowObservation::Decision {
                            stream_id,
                            claim_epoch,
                            handled_ordinal,
                            class: outcome.decision,
                            alias: outcome.alias,
                        });
                    }
                    Ok(Err(exhausted)) => {
                        if exhausted.attempts >= self.retry_attempts
                            && matches!(
                                exhausted.last_error.retry_class(),
                                crate::ingress_uow::DbRetryClass::SerializationFailure
                                    | crate::ingress_uow::DbRetryClass::Deadlock
                            )
                        {
                            waddle_xmpp::telemetry::reliability::increment_ingress_shadow_tx_retry(
                                waddle_xmpp::telemetry::attributes::IngressRetryOutcome::Exhausted,
                            );
                        }
                        let class = match exhausted.last_error.retry_class() {
                            crate::ingress_uow::DbRetryClass::SerializationFailure
                            | crate::ingress_uow::DbRetryClass::Deadlock => {
                                IngressShadowDecisionClass::SerializationExhaustion
                            }
                            crate::ingress_uow::DbRetryClass::NotRetryable => {
                                IngressShadowDecisionClass::Storage
                            }
                        };
                        observe(IngressShadowObservation::Decision {
                            stream_id,
                            claim_epoch,
                            handled_ordinal,
                            class,
                            alias: IngressShadowAliasOutcome::None,
                        });
                    }
                    Err(_) => observe(IngressShadowObservation::Decision {
                        stream_id,
                        claim_epoch,
                        handled_ordinal,
                        class: IngressShadowDecisionClass::Storage,
                        alias: IngressShadowAliasOutcome::None,
                    }),
                }
            }
        }
    }

    async fn execute_enrollment(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<IngressShadowCommitKind, IngressUowError> {
        let uow = PostgresIngressUnitOfWork::open_with_node_identity(
            self.database.clone(),
            self.lineage.clone(),
            self.node_identity.clone(),
        )?;
        let mut transaction = uow.begin().await?;
        transaction
            .set_local_timeouts(DEFAULT_LOCK_TIMEOUT_MS, DEFAULT_STATEMENT_TIMEOUT_MS)
            .await?;
        let _ = SmIngressStreamRepository::mint(&mut transaction, stream_id).await?;
        transaction.commit().await?;
        Ok(IngressShadowCommitKind::Enrolled)
    }

    async fn execute_submission(
        &self,
        submission: &IngressShadowSubmission,
    ) -> Result<ShadowSubmissionOutcome, IngressUowError> {
        let uow = PostgresIngressUnitOfWork::open_with_node_identity(
            self.database.clone(),
            self.lineage.clone(),
            self.node_identity.clone(),
        )?;
        let mut transaction = uow.begin().await?;
        transaction
            .set_local_timeouts(DEFAULT_LOCK_TIMEOUT_MS, DEFAULT_STATEMENT_TIMEOUT_MS)
            .await?;

        match PrincipalRepository::assert_principal(&mut transaction, &submission.principal).await?
        {
            PrincipalAssertion::Asserted => {}
            PrincipalAssertion::PrincipalAssertionFailed => {
                return Ok(ShadowSubmissionOutcome::rolled_back(
                    IngressShadowDecisionClass::PrincipalMissing,
                ));
            }
        }

        let fence = match ClaimRepository::assert_sm_claim(
            &mut transaction,
            &submission.stream_id,
            &submission.owner,
            submission.claim_epoch,
        )
        .await
        {
            Ok(fence) => fence,
            Err(IngressUowError::ClaimFenceMissing) => {
                return Ok(ShadowSubmissionOutcome::rolled_back(
                    IngressShadowDecisionClass::ClaimFenceMissing,
                ));
            }
            Err(error) => return Err(error),
        };

        let Some((sm_ingress_id, _frontier)) =
            SmIngressStreamRepository::lock(&mut transaction, &fence, &submission.stream_id)
                .await?
        else {
            transaction.commit().await?;
            return Ok(ShadowSubmissionOutcome::committed(
                IngressShadowCommitKind::SkippedUnenrolled,
                IngressShadowDecisionClass::SkippedUnenrolled,
                IngressShadowAliasOutcome::None,
            ));
        };

        if let Some(room_fence) = submission.room_claim_target() {
            match ClaimRepository::assert_room_claim(
                &mut transaction,
                &room_fence.room,
                &room_fence.owner,
                room_fence.claim_epoch,
            )
            .await
            {
                Ok(_) => {}
                Err(IngressUowError::ClaimFenceMissing) => {
                    return Ok(ShadowSubmissionOutcome::rolled_back(
                        IngressShadowDecisionClass::ClaimFenceMissing,
                    ));
                }
                Err(error) => return Err(error),
            }
        }

        let rowless_decision = submission.rowless_decision_marker();
        if matches!(
            rowless_decision,
            Some(IngressShadowDecisionClass::CaptureOverflow)
        ) {
            return Ok(ShadowSubmissionOutcome::rolled_back(
                IngressShadowDecisionClass::CaptureOverflow,
            ));
        }
        let digest_input = match digest_input_from_submission(submission)? {
            Ok(input) => input,
            Err(decision) => {
                let commit_kind = advance_shadow_frontier(
                    &mut transaction,
                    &fence,
                    sm_ingress_id,
                    submission.handled_ordinal,
                )
                .await?;
                if matches!(commit_kind, IngressShadowCommitKind::Stale) {
                    return Ok(ShadowSubmissionOutcome::rolled_back(
                        IngressShadowDecisionClass::FrontierStale,
                    ));
                }
                transaction.commit().await?;
                return Ok(ShadowSubmissionOutcome::committed(
                    commit_kind,
                    decision,
                    IngressShadowAliasOutcome::None,
                ));
            }
        };
        if let Some(decision) = rowless_decision {
            let commit_kind = advance_shadow_frontier(
                &mut transaction,
                &fence,
                sm_ingress_id,
                submission.handled_ordinal,
            )
            .await?;
            if matches!(commit_kind, IngressShadowCommitKind::Stale) {
                return Ok(ShadowSubmissionOutcome::rolled_back(
                    IngressShadowDecisionClass::FrontierStale,
                ));
            }
            transaction.commit().await?;
            return Ok(ShadowSubmissionOutcome::committed(
                commit_kind,
                decision,
                IngressShadowAliasOutcome::None,
            ));
        }

        let digest = digest_v1::digest(&digest_input);
        #[cfg(test)]
        if self
            .forced_alias_serialization_failures
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(IngressUowError::Database {
                retry_class: crate::ingress_uow::DbRetryClass::SerializationFailure,
            });
        }
        let (message_key, decision, alias) =
            record_shadow_message(&mut transaction, submission, &digest_input, &digest).await?;

        if matches!(decision, IngressShadowDecisionClass::AliasConflict) {
            let commit_kind = advance_shadow_frontier(
                &mut transaction,
                &fence,
                sm_ingress_id,
                submission.handled_ordinal,
            )
            .await?;
            if matches!(commit_kind, IngressShadowCommitKind::Stale) {
                return Ok(ShadowSubmissionOutcome::rolled_back(
                    IngressShadowDecisionClass::FrontierStale,
                ));
            }
            transaction.commit().await?;
            return Ok(ShadowSubmissionOutcome::committed(
                commit_kind,
                decision,
                alias,
            ));
        }

        let _ = EffectIntentRepository::record_all(
            &mut transaction,
            message_key,
            &submission.capture.intents,
        )
        .await?;
        let commit_kind = advance_shadow_frontier(
            &mut transaction,
            &fence,
            sm_ingress_id,
            submission.handled_ordinal,
        )
        .await?;
        if matches!(commit_kind, IngressShadowCommitKind::Stale) {
            return Ok(ShadowSubmissionOutcome::rolled_back(
                IngressShadowDecisionClass::FrontierStale,
            ));
        }
        let _ = SmIngressRepository::insert(
            &mut transaction,
            sm_ingress_id,
            submission.handled_ordinal,
            message_key,
        )
        .await?;
        transaction.commit().await?;
        Ok(ShadowSubmissionOutcome::committed(
            commit_kind,
            decision,
            alias,
        ))
    }
}

#[cfg(feature = "clustering")]
struct IngressShadowScheduler {
    rx: mpsc::UnboundedReceiver<QueuedIngressShadowTask>,
    completion_tx: mpsc::UnboundedSender<SmSessionId>,
    completion_rx: mpsc::UnboundedReceiver<SmSessionId>,
    execute: IngressShadowExecutor,
    max_concurrency: usize,
    active_streams: HashSet<SmSessionId>,
    ready_streams: VecDeque<SmSessionId>,
    ready_members: HashSet<SmSessionId>,
    queued_by_stream: HashMap<SmSessionId, VecDeque<QueuedIngressShadowTask>>,
}

#[cfg(feature = "clustering")]
impl IngressShadowScheduler {
    fn new(
        rx: mpsc::UnboundedReceiver<QueuedIngressShadowTask>,
        max_concurrency: usize,
        execute: IngressShadowExecutor,
    ) -> Self {
        let (completion_tx, completion_rx) = mpsc::unbounded_channel();
        Self {
            rx,
            completion_tx,
            completion_rx,
            execute,
            max_concurrency: max_concurrency.max(1),
            active_streams: HashSet::new(),
            ready_streams: VecDeque::new(),
            ready_members: HashSet::new(),
            queued_by_stream: HashMap::new(),
        }
    }

    async fn run(mut self) {
        let mut intake_open = true;
        loop {
            self.dispatch_ready();
            if !intake_open && self.active_streams.is_empty() && self.queued_by_stream.is_empty() {
                break;
            }
            tokio::select! {
                maybe_task = self.rx.recv(), if intake_open => {
                    match maybe_task {
                        Some(task) => self.enqueue(task),
                        None => intake_open = false,
                    }
                }
                maybe_complete = self.completion_rx.recv(), if !self.active_streams.is_empty() => {
                    if let Some(stream_id) = maybe_complete {
                        self.complete(stream_id);
                    }
                }
            }
        }
    }

    fn enqueue(&mut self, task: QueuedIngressShadowTask) {
        let stream_id = task.task.stream_id().clone();
        let queue = self.queued_by_stream.entry(stream_id.clone()).or_default();
        queue.push_back(task);
        if queue.len() == 1 && !self.active_streams.contains(&stream_id) {
            self.mark_ready(stream_id);
        }
    }

    fn complete(&mut self, stream_id: SmSessionId) {
        self.active_streams.remove(&stream_id);
        if self
            .queued_by_stream
            .get(&stream_id)
            .is_some_and(|queue| !queue.is_empty())
        {
            self.mark_ready(stream_id);
        } else {
            self.queued_by_stream.remove(&stream_id);
        }
    }

    fn dispatch_ready(&mut self) {
        while self.active_streams.len() < self.max_concurrency {
            let Some(stream_id) = self.ready_streams.pop_front() else {
                break;
            };
            self.ready_members.remove(&stream_id);
            let queued = {
                let Some(queue) = self.queued_by_stream.get_mut(&stream_id) else {
                    continue;
                };
                let next = queue.pop_front();
                let became_empty = queue.is_empty();
                (next, became_empty)
            };
            let Some(task) = queued.0 else {
                self.queued_by_stream.remove(&stream_id);
                continue;
            };
            if queued.1 {
                self.queued_by_stream.remove(&stream_id);
            }
            self.active_streams.insert(stream_id.clone());
            let completion_tx = self.completion_tx.clone();
            let execute = self.execute.clone();
            tokio::spawn(async move {
                (execute)(task.task).await;
                drop(task.permit);
                let _ = completion_tx.send(stream_id);
            });
        }
    }

    fn mark_ready(&mut self, stream_id: SmSessionId) {
        if self.ready_members.insert(stream_id.clone()) {
            self.ready_streams.push_back(stream_id);
        }
    }
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShadowSubmissionOutcome {
    commit_kind: Option<IngressShadowCommitKind>,
    decision: IngressShadowDecisionClass,
    alias: IngressShadowAliasOutcome,
}

#[cfg(feature = "clustering")]
impl ShadowSubmissionOutcome {
    fn committed(
        commit_kind: IngressShadowCommitKind,
        decision: IngressShadowDecisionClass,
        alias: IngressShadowAliasOutcome,
    ) -> Self {
        Self {
            commit_kind: Some(commit_kind),
            decision,
            alias,
        }
    }

    fn rolled_back(decision: IngressShadowDecisionClass) -> Self {
        Self {
            commit_kind: None,
            decision,
            alias: IngressShadowAliasOutcome::None,
        }
    }
}

#[cfg(feature = "clustering")]
async fn record_shadow_message(
    transaction: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    submission: &IngressShadowSubmission,
    digest_input: &DigestInput,
    digest: &waddle_xmpp::ingress::SemanticDigest,
) -> Result<
    (
        MessageKey,
        IngressShadowDecisionClass,
        IngressShadowAliasOutcome,
    ),
    IngressUowError,
> {
    let origin = digest_input.origin().cloned();
    let minted = || MessageKey::new();
    let result = match origin.as_ref() {
        Some(origin_id) => {
            CanonicalMessageRepository::resolve_and_record_alias(
                transaction,
                submission.principal.bare_jid(),
                &submission.target,
                origin_id,
                digest,
                minted,
            )
            .await?
        }
        None => {
            let key = minted();
            CanonicalMessageRepository::record(transaction, key, digest).await?;
            AliasResolution::NoOrigin(key)
        }
    };
    Ok(match result {
        AliasResolution::NoOrigin(message_key) => (
            message_key,
            IngressShadowDecisionClass::Accepted,
            IngressShadowAliasOutcome::None,
        ),
        AliasResolution::Aliased(AliasOutcome::Inserted(message_key)) => (
            message_key,
            IngressShadowDecisionClass::Accepted,
            IngressShadowAliasOutcome::Inserted,
        ),
        AliasResolution::Aliased(AliasOutcome::Existing(message_key)) => (
            message_key,
            IngressShadowDecisionClass::ExistingSameDigest,
            IngressShadowAliasOutcome::Existing,
        ),
        AliasResolution::Aliased(AliasOutcome::Conflict(_)) => (
            MessageKey::new(),
            IngressShadowDecisionClass::AliasConflict,
            IngressShadowAliasOutcome::Conflict,
        ),
    })
}

#[cfg(feature = "clustering")]
async fn advance_shadow_frontier(
    transaction: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    fence: &crate::ingress_uow::SmClaimFence<'_>,
    sm_ingress_id: waddle_xmpp::ingress::SmIngressId,
    handled_ordinal: IngressOrdinal,
) -> Result<IngressShadowCommitKind, IngressUowError> {
    Ok(
        match SmIngressStreamRepository::advance_frontier(
            transaction,
            fence,
            sm_ingress_id,
            handled_ordinal,
        )
        .await?
        {
            ShadowFrontierOutcome::Advanced => IngressShadowCommitKind::Advanced,
            ShadowFrontierOutcome::Idempotent => IngressShadowCommitKind::Idempotent,
            ShadowFrontierOutcome::Stale { .. } => IngressShadowCommitKind::Stale,
        },
    )
}

#[cfg(feature = "clustering")]
fn digest_input_from_submission(
    submission: &IngressShadowSubmission,
) -> Result<Result<DigestInput, IngressShadowDecisionClass>, IngressUowError> {
    let mut message = submission.message.clone();
    loop {
        let context = DigestContext {
            target: submission.target.clone(),
            server_authorities: submission.server_authorities(),
            stanza_lang: submission.capture.stanza_lang.clone(),
        };
        match DigestInput::from_parsed(&message, &context) {
            Ok(input) => return Ok(Ok(input)),
            Err(DigestInputError::ForgedServerStanzaId { by }) => {
                strip_stanza_id_for_authority(&mut message, &by);
            }
            Err(_) => return Ok(Err(IngressShadowDecisionClass::SemanticMalformed)),
        }
    }
}

#[cfg(feature = "clustering")]
fn strip_stanza_id_for_authority(message: &mut Message, authority: &BareJid) {
    message.payloads.retain(|payload| {
        !(payload.name() == "stanza-id"
            && payload.ns() == waddle_xmpp_core::xep0359::NS_SID
            && payload
                .attr("by")
                .and_then(|raw| raw.parse::<BareJid>().ok())
                .is_some_and(|by| by == *authority))
    });
}

#[cfg(feature = "clustering")]
impl IngressShadowSubmission {
    fn rowless_decision_marker(&self) -> Option<IngressShadowDecisionClass> {
        if self
            .capture
            .markers
            .iter()
            .any(|marker| matches!(marker, ShadowDecisionMarker::Overflow))
        {
            return Some(IngressShadowDecisionClass::CaptureOverflow);
        }
        if self
            .capture
            .markers
            .iter()
            .any(|marker| matches!(marker, ShadowDecisionMarker::AuthorizationDenied { .. }))
        {
            return Some(IngressShadowDecisionClass::AuthorizationDenied);
        }
        if self
            .capture
            .markers
            .iter()
            .any(|marker| matches!(marker, ShadowDecisionMarker::SemanticRejected { .. }))
        {
            return Some(IngressShadowDecisionClass::SemanticMalformed);
        }
        None
    }

    fn room_claim_target(&self) -> Option<&IngressShadowRoomFence> {
        self.capture.room_fence.as_ref()
    }

    fn server_authorities(&self) -> Vec<BareJid> {
        let mut authorities = vec![self.principal.bare_jid().clone()];
        if let Some(room_fence) = self.room_claim_target() {
            if !authorities.contains(&room_fence.room) {
                authorities.push(room_fence.room.clone());
            }
        }
        authorities
    }
}

#[cfg(all(test, feature = "clustering"))]
mod tests {
    use super::*;
    use crate::{
        config::LineageConfig,
        db::{lineage, Database, DatabaseConfig, DatabaseDriver, IntoParams, MigrationRunner},
    };
    use jid::{BareJid, Jid};
    use sqlx::Connection;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{oneshot, Notify};
    use tokio::time::Duration;
    use uuid::Uuid;
    use waddle_xmpp::auth::{AuthContextId, AuthContextVersion, PrincipalAuthEpoch};
    use waddle_xmpp::ingress::{ConnectionGeneration, NormalizedTarget};
    use waddle_xmpp::ownership::ClaimStore;
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::message::MessageType;

    fn principal() -> AuthenticatedPrincipalRef {
        AuthenticatedPrincipalRef::new(
            "romeo@example.com".parse().expect("bare jid"),
            AuthContextId::new(uuid::Uuid::new_v4()),
            AuthContextVersion::new(1),
            PrincipalAuthEpoch::new(1),
        )
    }

    fn base_submission(message: Message) -> IngressShadowSubmission {
        IngressShadowSubmission {
            stream_id: SmSessionId::new("stream-a"),
            owner: NodeIdentity::new("node-a", "epoch-a"),
            claim_epoch: ClaimEpoch(7),
            handled_ordinal: IngressOrdinal::FIRST,
            principal: principal(),
            target: NormalizedTarget::Bare(
                "room@conference.example.com"
                    .parse::<BareJid>()
                    .expect("room jid"),
            ),
            message,
            capture: IngressEffectCaptureSnapshot {
                stanza_lang: Some(xmpp_parsers::message::Lang::from("en")),
                sanitized_message: None,
                room_fence: None,
                intents: Vec::new(),
                markers: Vec::new(),
            },
            connection_generation: Some(ConnectionGeneration::INITIAL),
        }
    }

    fn test_handle(
        queue_capacity: usize,
        max_concurrency: usize,
        execute: impl Fn(IngressShadowTask) -> IngressShadowExecuteFuture + Send + Sync + 'static,
    ) -> IngressShadowHandle {
        IngressShadowHandle::spawn_worker(queue_capacity, max_concurrency, Arc::new(execute))
    }

    struct PoolCloseSignal(Option<oneshot::Sender<()>>);

    impl Drop for PoolCloseSignal {
        fn drop(&mut self) {
            if let Some(closed) = self.0.take() {
                let _ = closed.send(());
            }
        }
    }

    /// A real PostgreSQL fixture for the shadow transaction.  It intentionally
    /// lives beside the processor rather than testing repositories in
    /// isolation: B3's contract is the transaction's decision/row frontier
    /// matrix, including rollbacks between the individual repository calls.
    struct ShadowFixture {
        db: Database,
        processor: IngressShadowProcessor,
        admin: sqlx::PgPool,
        schema: String,
        stream_id: SmSessionId,
        owner: NodeIdentity,
        claim_epoch: ClaimEpoch,
        principal: AuthenticatedPrincipalRef,
        target: NormalizedTarget,
    }

    impl ShadowFixture {
        async fn open(test_name: &str) -> Option<Self> {
            let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
                eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (ingress shadow)");
                return None;
            };
            let schema = format!(
                "waddle_test_ingress_shadow_{test_name}_{}",
                Uuid::new_v4().simple()
            );
            let admin = sqlx::PgPool::connect(&database_url)
                .await
                .expect("connect PostgreSQL admin pool");
            sqlx::query(&format!("CREATE SCHEMA {schema}"))
                .execute(&admin)
                .await
                .expect("create isolated PostgreSQL schema");
            let schema_url = postgres_url_with_search_path(&database_url, &schema);
            let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, schema_url);
            config.pool_size = 8;
            let db = Database::from_config("ingress-shadow-test", &config)
                .await
                .expect("open isolated PostgreSQL database");
            MigrationRunner::single()
                .run(&db)
                .await
                .expect("apply migrations");
            crate::clustering::claims::PostgresClaimStore::new(db.clone())
                .ensure_schema()
                .await
                .expect("initialize claims schema");
            crate::sm_persistence::DatabaseSmPersistence::open(Some(db.database_url()))
                .await
                .expect("initialize SM persistence schema");

            let lineage = LineageConfig {
                deployment_uuid: Some(
                    "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001"
                        .parse()
                        .expect("valid fixture lineage UUID"),
                ),
                action: None,
            };
            lineage::enroll(&db, &lineage)
                .await
                .expect("enroll fixture lineage");

            let stream_id = SmSessionId::new(format!("shadow-stream-{}", Uuid::new_v4().simple()));
            let owner = NodeIdentity::new("shadow-node", "shadow-incarnation");
            let claim_epoch = ClaimEpoch(17);
            let principal = AuthenticatedPrincipalRef::new(
                "romeo@example.com".parse().expect("fixture bare JID"),
                AuthContextId::new(Uuid::new_v4()),
                AuthContextVersion::new(3),
                PrincipalAuthEpoch::new(5),
            );
            let target =
                NormalizedTarget::Bare("juliet@example.com".parse().expect("fixture target"));
            let node_identity = SharedNodeIdentity::new(owner.clone());
            let processor = IngressShadowProcessor {
                database: db.clone(),
                lineage,
                node_identity,
                retry_attempts: 5,
                forced_alias_serialization_failures: Arc::new(AtomicUsize::new(0)),
            };
            let fixture = Self {
                db,
                processor,
                admin,
                schema,
                stream_id,
                owner,
                claim_epoch,
                principal,
                target,
            };
            fixture.seed_principal_and_claim().await;
            fixture
                .processor
                .execute_enrollment(&fixture.stream_id)
                .await
                .expect("enroll fresh SM stream");
            Some(fixture)
        }

        async fn seed_principal_and_claim(&self) {
            let suffix = Uuid::new_v4().simple().to_string();
            self.execute(
                "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, now(), now())",
                crate::db_params![
                    self.principal.bare_jid().to_string(),
                    format!("shadow-{suffix}"),
                    format!("shadow-{suffix}"),
                ],
            )
            .await
            .expect("seed fixture user");
            self.execute(
                "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, now(), now())",
                crate::db_params![
                    format!("shadow-session-{suffix}"),
                    self.principal.bare_jid().to_string(),
                    format!("shadow-token-{suffix}"),
                    self.principal.auth_context_id().as_uuid().to_string(),
                    i64::try_from(self.principal.auth_context_version().get()).expect("version fits"),
                    i64::try_from(self.principal.auth_epoch().get()).expect("epoch fits"),
                ],
            )
            .await
            .expect("seed fixture authenticated session");
            self.execute(
                "INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    format!("sm_session:{}", self.stream_id.as_str()),
                    "sm_session".to_string(),
                    self.owner.node_id.clone(),
                    self.owner.node_epoch.clone(),
                    self.claim_epoch.0,
                ],
            )
            .await
            .expect("seed exact SM claim");
        }

        fn submission(&self, ordinal: u64, origin: Option<&str>) -> IngressShadowSubmission {
            let mut message = Message::new(Some(Jid::from(
                "juliet@example.com"
                    .parse::<BareJid>()
                    .expect("fixture target"),
            )));
            message.type_ = MessageType::Chat;
            if let Some(origin) = origin {
                waddle_xmpp_core::xep0359::add_origin_id(&mut message, origin);
            }
            IngressShadowSubmission {
                stream_id: self.stream_id.clone(),
                owner: self.owner.clone(),
                claim_epoch: self.claim_epoch,
                handled_ordinal: IngressOrdinal::from_storage(ordinal).expect("positive ordinal"),
                principal: self.principal.clone(),
                target: self.target.clone(),
                message,
                capture: IngressEffectCaptureSnapshot {
                    stanza_lang: None,
                    sanitized_message: None,
                    room_fence: None,
                    intents: Vec::new(),
                    markers: Vec::new(),
                },
                connection_generation: Some(ConnectionGeneration::INITIAL),
            }
        }

        fn submission_with_inbox_intent(
            &self,
            ordinal: u64,
            origin: Option<&str>,
        ) -> IngressShadowSubmission {
            let mut submission = self.submission(ordinal, origin);
            submission.capture.intents.push(
                waddle_xmpp::ingress::IngressEffectIntent::InboxProject {
                    owner: self.principal.bare_jid().clone(),
                    increment_unread: true,
                },
            );
            submission
        }

        async fn execute(
            &self,
            sql: &str,
            params: impl IntoParams,
        ) -> Result<u64, crate::db::DatabaseError> {
            let conn = self.db.guard().await?;
            conn.execute(sql, params).await
        }

        async fn count(&self, table: &str) -> i64 {
            let conn = self.db.guard().await.expect("database connection");
            let mut rows = conn
                .query(&format!("SELECT COUNT(*) FROM {table}"), ())
                .await
                .expect("count rows");
            rows.next()
                .await
                .expect("read count")
                .expect("count row")
                .get(0)
                .expect("decode count")
        }

        async fn frontier(&self) -> u64 {
            let conn = self.db.guard().await.expect("database connection");
            let mut rows = conn
                .query(
                    "SELECT handled_ordinal::text FROM ingress_sm_streams WHERE stream_id = ?",
                    crate::db_params![self.stream_id.as_str().to_string()],
                )
                .await
                .expect("read shadow frontier");
            rows.next()
                .await
                .expect("read frontier")
                .expect("enrolled stream")
                .get::<String>(0)
                .expect("decode frontier")
                .parse()
                .expect("frontier is u64")
        }

        async fn assert_rows(&self, messages: i64, aliases: i64, refs: i64, intents: i64) {
            assert_eq!(self.count("ingress_messages").await, messages, "messages");
            assert_eq!(
                self.count("ingress_origin_aliases").await,
                aliases,
                "aliases"
            );
            assert_eq!(self.count("ingress_sm_refs").await, refs, "SM refs");
            assert_eq!(
                self.count("ingress_effect_intents").await,
                intents,
                "effect intents"
            );
        }

        async fn close(self) {
            let Self {
                db, admin, schema, ..
            } = self;
            drop(db);
            sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
                .execute(&admin)
                .await
                .expect("drop isolated PostgreSQL schema");
        }
    }

    fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
        let mut url = url::Url::parse(database_url).expect("parse PostgreSQL URL");
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
            .append_pair("options", &format!("-c search_path={schema}"));
        url.to_string()
    }

    #[test]
    fn rowless_marker_prefers_authorization_denied() {
        let mut message = Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        message.type_ = MessageType::Groupchat;
        let mut submission = base_submission(message);
        submission.capture.markers = vec![
            ShadowDecisionMarker::SemanticRejected {
                reason: ShadowSemanticRejectedReason::MalformedPayload,
            },
            ShadowDecisionMarker::AuthorizationDenied {
                reason: ShadowAuthorizationDeniedReason::Forbidden,
            },
        ];

        assert_eq!(
            submission.rowless_decision_marker(),
            Some(IngressShadowDecisionClass::AuthorizationDenied)
        );
    }

    #[test]
    fn rowless_marker_promotes_capture_overflow_to_non_success() {
        let mut message = Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        message.type_ = xmpp_parsers::message::MessageType::Groupchat;
        let mut submission = base_submission(message);
        submission.capture.markers = vec![
            ShadowDecisionMarker::AuthorizationDenied {
                reason: ShadowAuthorizationDeniedReason::Forbidden,
            },
            ShadowDecisionMarker::Overflow,
        ];

        assert_eq!(
            submission.rowless_decision_marker(),
            Some(IngressShadowDecisionClass::CaptureOverflow)
        );
    }

    #[test]
    fn groupchat_submission_includes_room_in_server_authorities() {
        let mut message = Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        message.type_ = MessageType::Groupchat;
        let mut submission = base_submission(message);
        submission.capture.room_fence = Some(IngressShadowRoomFence {
            room: "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
            owner: NodeIdentity::new("room-node", "room-epoch"),
            claim_epoch: ClaimEpoch(11),
        });

        let authorities = submission.server_authorities();
        assert!(authorities.contains(submission.principal.bare_jid()));
        assert!(authorities.contains(
            &"room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid")
        ));
    }

    #[test]
    fn occupant_pm_submission_uses_room_fence_for_room_authorities() {
        let mut message = Message::new(Some(Jid::from(
            "room@conference.example.com/alice"
                .parse::<jid::FullJid>()
                .expect("occupant jid"),
        )));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        let mut submission = base_submission(message);
        submission.capture.room_fence = Some(IngressShadowRoomFence {
            room: "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
            owner: NodeIdentity::new("room-node", "room-epoch"),
            claim_epoch: ClaimEpoch(13),
        });

        assert_eq!(
            submission
                .room_claim_target()
                .expect("room fence should define room scope")
                .claim_epoch,
            ClaimEpoch(13)
        );
        assert!(submission.server_authorities().contains(
            &"room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid")
        ));
    }

    #[test]
    fn digest_input_strips_forged_server_stanza_ids_and_retries() {
        let mut message = Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        message.type_ = MessageType::Groupchat;
        message
            .payloads
            .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
                "forged",
                &Jid::from(
                    "room@conference.example.com"
                        .parse::<BareJid>()
                        .expect("room jid"),
                ),
            ));
        let submission = base_submission(message);

        let digest_input = digest_input_from_submission(&submission)
            .expect("digest evaluation succeeds")
            .expect("digest input is valid after stripping");
        assert_eq!(
            digest_input.stanza_lang(),
            Some(&xmpp_parsers::message::Lang::from("en"))
        );
    }

    #[test]
    fn strip_stanza_id_for_authority_removes_only_matching_authority() {
        let mut message = Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        message.type_ = MessageType::Chat;
        message
            .payloads
            .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
                "sender",
                &Jid::from("romeo@example.com".parse::<BareJid>().expect("sender jid")),
            ));
        message
            .payloads
            .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
                "other",
                &Jid::from("juliet@example.com".parse::<BareJid>().expect("other jid")),
            ));

        strip_stanza_id_for_authority(
            &mut message,
            &"romeo@example.com".parse::<BareJid>().expect("sender jid"),
        );

        let remaining = message
            .payloads
            .iter()
            .filter(|payload| payload.name() == "stanza-id")
            .count();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn capture_snapshot_supports_archive_stanza_ids() {
        let snapshot = IngressEffectCaptureSnapshot {
            stanza_lang: Some(xmpp_parsers::message::Lang::from("en")),
            sanitized_message: None,
            room_fence: None,
            intents: vec![
                waddle_xmpp::ingress::IngressEffectIntent::ArchiveAuthoritative {
                    archive: "romeo@example.com".parse().expect("bare jid"),
                    stanza_id: StanzaId::new(
                        "sid-1",
                        Jid::from("romeo@example.com".parse::<BareJid>().expect("bare jid")),
                    ),
                    by: "romeo@example.com".parse().expect("bare jid"),
                },
            ],
            markers: Vec::new(),
        };
        assert_eq!(snapshot.intents.len(), 1);
    }

    #[tokio::test]
    async fn postgres_decision_matrix_preserves_exact_shadow_rows_and_frontier() {
        let Some(fixture) = ShadowFixture::open("decision_matrix").await else {
            return;
        };

        let accepted = fixture.submission(1, Some("same-origin"));
        assert!(matches!(
            fixture.processor.execute_submission(&accepted).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::Accepted,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Inserted,
            })
        ));
        assert_eq!(fixture.frontier().await, 1);
        fixture.assert_rows(1, 1, 1, 0).await;

        let existing = fixture.submission(2, Some("same-origin"));
        assert!(matches!(
            fixture.processor.execute_submission(&existing).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::ExistingSameDigest,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Existing,
            })
        ));
        assert_eq!(fixture.frontier().await, 2);
        fixture.assert_rows(1, 1, 2, 0).await;

        let mut conflict = fixture.submission(3, Some("same-origin"));
        conflict
            .message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), "different".to_string());
        assert!(matches!(
            fixture.processor.execute_submission(&conflict).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::AliasConflict,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Conflict,
            })
        ));
        assert_eq!(fixture.frontier().await, 3);
        fixture.assert_rows(1, 1, 2, 0).await;

        let mut malformed = fixture.submission(4, None);
        malformed
            .message
            .payloads
            .push(waddle_xmpp_core::xep0359::build_origin_id_element("one"));
        malformed
            .message
            .payloads
            .push(waddle_xmpp_core::xep0359::build_origin_id_element("two"));
        assert!(matches!(
            fixture.processor.execute_submission(&malformed).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::SemanticMalformed,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 4);
        fixture.assert_rows(1, 1, 2, 0).await;

        let mut denied = fixture.submission(5, None);
        denied
            .capture
            .markers
            .push(ShadowDecisionMarker::AuthorizationDenied {
                reason: ShadowAuthorizationDeniedReason::Forbidden,
            });
        assert!(matches!(
            fixture.processor.execute_submission(&denied).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::AuthorizationDenied,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 5);
        fixture.assert_rows(1, 1, 2, 0).await;
        fixture.close().await;
    }

    #[tokio::test]
    async fn postgres_fence_principal_and_storage_failures_leave_shadow_unchanged() {
        let Some(fixture) = ShadowFixture::open("rollbacks").await else {
            return;
        };
        let mut bad_principal = fixture.submission(1, None);
        bad_principal.principal = AuthenticatedPrincipalRef::new(
            fixture.principal.bare_jid().clone(),
            fixture.principal.auth_context_id().clone(),
            fixture.principal.auth_context_version(),
            PrincipalAuthEpoch::new(fixture.principal.auth_epoch().get() + 1),
        );
        assert!(matches!(
            fixture.processor.execute_submission(&bad_principal).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::PrincipalMissing,
                commit_kind: None,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 0);
        assert_eq!(fixture.count("ingress_messages").await, 0);
        assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
        assert_eq!(fixture.count("ingress_sm_refs").await, 0);

        let mut bad_fence = fixture.submission(1, None);
        bad_fence.claim_epoch = ClaimEpoch(fixture.claim_epoch.0 + 1);
        assert!(matches!(
            fixture.processor.execute_submission(&bad_fence).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::ClaimFenceMissing,
                commit_kind: None,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 0);
        fixture.assert_rows(0, 0, 0, 0).await;

        let mut overflowed = fixture.submission(1, None);
        overflowed
            .capture
            .markers
            .push(ShadowDecisionMarker::Overflow);
        assert!(matches!(
            fixture.processor.execute_submission(&overflowed).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::CaptureOverflow,
                commit_kind: None,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 0);
        fixture.assert_rows(0, 0, 0, 0).await;

        // A per-run schema lets this test poison only the shadow dependency;
        // the processor must roll the transaction back without advancing h.
        fixture
            .execute("DROP TABLE ingress_effect_intents", ())
            .await
            .expect("poison shadow effect table");
        assert!(fixture
            .processor
            .execute_submission(&fixture.submission_with_inbox_intent(1, None))
            .await
            .is_err());
        assert_eq!(fixture.frontier().await, 0);
        assert_eq!(fixture.count("ingress_messages").await, 0);
        assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
        // The poisoned ingress_effect_intents table no longer exists, so the
        // surviving tables plus the unmoved frontier carry the rollback proof.
        assert_eq!(fixture.count("ingress_sm_refs").await, 0);
        fixture.close().await;
    }

    #[tokio::test]
    async fn postgres_serialization_exhaustion_rolls_back_every_attempt() {
        let Some(fixture) = ShadowFixture::open("serialization_exhaustion").await else {
            return;
        };
        let processor = fixture.processor.clone();
        let submission = fixture.submission(1, Some("retry-origin"));
        fixture
            .processor
            .forced_alias_serialization_failures
            .store(5, Ordering::SeqCst);
        let result = run_with_retry(5, || {
            let processor = processor.clone();
            let submission = submission.clone();
            async move { processor.execute_submission(&submission).await }
        })
        .await;
        assert!(matches!(
            result,
            Err(crate::ingress_uow::RetryExhausted {
                attempts: 5,
                last_error: IngressUowError::Database {
                    retry_class: crate::ingress_uow::DbRetryClass::SerializationFailure,
                },
            })
        ));
        assert_eq!(fixture.frontier().await, 0);
        fixture.assert_rows(0, 0, 0, 0).await;
        fixture.close().await;
    }

    #[tokio::test]
    async fn dropping_last_handle_joins_background_tasks_and_closes_dedicated_pool() {
        let (pool_closed_tx, pool_closed_rx) = oneshot::channel();
        let pool = Arc::new(PoolCloseSignal(Some(pool_closed_tx)));
        let handle = test_handle(1, 1, {
            let pool = pool.clone();
            move |_| {
                let pool = pool.clone();
                Box::pin(async move {
                    drop(pool);
                })
            }
        });
        let shutdown = handle
            .shutdown()
            .expect("worker should expose shutdown state");
        let handle_clone = handle.clone();
        drop(pool);
        drop(handle);
        assert!(
            !shutdown.complete.load(Ordering::Acquire),
            "a remaining handle clone must keep the worker alive"
        );

        drop(handle_clone);
        tokio::time::timeout(Duration::from_millis(250), shutdown.wait_for_completion())
            .await
            .expect("retry and scheduler tasks should join after the last handle drops");
        tokio::time::timeout(Duration::from_millis(250), pool_closed_rx)
            .await
            .expect("scheduler completion should release the dedicated pool")
            .expect("dedicated pool closure should be signalled");
    }

    #[tokio::test]
    async fn postgres_claim_lock_timeout_keeps_enqueue_prompt() {
        let Some(fixture) = ShadowFixture::open("claim_lock_timeout").await else {
            return;
        };
        let mut locker = sqlx::PgConnection::connect(fixture.db.database_url())
            .await
            .expect("open competing PostgreSQL connection");
        sqlx::query("BEGIN")
            .execute(&mut locker)
            .await
            .expect("start competing transaction");
        let claim_entity = format!("sm_session:{}", fixture.stream_id.as_str());
        let locked =
            sqlx::query("UPDATE clustering_claims SET claim_epoch = claim_epoch WHERE entity = $1")
                .bind(claim_entity)
                .execute(&mut locker)
                .await
                .expect("lock exact claim row");
        assert_eq!(
            locked.rows_affected(),
            1,
            "fixture should lock one claim row"
        );

        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (finished_tx, mut finished_rx) = mpsc::unbounded_channel();
        let processor = fixture.processor.clone();
        let handle = test_handle(4, 1, move |task| {
            let processor = processor.clone();
            let started_tx = started_tx.clone();
            let finished_tx = finished_tx.clone();
            Box::pin(async move {
                started_tx.send(()).expect("record worker start");
                processor.execute(task).await;
                finished_tx.send(()).expect("record worker finish");
            })
        });

        let enqueue_started = std::time::Instant::now();
        let disposition = handle.try_submit(fixture.submission(1, Some("lock-timeout-origin")));
        let enqueue_elapsed = enqueue_started.elapsed();
        assert_eq!(disposition, IngressShadowDisposition::Enqueued);
        assert!(
            enqueue_elapsed < Duration::from_millis(50),
            "enqueue should remain prompt while the worker blocks on PostgreSQL row locks, took {enqueue_elapsed:?}"
        );

        tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("shadow worker should start promptly")
            .expect("worker start recorded");
        wait_for_lock_waiter(
            &fixture.admin,
            "SELECT 1 FROM clustering_claims WHERE entity =",
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), finished_rx.recv())
                .await
                .is_err(),
            "worker should still be waiting on the locked exact-claim row"
        );
        tokio::time::timeout(Duration::from_secs(1), finished_rx.recv())
            .await
            .expect("lock_timeout should end the blocked shadow attempt")
            .expect("worker finish recorded");
        assert_eq!(fixture.frontier().await, 0);
        fixture.assert_rows(0, 0, 0, 0).await;

        sqlx::query("ROLLBACK")
            .execute(&mut locker)
            .await
            .expect("release competing claim lock");
        fixture.close().await;
    }

    #[tokio::test]
    async fn postgres_retransmit_is_idempotent_and_dropped_ordinal_exposes_gap() {
        let Some(fixture) = ShadowFixture::open("replay_gap").await else {
            return;
        };
        let first = fixture.submission(1, Some("replay-origin"));
        assert!(matches!(
            fixture.processor.execute_submission(&first).await,
            Ok(ShadowSubmissionOutcome {
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                ..
            })
        ));
        assert!(matches!(
            fixture.processor.execute_submission(&first).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::ExistingSameDigest,
                commit_kind: Some(IngressShadowCommitKind::Idempotent),
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 1);
        fixture.assert_rows(1, 1, 1, 0).await;

        // This models the production crash/overflow gap: ordinal 2 was
        // allocated at contiguous drain, but the bounded executor rejects it
        // before a worker can run its transaction. The allocation is not
        // reusable, so the next submitted ordinal must expose the gap.
        let dropped = fixture.submission(2, Some("dropped-allocated-ordinal"));
        let dropped_handle = test_handle(0, 1, |_| Box::pin(async {}));
        assert_eq!(
            dropped_handle.try_submit(dropped),
            IngressShadowDisposition::QueueFull,
            "allocated ordinal must be dropped before execution"
        );

        // Ordinal 3 must surface the durable observation gap.
        let stale = fixture.submission(3, Some("after-dropped-ordinal"));
        assert!(matches!(
            fixture.processor.execute_submission(&stale).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::FrontierStale,
                commit_kind: None,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 1);
        fixture.assert_rows(1, 1, 1, 0).await;
        fixture.close().await;
    }

    #[tokio::test]
    async fn blocked_stream_does_not_starve_other_streams() {
        let release_stream_a = Arc::new(Notify::new());
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (finished_tx, mut finished_rx) = mpsc::unbounded_channel();
        let handle = test_handle(4, 2, {
            let release_stream_a = release_stream_a.clone();
            move |task| {
                let release_stream_a = release_stream_a.clone();
                let started_tx = started_tx.clone();
                let finished_tx = finished_tx.clone();
                Box::pin(async move {
                    let stream_id = task.stream_id().clone();
                    started_tx
                        .send(stream_id.clone())
                        .expect("record started stream");
                    if stream_id.as_str() == "stream-a" {
                        release_stream_a.notified().await;
                    }
                    finished_tx.send(stream_id).expect("record finished stream");
                })
            }
        });
        let stream_a = SmSessionId::new("stream-a");
        let stream_b = SmSessionId::new("stream-b");

        assert_eq!(
            handle.try_enroll_stream(stream_a.clone()),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_enroll_stream(stream_b.clone()),
            IngressShadowDisposition::Enqueued
        );

        let started_first = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("first start should not block")
            .expect("first start recorded");
        let started_second = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("second start should not block on stream-a")
            .expect("second start recorded");
        assert!(started_first == stream_a || started_second == stream_a);
        assert!(started_first == stream_b || started_second == stream_b);

        let finished_first = tokio::time::timeout(Duration::from_millis(250), finished_rx.recv())
            .await
            .expect("unblocked stream should still finish")
            .expect("finished stream recorded");
        assert_eq!(
            finished_first, stream_b,
            "stream-b should complete while stream-a remains blocked"
        );

        release_stream_a.notify_waiters();
        let finished_second = tokio::time::timeout(Duration::from_millis(250), finished_rx.recv())
            .await
            .expect("blocked stream should finish after release")
            .expect("finished stream recorded");
        assert_eq!(finished_second, stream_a);
    }

    #[tokio::test]
    async fn queue_full_keeps_same_stream_fifo_order() {
        let release_first = Arc::new(Notify::new());
        let started_order = Arc::new(AtomicUsize::new(0));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let handle = test_handle(2, 2, {
            let release_first = release_first.clone();
            let started_order = started_order.clone();
            move |_task| {
                let release_first = release_first.clone();
                let started_order = started_order.clone();
                let started_tx = started_tx.clone();
                Box::pin(async move {
                    let order = started_order.fetch_add(1, Ordering::SeqCst);
                    started_tx.send(order).expect("record start order");
                    if order == 0 {
                        release_first.notified().await;
                    }
                })
            }
        });
        let stream_a = SmSessionId::new("stream-a");

        assert_eq!(
            handle.try_enroll_stream(stream_a.clone()),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_enroll_stream(stream_a.clone()),
            IngressShadowDisposition::Enqueued
        );
        // The third enrollment consumes the single reserved enrollment
        // admission slot; only the fourth sees a genuinely full queue.
        assert_eq!(
            handle.try_enroll_stream(stream_a.clone()),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_enroll_stream(stream_a),
            IngressShadowDisposition::QueueFull
        );

        let first_started = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("first task should start")
            .expect("first start order recorded");
        assert_eq!(first_started, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), started_rx.recv())
                .await
                .is_err(),
            "same-stream work must remain queued behind the blocked first task"
        );

        release_first.notify_waiters();
        let second_started = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("second task should start after the first completes")
            .expect("second start order recorded");
        assert_eq!(second_started, 1);
    }

    #[tokio::test]
    async fn reserved_enrollment_admission_survives_a_saturated_submission_queue() {
        let hold_submission = Arc::new(Notify::new());
        let handle = test_handle(1, 1, {
            let hold_submission = hold_submission.clone();
            move |task| {
                let hold_submission = hold_submission.clone();
                Box::pin(async move {
                    if matches!(task, IngressShadowTask::Submit(_)) {
                        hold_submission.notified().await;
                    }
                })
            }
        });
        let message = Message::new(Some(jid::Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));

        assert_eq!(
            handle.try_submit(base_submission(message)),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_enroll_stream(SmSessionId::new("fresh-sm-stream")),
            IngressShadowDisposition::Enqueued,
            "fresh SM enables retain one reserved admission slot when submit work fills the queue"
        );

        hold_submission.notify_waiters();
    }

    #[tokio::test]
    async fn second_fresh_enable_eventually_enrolls_after_reserved_permit_is_held_long() {
        let hold_submission = Arc::new(Notify::new());
        let hold_stream_a = Arc::new(Notify::new());
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let handle = test_handle(1, 1, {
            let hold_submission = hold_submission.clone();
            let hold_stream_a = hold_stream_a.clone();
            move |task| {
                let hold_submission = hold_submission.clone();
                let hold_stream_a = hold_stream_a.clone();
                let started_tx = started_tx.clone();
                Box::pin(async move {
                    match task {
                        IngressShadowTask::Submit(_) => {
                            started_tx
                                .send("submit".to_string())
                                .expect("record submit start");
                            hold_submission.notified().await;
                        }
                        IngressShadowTask::Enroll { stream_id } => {
                            started_tx
                                .send(stream_id.to_string())
                                .expect("record enroll start");
                            if stream_id.as_str() == "stream-a" {
                                hold_stream_a.notified().await;
                            }
                        }
                    }
                })
            }
        });
        let message = Message::new(Some(jid::Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        let stream_a = SmSessionId::new("stream-a");
        let stream_b = SmSessionId::new("stream-b");

        assert_eq!(
            handle.try_submit(base_submission(message)),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.ensure_stream_enrollment(stream_a.clone()),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.ensure_stream_enrollment(stream_b.clone()),
            IngressShadowDisposition::QueueFull,
            "second fresh enable should start as pending while the reserved permit is occupied"
        );

        let started_submit = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("submission should start promptly")
            .expect("submission start recorded");
        assert_eq!(started_submit, "submit");

        hold_submission.notify_waiters();
        let started_stream_a = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("first fresh enable should eventually start")
            .expect("stream-a start recorded");
        assert_eq!(started_stream_a, stream_a.to_string());
        assert!(
            tokio::time::timeout(Duration::from_millis(75), started_rx.recv())
                .await
                .is_err(),
            "second fresh enable must remain pending while stream-a still holds the reserved permit"
        );

        hold_stream_a.notify_waiters();
        let started_stream_b = tokio::time::timeout(Duration::from_millis(500), started_rx.recv())
            .await
            .expect("second fresh enable should eventually start once the permit is released")
            .expect("stream-b start recorded");
        assert_eq!(started_stream_b, stream_b.to_string());
    }

    async fn wait_for_lock_waiter(admin: &sqlx::PgPool, fragment: &str) {
        for _ in 0..400 {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query LIKE $1",
            )
            .bind(format!("%{fragment}%"))
            .fetch_one(admin)
            .await
            .expect("poll pg_stat_activity for a lock waiter");
            if waiting > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no blocked backend appeared for query fragment {fragment:?}");
    }
}
