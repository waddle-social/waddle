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
use crate::ingress_substrate::{
    AliasGcBudget, AliasGcError, AliasGcFailure, AliasGcOutcome, AliasGcProgress,
    PostgresIngressSubstrate,
};
#[cfg(feature = "clustering")]
use crate::ingress_uow::{
    run_with_retry, CanonicalMessageRepository, ClaimRepository, EffectIntentRepository,
    EffectIntentWriteOutcome, IngressUowError, PostgresIngressUnitOfWork, PrincipalAssertion,
    PrincipalRepository, ShadowFrontierOutcome, SmIngressRepository, SmIngressStreamRepository,
};
#[cfg(feature = "clustering")]
use chrono::Utc;
#[cfg(feature = "clustering")]
use jid::BareJid;
#[cfg(feature = "clustering")]
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
#[cfg(feature = "clustering")]
use std::future::Future;
#[cfg(feature = "clustering")]
use std::pin::Pin;
#[cfg(feature = "clustering")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "clustering")]
use std::time::Instant;
use thiserror::Error;
#[cfg(feature = "clustering")]
use tokio::sync::mpsc;
#[cfg(feature = "clustering")]
use tokio::sync::Notify;
#[cfg(feature = "clustering")]
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
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
const RETENTION_GC_BUDGET: Duration = Duration::from_secs(2);
/// Last-resort envelope around one GC run, sized from the longest path the
/// per-operation bounds allow after the final cooperative check: one scan
/// (`RETENTION_GC_SCAN_TIMEOUT`), then one candidate transaction — the epoch
/// lock wait plus the nine single-row statements it issues — and margin, so
/// slowness inside the bounds is classified by the run itself rather than
/// cancelled from outside.  2 s + 1 s + 0.25 s + 9 × 0.25 s ≈ 5.5 s.
#[cfg(feature = "clustering")]
const RETENTION_GC_HARD_DEADLINE: Duration = Duration::from_secs(6);
#[cfg(feature = "clustering")]
const RETENTION_GC_LOCK_TIMEOUT: Duration = Duration::from_millis(250);
/// Every candidate-transaction statement touches one row by primary key.
#[cfg(feature = "clustering")]
const RETENTION_GC_STATEMENT_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(feature = "clustering")]
const RETENTION_GC_SCAN_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(feature = "clustering")]
const DEFAULT_RETIREMENT_ADMISSION_RETRY_DELAY: Duration = Duration::from_millis(10);

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

#[cfg(all(test, feature = "clustering"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IngressShadowTestTaskKind {
    Enroll,
    Submit,
    Retire,
}

#[derive(Debug, Error)]
pub enum IngressShadowStartupError {
    #[error("WADDLE_INGRESS_SHADOW_ENABLED=true requires a PostgreSQL global database")]
    PostgresRequired,
    #[error("WADDLE_INGRESS_SHADOW_ENABLED=true requires a clustering-enabled binary")]
    ClusteringFeatureRequired,
    #[error("WADDLE_INGRESS_SHADOW_ENABLED=true requires a live clustering node identity")]
    NodeIdentityRequired,
    #[error(
        "WADDLE_INGRESS_SHADOW_ENABLED=true could not open the dedicated shadow database pool"
    )]
    DedicatedPoolOpen(#[source] crate::db::DatabaseError),
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
        tx: IngressShadowTx,
        enqueued_streams: Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
        retiring_streams: Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
        retirement_retry_dispatcher: RetirementRetryDispatcher,
        stream_activity: Arc<std::sync::Mutex<StreamActivityState>>,
        submission_capacity: Arc<Semaphore>,
        enrollment_capacity: Arc<Semaphore>,
        retirement_capacity: usize,
        shutdown: Arc<IngressShadowShutdown>,
    },
}

#[cfg(feature = "clustering")]
type IngressShadowTx =
    Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<QueuedIngressShadowTask>>>>;

#[cfg(feature = "clustering")]
#[derive(Debug, Default)]
struct IngressShadowShutdown {
    cancellation: CancellationToken,
    force_stop: CancellationToken,
    active_task_aborts: std::sync::Mutex<Vec<ActiveShadowTask>>,
    complete: AtomicBool,
    complete_notify: Notify,
}

/// Bound on waiting, after `force_stop`, for every admitted submission's
/// obligation to be released (`AbortHandle::abort` only schedules the
/// cancellation; queued tasks drop when the scheduler loop exits). The HTTP
/// shutdown coordinator extends its drain wait by this margin so the final
/// metrics flush never starts while this barrier is still recording aborts.
pub const FORCED_TEARDOWN_JOIN: Duration = Duration::from_millis(250);
#[cfg(feature = "clustering")]
const FORCED_TEARDOWN_POLL: Duration = Duration::from_millis(5);

#[cfg(feature = "clustering")]
impl IngressShadowShutdown {
    fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// End the scheduler at the graceful-shutdown deadline. Queued work is
    /// dropped and active, still-uncommitted attempts are aborted rather than
    /// being left to an imminent runtime teardown.
    fn force_stop(&self) {
        self.force_stop.cancel();
        for task in self
            .active_task_aborts
            .lock()
            .expect("shadow active task abort handles must not be poisoned")
            .drain(..)
        {
            task.abort.abort();
        }
    }

    fn track_active_task(&self, finished: Arc<AtomicBool>, abort: tokio::task::AbortHandle) {
        let mut tasks = self
            .active_task_aborts
            .lock()
            .expect("shadow active task abort handles must not be poisoned");
        if self.force_stop.is_cancelled() {
            abort.abort();
            return;
        }
        if !finished.load(Ordering::Acquire) {
            tasks.push(ActiveShadowTask { finished, abort });
        }
    }

    fn finish_active_task(&self, finished: &Arc<AtomicBool>) {
        finished.store(true, Ordering::Release);
        self.active_task_aborts
            .lock()
            .expect("shadow active task abort handles must not be poisoned")
            .retain(|task| !Arc::ptr_eq(&task.finished, finished));
    }

    fn mark_complete(&self) {
        self.complete.store(true, Ordering::Release);
        self.complete_notify.notify_waiters();
    }

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
#[derive(Debug)]
struct ActiveShadowTask {
    finished: Arc<AtomicBool>,
    abort: tokio::task::AbortHandle,
}

#[cfg(feature = "clustering")]
impl Drop for IngressShadowInner {
    fn drop(&mut self) {
        if let Self::Worker { tx, shutdown, .. } = self {
            shutdown.cancel();
            // The scheduler's executor retains a processor clone, and that
            // processor retains this sender.  Cancellation alone therefore
            // cannot close `rx` when the final public handle is dropped.
            close_worker_intake(tx);
        }
    }
}

#[derive(Debug)]
enum IngressShadowTask {
    Enroll { stream_id: SmSessionId },
    Submit(Box<IngressShadowSubmission>),
    Retire { stream_id: SmSessionId },
}

/// Result of terminal stream retirement.  A live claim is not an absent
/// stream: its deferred release must be observed before the shadow rows can
/// be retired.
#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetirementOutcome {
    Deleted,
    StreamMissing,
    DeferredClaim,
}

#[cfg(feature = "clustering")]
#[derive(Debug, Default)]
struct RetirementRetryState {
    queued: VecDeque<SmSessionId>,
    queued_members: HashSet<SmSessionId>,
    scan_requested: bool,
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone)]
struct RetirementRetryDispatcher {
    state: Arc<std::sync::Mutex<RetirementRetryState>>,
    notify: Arc<Notify>,
    capacity: usize,
}

#[cfg(feature = "clustering")]
#[derive(Debug, Default)]
struct StreamActivityState {
    pending: HashMap<SmSessionId, usize>,
    idle_waiters: HashMap<SmSessionId, Arc<Notify>>,
    outstanding: BTreeMap<u64, Instant>,
    next_seq: u64,
}

#[cfg(feature = "clustering")]
struct QueuedIngressShadowTask {
    task: IngressShadowTask,
    permit: Option<OwnedSemaphorePermit>,
    outstanding: Option<OutstandingSubmission>,
}

/// An admitted submission's obligation: registered before the task is
/// handed to the worker and released exactly once — by the terminal
/// `Decision` observation, or as `Aborted` if the task is dropped without
/// one (forced shutdown discarding queued or in-flight work).
#[cfg(feature = "clustering")]
struct OutstandingSubmission {
    stream_activity: Arc<std::sync::Mutex<StreamActivityState>>,
    stream_id: SmSessionId,
    seq: u64,
    armed: bool,
}

#[cfg(feature = "clustering")]
impl Drop for OutstandingSubmission {
    fn drop(&mut self) {
        if self.armed {
            finish_outstanding(
                &self.stream_activity,
                &self.stream_id,
                self.seq,
                OutstandingEnd::Aborted,
            );
        }
    }
}

#[cfg(feature = "clustering")]
fn stream_activity_lock(
    stream_activity: &std::sync::Mutex<StreamActivityState>,
) -> std::sync::MutexGuard<'_, StreamActivityState> {
    stream_activity
        .lock()
        .expect("stream activity mutex must not be poisoned")
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutstandingEnd {
    /// The worker produced the submission's terminal decision.
    Decision,
    /// The task was dropped without a decision (forced shutdown, intake
    /// closed under a queued task).
    Aborted,
}

#[cfg(feature = "clustering")]
struct IngressShadowTelemetryState {
    enabled: AtomicBool,
    stream_activity: std::sync::Mutex<Option<Arc<std::sync::Mutex<StreamActivityState>>>>,
}

#[cfg(feature = "clustering")]
impl IngressShadowTelemetryState {
    fn set(
        &self,
        enabled: bool,
        stream_activity: Option<Arc<std::sync::Mutex<StreamActivityState>>>,
    ) {
        self.enabled.store(enabled, Ordering::Release);
        *self
            .stream_activity
            .lock()
            .expect("ingress shadow telemetry state mutex must not be poisoned") = stream_activity;
    }

    fn oldest_outstanding_submission_age_seconds(&self) -> f64 {
        let Some(stream_activity) = self
            .stream_activity
            .lock()
            .expect("ingress shadow telemetry state mutex must not be poisoned")
            .clone()
        else {
            return 0.0;
        };
        let oldest = stream_activity_lock(&stream_activity)
            .outstanding
            .values()
            .next()
            .map_or(0.0, |enqueued_at| enqueued_at.elapsed().as_secs_f64());
        oldest
    }
}

#[cfg(feature = "clustering")]
fn init_ingress_shadow_instruments(
    enabled: bool,
    stream_activity: Option<Arc<std::sync::Mutex<StreamActivityState>>>,
) {
    struct IngressShadowInstruments {
        _enabled: opentelemetry::metrics::ObservableGauge<i64>,
        _oldest_outstanding_submission_age: opentelemetry::metrics::ObservableGauge<f64>,
    }

    static STATE: std::sync::OnceLock<Arc<IngressShadowTelemetryState>> =
        std::sync::OnceLock::new();
    static INSTRUMENTS: std::sync::OnceLock<IngressShadowInstruments> = std::sync::OnceLock::new();
    let state = STATE
        .get_or_init(|| {
            Arc::new(IngressShadowTelemetryState {
                enabled: AtomicBool::new(false),
                stream_activity: std::sync::Mutex::new(None),
            })
        })
        .clone();
    state.set(enabled, stream_activity);
    INSTRUMENTS.get_or_init(|| IngressShadowInstruments {
        _enabled: opentelemetry::global::meter("waddle-server")
            .i64_observable_gauge("ingress.shadow.enabled")
            // No unit on purpose: the OTLP→Prometheus translation appends
            // `_ratio` to unit-"1" gauges, which would rename the series the
            // soak alerts select (`ingress_shadow_enabled`).
            .with_description("Whether the ingress-shadow worker is enabled for this replica.")
            .with_callback({
                let state = state.clone();
                move |observer| {
                    observer.observe(i64::from(state.enabled.load(Ordering::Acquire)), &[]);
                }
            })
            .build(),
        _oldest_outstanding_submission_age: opentelemetry::global::meter("waddle-server")
            .f64_observable_gauge("ingress.shadow.oldest_outstanding_submission_age")
            .with_description(
                "Age in seconds of this replica's oldest admitted but uncompleted ingress-shadow submission.",
            )
            .with_unit("s")
            .with_callback(move |observer| {
                observer.observe(state.oldest_outstanding_submission_age_seconds(), &[]);
            })
            .build(),
    });
}

#[cfg(feature = "clustering")]
type IngressShadowExecuteFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
#[cfg(feature = "clustering")]
type IngressShadowExecutor = Arc<
    dyn Fn(IngressShadowTask, Option<OutstandingSubmission>) -> IngressShadowExecuteFuture
        + Send
        + Sync,
>;
#[cfg(all(test, feature = "clustering"))]
type IngressShadowSimpleExecutor =
    Arc<dyn Fn(IngressShadowTask) -> IngressShadowExecuteFuture + Send + Sync>;

impl IngressShadowTask {
    fn kind(&self) -> Option<IngressShadowRequestKind> {
        match self {
            Self::Enroll { .. } => Some(IngressShadowRequestKind::Enroll),
            Self::Submit(_) => Some(IngressShadowRequestKind::Submit),
            Self::Retire { .. } => None,
        }
    }

    fn stream_id(&self) -> &SmSessionId {
        match self {
            Self::Enroll { stream_id } => stream_id,
            Self::Submit(submission) => &submission.stream_id,
            Self::Retire { stream_id } => stream_id,
        }
    }
}

fn validate_ingress_shadow_prerequisites(
    driver: DatabaseDriver,
    clustering_compiled: bool,
    has_node_identity: bool,
) -> Result<(), IngressShadowStartupError> {
    if driver != DatabaseDriver::Postgres {
        return Err(IngressShadowStartupError::PostgresRequired);
    }
    if !clustering_compiled {
        return Err(IngressShadowStartupError::ClusteringFeatureRequired);
    }
    if !has_node_identity {
        return Err(IngressShadowStartupError::NodeIdentityRequired);
    }
    Ok(())
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
    ) -> Result<Self, IngressShadowStartupError> {
        if !config.enabled {
            #[cfg(feature = "clustering")]
            init_ingress_shadow_instruments(false, None);
            return Ok(Self::disabled());
        }
        validate_ingress_shadow_prerequisites(
            database.driver(),
            cfg!(feature = "clustering"),
            node_identity.is_some(),
        )?;
        #[cfg(not(feature = "clustering"))]
        {
            let _ = (config, database, lineage, node_identity);
            Err(IngressShadowStartupError::ClusteringFeatureRequired)
        }
        #[cfg(feature = "clustering")]
        {
            let node_identity = node_identity.expect("validated shadow node identity");
            let mut shadow_database_config = crate::db::DatabaseConfig::new(
                database.driver(),
                database.database_url().to_owned(),
            );
            shadow_database_config.pool_size = config.pool_size;
            let database = Database::from_config("ingress-shadow", &shadow_database_config)
                .await
                .map_err(IngressShadowStartupError::DedicatedPoolOpen)?;
            let tx = Arc::new(std::sync::Mutex::new(None));
            let enqueued_streams = Arc::new(std::sync::Mutex::new(HashSet::new()));
            let retiring_streams = Arc::new(std::sync::Mutex::new(HashSet::new()));
            let stream_activity = Arc::new(std::sync::Mutex::new(StreamActivityState::default()));
            init_ingress_shadow_instruments(true, Some(stream_activity.clone()));
            let worker = IngressShadowProcessor {
                database,
                lineage,
                node_identity,
                retry_attempts: config.retry_attempts,
                tx: tx.clone(),
                enqueued_streams: enqueued_streams.clone(),
                retiring_streams: retiring_streams.clone(),
                #[cfg(test)]
                forced_alias_serialization_failures: Arc::new(std::sync::atomic::AtomicUsize::new(
                    0,
                )),
                #[cfg(test)]
                forced_retirement_retryable_failures: Arc::new(
                    std::sync::atomic::AtomicUsize::new(0),
                ),
                gc_state: Arc::new(RetentionGcState::default()),
            };
            let recovery_database = worker.database.clone();
            let handle = Self::spawn_worker_with_enqueued_streams(
                WorkerLimits {
                    queue_capacity: config.queue_capacity,
                    max_concurrency: ingress_shadow_max_concurrency(
                        config.queue_capacity,
                        config.pool_size,
                    ),
                },
                tx,
                enqueued_streams,
                retiring_streams,
                Some(worker.database.clone()),
                stream_activity,
                Arc::new(move |task, outstanding| {
                    let worker = worker.clone();
                    Box::pin(async move {
                        worker.execute(task, outstanding.as_ref()).await;
                    })
                }),
            );
            handle
                .recover_orphaned_retirements(&recovery_database)
                .await;
            Ok(handle)
        }
    }

    pub fn try_enroll_stream(&self, stream_id: SmSessionId) -> IngressShadowDisposition {
        self.try_send(IngressShadowTask::Enroll { stream_id })
    }

    pub fn ensure_stream_enrollment(&self, stream_id: SmSessionId) -> IngressShadowDisposition {
        self.try_enroll_stream(stream_id)
    }

    /// Forget a terminal SM stream so a future session with the same ID can
    /// enroll again without retaining this process-lifetime gate entry, while
    /// releasing the stream's shadow SM references after earlier queued work
    /// for the stream has drained.
    pub fn forget_stream(&self, stream_id: &SmSessionId) {
        #[cfg(feature = "clustering")]
        if let IngressShadowInner::Worker {
            enqueued_streams, ..
        } = self.inner.as_ref()
        {
            forget_enqueued_stream(enqueued_streams, stream_id);
        }
        let _ = self.try_send(IngressShadowTask::Retire {
            stream_id: stream_id.clone(),
        });
    }

    #[cfg(feature = "clustering")]
    async fn recover_orphaned_retirements(&self, database: &Database) {
        let IngressShadowInner::Worker {
            retirement_capacity,
            ..
        } = self.inner.as_ref()
        else {
            return;
        };
        let orphaned =
            match orphaned_shadow_streams(database, retirement_capacity.saturating_add(1)).await {
                Ok(orphaned) => orphaned,
                Err(error) => {
                    tracing::warn!(%error, "ingress shadow orphan retirement recovery failed");
                    return;
                }
            };
        for stream_id in orphaned {
            let _ = self.try_send(IngressShadowTask::Retire { stream_id });
        }
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

    pub async fn wait_for_stream_idle(&self, stream_id: &SmSessionId, timeout: Duration) -> bool {
        #[cfg(not(feature = "clustering"))]
        {
            let _ = (stream_id, timeout);
            true
        }
        #[cfg(feature = "clustering")]
        {
            let IngressShadowInner::Worker {
                stream_activity, ..
            } = self.inner.as_ref()
            else {
                let _ = (stream_id, timeout);
                return true;
            };
            let deadline = std::time::Instant::now() + timeout;
            loop {
                let Some(waiter) = wait_for_stream_idle_notifier(stream_activity, stream_id) else {
                    return true;
                };
                // Register the waiter before checking activity again.  The
                // finisher removes the waiter from the map as it notifies;
                // without this arming/recheck pair, that notification can
                // fall between `wait_for_stream_idle_notifier` and the
                // first poll of `notified()`, forcing a false timeout.
                let notified = waiter.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if stream_is_idle(stream_activity, stream_id) {
                    return true;
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() || tokio::time::timeout(remaining, notified).await.is_err() {
                    return false;
                }
            }
        }
    }

    pub async fn drain_and_join(&self, timeout: Duration) -> bool {
        #[cfg(not(feature = "clustering"))]
        let _ = timeout;
        match self.inner.as_ref() {
            IngressShadowInner::Disabled => true,
            #[cfg(feature = "clustering")]
            IngressShadowInner::Worker {
                tx,
                shutdown,
                stream_activity,
                ..
            } => {
                shutdown.cancel();
                close_worker_intake(tx);
                if tokio::time::timeout(timeout, shutdown.wait_for_completion())
                    .await
                    .is_ok()
                {
                    true
                } else {
                    shutdown.force_stop();
                    // Every admitted submission — queued, never polled, or
                    // in flight — is released through `finish_outstanding`
                    // exactly when its terminal/abort telemetry is emitted,
                    // so an empty outstanding map is the barrier the final
                    // telemetry flush needs.
                    if tokio::time::timeout(
                        FORCED_TEARDOWN_JOIN,
                        wait_for_outstanding_drained(stream_activity),
                    )
                    .await
                    .is_err()
                    {
                        tracing::warn!(
                            outstanding = stream_activity_lock(stream_activity).outstanding.len(),
                            budget_ms = FORCED_TEARDOWN_JOIN.as_millis(),
                            "forced ingress shadow teardown barrier expired with admitted submissions still outstanding"
                        );
                    }
                    false
                }
            }
        }
    }

    pub async fn wait_for_completion(&self) {
        match self.inner.as_ref() {
            IngressShadowInner::Disabled => {}
            #[cfg(feature = "clustering")]
            IngressShadowInner::Worker { shutdown, .. } => shutdown.wait_for_completion().await,
        }
    }

    fn try_send(&self, task: IngressShadowTask) -> IngressShadowDisposition {
        let kind = task.kind();
        let stream_id = task.stream_id().clone();
        let disposition = match self.inner.as_ref() {
            IngressShadowInner::Disabled => IngressShadowDisposition::Disabled,
            #[cfg(feature = "clustering")]
            IngressShadowInner::Worker {
                tx,
                enqueued_streams,
                retiring_streams,
                retirement_retry_dispatcher,
                stream_activity,
                submission_capacity,
                enrollment_capacity,
                retirement_capacity,
                ..
            } => try_send_worker_task(
                WorkerTaskContext {
                    tx,
                    enqueued_streams,
                    retiring_streams,
                    retirement_retry_dispatcher,
                    stream_activity,
                    submission_capacity,
                    enrollment_capacity,
                    retirement_capacity: *retirement_capacity,
                },
                task,
            ),
        };
        observe_disposition(kind, stream_id, disposition)
    }

    #[cfg(all(test, feature = "clustering"))]
    fn spawn_worker(
        queue_capacity: usize,
        max_concurrency: usize,
        execute: IngressShadowSimpleExecutor,
    ) -> Self {
        Self::spawn_worker_with_enqueued_streams(
            WorkerLimits {
                queue_capacity,
                max_concurrency,
            },
            Arc::new(std::sync::Mutex::new(None)),
            Arc::new(std::sync::Mutex::new(HashSet::new())),
            Arc::new(std::sync::Mutex::new(HashSet::new())),
            None,
            Arc::new(std::sync::Mutex::new(StreamActivityState::default())),
            Arc::new(move |task, _outstanding| execute(task)),
        )
    }

    #[cfg(all(test, feature = "clustering"))]
    pub(crate) fn spawn_test_worker<F, Fut>(
        queue_capacity: usize,
        max_concurrency: usize,
        execute: F,
    ) -> Self
    where
        F: Fn(IngressShadowTestTaskKind, SmSessionId) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::spawn_worker(
            queue_capacity,
            max_concurrency,
            Arc::new(move |task| {
                let kind = match task {
                    IngressShadowTask::Enroll { .. } => IngressShadowTestTaskKind::Enroll,
                    IngressShadowTask::Submit(_) => IngressShadowTestTaskKind::Submit,
                    IngressShadowTask::Retire { .. } => IngressShadowTestTaskKind::Retire,
                };
                let stream_id = task.stream_id().clone();
                Box::pin(execute(kind, stream_id))
            }),
        )
    }

    #[cfg(feature = "clustering")]
    fn spawn_worker_with_enqueued_streams(
        limits: WorkerLimits,
        tx: IngressShadowTx,
        enqueued_streams: Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
        retiring_streams: Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
        retry_database: Option<Database>,
        stream_activity: Arc<std::sync::Mutex<StreamActivityState>>,
        execute: IngressShadowExecutor,
    ) -> Self {
        let WorkerLimits {
            queue_capacity,
            max_concurrency,
        } = limits;
        let (sender, rx) = mpsc::unbounded_channel();
        *tx.lock()
            .expect("shadow worker sender mutex must not be poisoned") = Some(sender);
        let submission_capacity = Arc::new(Semaphore::new(queue_capacity));
        let enrollment_capacity = Arc::new(Semaphore::new(queue_capacity));
        let shutdown = Arc::new(IngressShadowShutdown::default());
        let scheduler_shutdown = shutdown.clone();
        let scheduler = tokio::spawn(
            IngressShadowScheduler::new(
                rx,
                max_concurrency,
                execute,
                stream_activity.clone(),
                scheduler_shutdown,
            )
            .run(),
        );
        let retirement_retry_dispatcher = RetirementRetryDispatcher {
            state: Arc::new(std::sync::Mutex::new(RetirementRetryState {
                scan_requested: true,
                ..RetirementRetryState::default()
            })),
            notify: Arc::new(Notify::new()),
            capacity: queue_capacity,
        };
        tokio::spawn(run_retirement_retry_dispatcher(
            tx.clone(),
            retiring_streams.clone(),
            retirement_retry_dispatcher.clone(),
            queue_capacity,
            shutdown.clone(),
            retry_database,
        ));
        let shutdown_completion = shutdown.clone();
        tokio::spawn(async move {
            shutdown_completion.cancellation.cancelled().await;
            let _ = scheduler.await;
            shutdown_completion.mark_complete();
        });
        Self {
            inner: Arc::new(IngressShadowInner::Worker {
                tx,
                enqueued_streams,
                retiring_streams,
                retirement_retry_dispatcher,
                stream_activity,
                submission_capacity,
                enrollment_capacity,
                retirement_capacity: queue_capacity,
                shutdown,
            }),
        }
    }

    #[cfg(all(test, feature = "clustering"))]
    fn shutdown(&self) -> Option<Arc<IngressShadowShutdown>> {
        match self.inner.as_ref() {
            IngressShadowInner::Disabled => None,
            IngressShadowInner::Worker { shutdown, .. } => Some(shutdown.clone()),
        }
    }
}

/// Sizing knobs for a spawned shadow worker.
#[cfg(feature = "clustering")]
struct WorkerLimits {
    queue_capacity: usize,
    max_concurrency: usize,
}

/// Borrowed view of the worker-variant queue state that a single task
/// admission needs; groups what would otherwise be eight loose parameters.
#[cfg(feature = "clustering")]
struct WorkerTaskContext<'a> {
    tx: &'a IngressShadowTx,
    enqueued_streams: &'a Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    retiring_streams: &'a Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    retirement_retry_dispatcher: &'a RetirementRetryDispatcher,
    stream_activity: &'a Arc<std::sync::Mutex<StreamActivityState>>,
    submission_capacity: &'a Arc<Semaphore>,
    enrollment_capacity: &'a Arc<Semaphore>,
    retirement_capacity: usize,
}

#[cfg(feature = "clustering")]
fn try_send_worker_task(
    ctx: WorkerTaskContext<'_>,
    task: IngressShadowTask,
) -> IngressShadowDisposition {
    let WorkerTaskContext {
        tx,
        enqueued_streams,
        retiring_streams,
        retirement_retry_dispatcher,
        stream_activity,
        submission_capacity,
        enrollment_capacity,
        retirement_capacity,
    } = ctx;
    match task {
        IngressShadowTask::Enroll { stream_id } => {
            send_enrollment_task(tx, enqueued_streams, enrollment_capacity, stream_id)
        }
        submit @ IngressShadowTask::Submit(_) => {
            let stream_id = submit.stream_id().clone();
            match ensure_stream_enrollment_task(
                tx,
                enqueued_streams,
                enrollment_capacity,
                &stream_id,
            ) {
                IngressShadowDisposition::Enqueued => {}
                disposition => return disposition,
            }
            let permit = match try_acquire_task_permit(submission_capacity) {
                Ok(permit) => permit,
                Err(disposition) => return disposition,
            };
            note_stream_task_enqueued(stream_activity, &stream_id);
            match admit_submission(tx, stream_activity, stream_id.clone(), submit, permit) {
                Ok(()) => IngressShadowDisposition::Enqueued,
                Err(task) => {
                    note_stream_task_finished(stream_activity, &stream_id);
                    drop(task.permit);
                    IngressShadowDisposition::Closed
                }
            }
        }
        IngressShadowTask::Retire { stream_id } => {
            let disposition =
                send_retirement_task(tx, retiring_streams, retirement_capacity, stream_id.clone());
            if matches!(disposition, IngressShadowDisposition::QueueFull) {
                schedule_retirement_task_retry(retirement_retry_dispatcher, stream_id.clone());
            }
            if !matches!(disposition, IngressShadowDisposition::Enqueued) {
                // A closed worker cannot run the ordered retirement task; do
                // not retain the process-lifetime enrollment gate in that case.
                forget_enqueued_stream(enqueued_streams, &stream_id);
            }
            disposition
        }
    }
}

#[cfg(feature = "clustering")]
fn send_enrollment_task(
    tx: &IngressShadowTx,
    enqueued_streams: &Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    enrollment_capacity: &Arc<Semaphore>,
    stream_id: SmSessionId,
) -> IngressShadowDisposition {
    let mut enqueued = enqueued_streams
        .lock()
        .expect("enqueued stream mutex must not be poisoned");
    if enqueued.contains(&stream_id) {
        return IngressShadowDisposition::Enqueued;
    }
    let permit = match try_acquire_task_permit(enrollment_capacity) {
        Ok(permit) => permit,
        Err(disposition) => return disposition,
    };
    enqueued.insert(stream_id.clone());
    match send_worker_task(
        tx,
        QueuedIngressShadowTask {
            task: IngressShadowTask::Enroll {
                stream_id: stream_id.clone(),
            },
            permit: Some(permit),
            outstanding: None,
        },
    ) {
        Ok(()) => IngressShadowDisposition::Enqueued,
        Err(task) => {
            drop(task.permit);
            enqueued.remove(&stream_id);
            IngressShadowDisposition::Closed
        }
    }
}

#[cfg(feature = "clustering")]
fn ensure_stream_enrollment_task(
    tx: &IngressShadowTx,
    enqueued_streams: &Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    enrollment_capacity: &Arc<Semaphore>,
    stream_id: &SmSessionId,
) -> IngressShadowDisposition {
    let mut enqueued = enqueued_streams
        .lock()
        .expect("enqueued stream mutex must not be poisoned");
    if enqueued.contains(stream_id) {
        return IngressShadowDisposition::Enqueued;
    }
    let permit = match try_acquire_task_permit(enrollment_capacity) {
        Ok(permit) => permit,
        Err(disposition) => return disposition,
    };
    enqueued.insert(stream_id.clone());
    match send_worker_task(
        tx,
        QueuedIngressShadowTask {
            task: IngressShadowTask::Enroll {
                stream_id: stream_id.clone(),
            },
            permit: Some(permit),
            outstanding: None,
        },
    ) {
        Ok(()) => IngressShadowDisposition::Enqueued,
        Err(task) => {
            drop(task.permit);
            enqueued.remove(stream_id);
            IngressShadowDisposition::Closed
        }
    }
}

#[cfg(feature = "clustering")]
fn send_retirement_task(
    tx: &IngressShadowTx,
    retiring_streams: &Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    retirement_capacity: usize,
    stream_id: SmSessionId,
) -> IngressShadowDisposition {
    let mut retiring = retiring_streams
        .lock()
        .expect("retiring stream mutex must not be poisoned");
    if retiring.contains(&stream_id) {
        return IngressShadowDisposition::Enqueued;
    }
    if retiring.len() >= retirement_capacity {
        return IngressShadowDisposition::QueueFull;
    }
    retiring.insert(stream_id.clone());
    match send_worker_task(
        tx,
        QueuedIngressShadowTask {
            task: IngressShadowTask::Retire { stream_id },
            permit: None,
            outstanding: None,
        },
    ) {
        Ok(()) => IngressShadowDisposition::Enqueued,
        Err(task) => {
            retiring.remove(task.task.stream_id());
            IngressShadowDisposition::Closed
        }
    }
}

#[cfg(feature = "clustering")]
fn reschedule_retirement_task(
    tx: &IngressShadowTx,
    stream_id: SmSessionId,
) -> IngressShadowDisposition {
    match send_worker_task(
        tx,
        QueuedIngressShadowTask {
            task: IngressShadowTask::Retire { stream_id },
            permit: None,
            outstanding: None,
        },
    ) {
        Ok(()) => IngressShadowDisposition::Enqueued,
        Err(_) => IngressShadowDisposition::Closed,
    }
}

#[cfg(feature = "clustering")]
fn schedule_deferred_retirement_retry(tx: IngressShadowTx, stream_id: SmSessionId) {
    // Only an already-admitted retirement can enter this path, so this timer
    // is bounded by `retirement_capacity`.  Preserve its admission while a
    // deferred SM claim is released rather than declaring terminal cleanup
    // complete on the first fenced observation.
    tokio::spawn(async move {
        tokio::time::sleep(DEFAULT_RETIREMENT_ADMISSION_RETRY_DELAY).await;
        let _ = reschedule_retirement_task(&tx, stream_id);
    });
}

#[cfg(feature = "clustering")]
fn schedule_retirement_task_retry(dispatcher: &RetirementRetryDispatcher, stream_id: SmSessionId) {
    let mut state = dispatcher
        .state
        .lock()
        .expect("retirement retry dispatcher mutex must not be poisoned");
    if state.queued_members.contains(&stream_id) {
        state.scan_requested = true;
    } else if state.queued.len() < dispatcher.capacity {
        state.queued_members.insert(stream_id.clone());
        state.queued.push_back(stream_id);
    } else {
        state.scan_requested = true;
    }
    dispatcher.notify.notify_one();
}

#[cfg(feature = "clustering")]
fn note_stream_task_enqueued(
    stream_activity: &Arc<std::sync::Mutex<StreamActivityState>>,
    stream_id: &SmSessionId,
) {
    let mut state = stream_activity
        .lock()
        .expect("stream activity mutex must not be poisoned");
    *state.pending.entry(stream_id.clone()).or_insert(0) += 1;
}

#[cfg(feature = "clustering")]
fn note_stream_task_finished(
    stream_activity: &Arc<std::sync::Mutex<StreamActivityState>>,
    stream_id: &SmSessionId,
) {
    let waiter = {
        let mut state = stream_activity
            .lock()
            .expect("stream activity mutex must not be poisoned");
        let Some(pending) = state.pending.get_mut(stream_id) else {
            return;
        };
        *pending = pending.saturating_sub(1);
        if *pending > 0 {
            return;
        }
        state.pending.remove(stream_id);
        state.idle_waiters.remove(stream_id)
    };
    if let Some(waiter) = waiter {
        waiter.notify_waiters();
    }
}

/// Admit a submission atomically: the obligation is registered, the task is
/// handed to the worker and the admission counter advances all under the
/// activity lock (the channel send is synchronous), so no observer — the
/// worker releasing the obligation, or the forced-teardown barrier — can see
/// an admitted submission whose admission has not been counted yet. A closed
/// intake leaves no trace: the entry is removed before the lock is released
/// and the disarmed guard records nothing.
#[cfg(feature = "clustering")]
fn admit_submission(
    tx: &IngressShadowTx,
    stream_activity: &Arc<std::sync::Mutex<StreamActivityState>>,
    stream_id: SmSessionId,
    submit: IngressShadowTask,
    permit: OwnedSemaphorePermit,
) -> Result<(), QueuedIngressShadowTask> {
    let mut state = stream_activity_lock(stream_activity);
    let seq = state.next_seq;
    state.next_seq = state.next_seq.wrapping_add(1);
    state.outstanding.insert(seq, Instant::now());
    let outstanding = OutstandingSubmission {
        stream_activity: stream_activity.clone(),
        stream_id,
        seq,
        armed: true,
    };
    match send_worker_task(
        tx,
        QueuedIngressShadowTask {
            task: submit,
            permit: Some(permit),
            outstanding: Some(outstanding),
        },
    ) {
        Ok(()) => {
            waddle_xmpp::telemetry::reliability::increment_ingress_shadow_admissions();
            Ok(())
        }
        Err(mut task) => {
            state.outstanding.remove(&seq);
            if let Some(mut rejected) = task.outstanding.take() {
                rejected.armed = false;
            }
            Err(task)
        }
    }
}

/// Register an admitted submission's obligation and return its handle
/// (test-only: production admission goes through [`admit_submission`]).
#[cfg(all(test, feature = "clustering"))]
fn register_outstanding(
    stream_activity: &Arc<std::sync::Mutex<StreamActivityState>>,
    stream_id: SmSessionId,
) -> OutstandingSubmission {
    let seq = {
        let mut state = stream_activity_lock(stream_activity);
        let seq = state.next_seq;
        state.next_seq = state.next_seq.wrapping_add(1);
        state.outstanding.insert(seq, Instant::now());
        seq
    };
    OutstandingSubmission {
        stream_activity: stream_activity.clone(),
        stream_id,
        seq,
        armed: true,
    }
}

/// Resolve once no admitted submission is outstanding.
#[cfg(feature = "clustering")]
async fn wait_for_outstanding_drained(
    stream_activity: &Arc<std::sync::Mutex<StreamActivityState>>,
) {
    while !stream_activity_lock(stream_activity).outstanding.is_empty() {
        tokio::time::sleep(FORCED_TEARDOWN_POLL).await;
    }
}

/// Release an obligation exactly once; later calls for the same `seq` are
/// no-ops, so a terminal observation followed by the handle's drop never
/// double-counts.
#[cfg(feature = "clustering")]
fn finish_outstanding(
    stream_activity: &Arc<std::sync::Mutex<StreamActivityState>>,
    stream_id: &SmSessionId,
    seq: u64,
    end: OutstandingEnd,
) {
    // The obligation is released under the lock and only after its telemetry
    // has been recorded, so a forced-teardown observer that finds the map
    // empty is guaranteed to see the counters already advanced.
    let mut state = stream_activity_lock(stream_activity);
    if !state.outstanding.contains_key(&seq) {
        return;
    }
    if matches!(end, OutstandingEnd::Aborted) {
        waddle_xmpp::telemetry::reliability::increment_ingress_shadow_aborted();
        waddle_xmpp::telemetry::reliability::increment_ingress_shadow_completions();
        tracing::warn!(stream_id = %stream_id, seq, "ingress shadow submission aborted before completion");
    }
    state.outstanding.remove(&seq);
}

#[cfg(feature = "clustering")]
fn wait_for_stream_idle_notifier(
    stream_activity: &Arc<std::sync::Mutex<StreamActivityState>>,
    stream_id: &SmSessionId,
) -> Option<Arc<Notify>> {
    let mut state = stream_activity
        .lock()
        .expect("stream activity mutex must not be poisoned");
    state.pending.contains_key(stream_id).then(|| {
        state
            .idle_waiters
            .entry(stream_id.clone())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    })
}

#[cfg(feature = "clustering")]
fn stream_is_idle(
    stream_activity: &Arc<std::sync::Mutex<StreamActivityState>>,
    stream_id: &SmSessionId,
) -> bool {
    !stream_activity
        .lock()
        .expect("stream activity mutex must not be poisoned")
        .pending
        .contains_key(stream_id)
}

#[cfg(feature = "clustering")]
async fn run_retirement_retry_dispatcher(
    tx: IngressShadowTx,
    retiring_streams: Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    dispatcher: RetirementRetryDispatcher,
    retirement_capacity: usize,
    shutdown: Arc<IngressShadowShutdown>,
    database: Option<Database>,
) {
    loop {
        let has_work = {
            let state = dispatcher
                .state
                .lock()
                .expect("retirement retry dispatcher mutex must not be poisoned");
            !state.queued.is_empty() || state.scan_requested
        };
        tokio::select! {
            _ = shutdown.cancellation.cancelled() => return,
            _ = tokio::time::sleep(DEFAULT_RETIREMENT_ADMISSION_RETRY_DELAY), if has_work => {}
            _ = dispatcher.notify.notified(), if !has_work => {}
        }

        let should_scan = {
            let state = dispatcher
                .state
                .lock()
                .expect("retirement retry dispatcher mutex must not be poisoned");
            state.scan_requested
        };
        if should_scan {
            let Some(database) = database.as_ref() else {
                dispatcher
                    .state
                    .lock()
                    .expect("retirement retry dispatcher mutex must not be poisoned")
                    .scan_requested = false;
                continue;
            };
            match orphaned_shadow_streams(database, dispatcher.capacity.saturating_add(1)).await {
                Ok(orphaned) => {
                    let mut state = dispatcher
                        .state
                        .lock()
                        .expect("retirement retry dispatcher mutex must not be poisoned");
                    queue_scanned_retirements(&mut state, orphaned, dispatcher.capacity);
                }
                Err(error) => {
                    tracing::warn!(%error, "ingress shadow orphan retirement retry scan failed");
                    dispatcher
                        .state
                        .lock()
                        .expect("retirement retry dispatcher mutex must not be poisoned")
                        .scan_requested = true;
                }
            }
        }

        let next = {
            let mut state = dispatcher
                .state
                .lock()
                .expect("retirement retry dispatcher mutex must not be poisoned");
            let next = state.queued.pop_front();
            if let Some(stream_id) = next.as_ref() {
                state.queued_members.remove(stream_id);
            }
            next
        };
        let Some(stream_id) = next else {
            continue;
        };
        match send_retirement_task(
            &tx,
            &retiring_streams,
            retirement_capacity,
            stream_id.clone(),
        ) {
            IngressShadowDisposition::QueueFull => {
                let mut state = dispatcher
                    .state
                    .lock()
                    .expect("retirement retry dispatcher mutex must not be poisoned");
                if state.queued.len() < dispatcher.capacity
                    && state.queued_members.insert(stream_id.clone())
                {
                    state.queued.push_back(stream_id);
                }
                state.scan_requested = true;
            }
            IngressShadowDisposition::Enqueued | IngressShadowDisposition::Closed => {}
            IngressShadowDisposition::Disabled => {
                unreachable!("worker retry path cannot be disabled")
            }
        }
    }
}

#[cfg(feature = "clustering")]
fn queue_scanned_retirements(
    state: &mut RetirementRetryState,
    orphaned: Vec<SmSessionId>,
    capacity: usize,
) {
    let mut orphaned = orphaned.into_iter();
    let scanned_page: Vec<_> = orphaned.by_ref().take(capacity).collect();
    state.scan_requested = orphaned.next().is_some();
    for stream_id in scanned_page {
        if !state.queued_members.insert(stream_id.clone()) {
            continue;
        }
        if state.queued.len() >= capacity {
            state.queued_members.remove(&stream_id);
            state.scan_requested = true;
            break;
        }
        state.queued.push_back(stream_id);
    }
}

#[cfg(feature = "clustering")]
fn send_worker_task(
    tx: &IngressShadowTx,
    task: QueuedIngressShadowTask,
) -> Result<(), QueuedIngressShadowTask> {
    let guard = tx
        .lock()
        .expect("shadow worker sender mutex must not be poisoned");
    let Some(sender) = guard.as_ref() else {
        return Err(task);
    };
    sender.send(task).map_err(|error| error.0)
}

#[cfg(feature = "clustering")]
fn close_worker_intake(tx: &IngressShadowTx) {
    tx.lock()
        .expect("shadow worker sender mutex must not be poisoned")
        .take();
}

fn observe_disposition(
    kind: Option<IngressShadowRequestKind>,
    stream_id: SmSessionId,
    disposition: IngressShadowDisposition,
) -> IngressShadowDisposition {
    let Some(kind) = kind else {
        return disposition;
    };
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
fn try_acquire_task_permit(
    capacity: &Arc<Semaphore>,
) -> Result<OwnedSemaphorePermit, IngressShadowDisposition> {
    capacity
        .clone()
        .try_acquire_owned()
        .map_err(|error| match error {
            tokio::sync::TryAcquireError::NoPermits => IngressShadowDisposition::QueueFull,
            tokio::sync::TryAcquireError::Closed => IngressShadowDisposition::Closed,
        })
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
    tx: IngressShadowTx,
    enqueued_streams: Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    retiring_streams: Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    /// Test-only deterministic fault point inside the transaction, after
    /// fences and digest evaluation but before alias persistence.  PostgreSQL
    /// serialization failures are otherwise difficult to force reliably at
    /// the processor's READ COMMITTED boundary.
    #[cfg(test)]
    forced_alias_serialization_failures: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    forced_retirement_retryable_failures: Arc<std::sync::atomic::AtomicUsize>,
    gc_state: Arc<RetentionGcState>,
}

#[cfg(feature = "clustering")]
#[derive(Clone, Copy)]
struct RetentionGcBudget {
    cooperative: Duration,
    hard_deadline: Duration,
    lock_timeout: Duration,
    statement_timeout: Duration,
    scan_timeout: Duration,
}

#[cfg(feature = "clustering")]
impl RetentionGcBudget {
    const DEFAULT: Self = Self {
        cooperative: RETENTION_GC_BUDGET,
        hard_deadline: RETENTION_GC_HARD_DEADLINE,
        lock_timeout: RETENTION_GC_LOCK_TIMEOUT,
        statement_timeout: RETENTION_GC_STATEMENT_TIMEOUT,
        scan_timeout: RETENTION_GC_SCAN_TIMEOUT,
    };
}

/// Per-process retention GC coordination: one run at a time, and a trigger
/// that arrives while a run is in flight is kept as a pending rerun instead
/// of being dropped, so a burst that ends mid-run still drains.
#[cfg(feature = "clustering")]
#[derive(Default)]
struct RetentionGcState {
    in_flight: AtomicBool,
    rerun_requested: AtomicBool,
}

#[cfg(feature = "clustering")]
struct RetentionGcInFlightGuard<'a>(&'a AtomicBool);

#[cfg(feature = "clustering")]
impl RetentionGcInFlightGuard<'_> {
    fn acquire(flag: &AtomicBool) -> Option<RetentionGcInFlightGuard<'_>> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| RetentionGcInFlightGuard(flag))
    }
}

#[cfg(feature = "clustering")]
impl Drop for RetentionGcInFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(feature = "clustering")]
fn classify_retention_gc_result(
    result: Result<AliasGcOutcome, AliasGcFailure>,
) -> (waddle_xmpp::telemetry::attributes::IngressGcOutcome, usize) {
    use waddle_xmpp::telemetry::attributes::IngressGcOutcome;

    match result {
        Ok(AliasGcOutcome {
            deleted_messages,
            completed: true,
        }) => (IngressGcOutcome::Completed, deleted_messages),
        Ok(AliasGcOutcome {
            deleted_messages,
            completed: false,
        }) => (IngressGcOutcome::Partial, deleted_messages),
        Err(AliasGcFailure {
            deleted_messages,
            error: AliasGcError::DatabaseTimeout { .. },
        }) => (IngressGcOutcome::TimedOut, deleted_messages),
        Err(AliasGcFailure {
            deleted_messages,
            error: AliasGcError::Substrate(_),
        }) => (IngressGcOutcome::Failed, deleted_messages),
    }
}

#[cfg(feature = "clustering")]
fn record_retention_gc_result(
    outcome: waddle_xmpp::telemetry::attributes::IngressGcOutcome,
    deleted_messages: usize,
) {
    waddle_xmpp::telemetry::reliability::increment_ingress_shadow_gc_run(outcome);
    if deleted_messages > 0 {
        waddle_xmpp::telemetry::reliability::add_ingress_shadow_gc_reclaimed_messages(
            u64::try_from(deleted_messages).unwrap_or(u64::MAX),
        );
    }
}

#[cfg(feature = "clustering")]
fn clear_failed_enrollment(
    enqueued_streams: &Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    stream_id: &SmSessionId,
) {
    forget_enqueued_stream(enqueued_streams, stream_id);
}

#[cfg(feature = "clustering")]
fn forget_enqueued_stream(
    enqueued_streams: &Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    stream_id: &SmSessionId,
) {
    enqueued_streams
        .lock()
        .expect("enqueued stream mutex must not be poisoned")
        .remove(stream_id);
}

#[cfg(feature = "clustering")]
fn forget_retiring_stream(
    retiring_streams: &Arc<std::sync::Mutex<HashSet<SmSessionId>>>,
    stream_id: &SmSessionId,
) {
    retiring_streams
        .lock()
        .expect("retiring stream mutex must not be poisoned")
        .remove(stream_id);
}

#[cfg(feature = "clustering")]
fn observe_retry_sequence(attempts: usize, exhausted: bool) {
    if attempts <= 1 {
        return;
    }
    let outcome = if exhausted {
        waddle_xmpp::telemetry::attributes::IngressRetryOutcome::Exhausted
    } else {
        waddle_xmpp::telemetry::attributes::IngressRetryOutcome::Retried
    };
    waddle_xmpp::telemetry::reliability::increment_ingress_shadow_tx_retry(outcome);
}

/// Second-scale buckets with an explicit boundary at the soak's 2 s p99 gate
/// and at the 2.5 s attempt deadline: without a 2.0 edge, a steady 1.1 s
/// would interpolate to ≈2.49 s inside `(1.0, 2.5]` and false-fail the gate.
#[cfg(feature = "clustering")]
const SHADOW_TX_DURATION_BUCKETS: [f64; 12] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 2.5, 5.0, 10.0,
];

#[cfg(feature = "clustering")]
fn record_submission_attempt_duration(duration: Duration) {
    waddle_xmpp::histogram_record!(
        "ingress.shadow.tx.duration",
        "s",
        "Duration of one shadow-ingress submission transaction attempt.",
        buckets: SHADOW_TX_DURATION_BUCKETS,
        duration.as_secs_f64(),
    );
}

/// Times exactly one submission attempt and records it exactly once: on
/// completion through [`AttemptTimer::finish`], or — if the deadline cancels
/// the attempt future mid-flight — from `Drop`, so slow transactions cannot
/// hide from the latency gate. An attempt that already finished records
/// nothing more when the enclosing future is later dropped in backoff.
#[cfg(feature = "clustering")]
struct AttemptTimer {
    started: Instant,
    recorded: bool,
}

#[cfg(feature = "clustering")]
impl AttemptTimer {
    fn start() -> Self {
        Self {
            started: Instant::now(),
            recorded: false,
        }
    }

    fn finish(mut self) {
        self.record();
    }

    fn record(&mut self) {
        if !self.recorded {
            self.recorded = true;
            record_submission_attempt_duration(self.started.elapsed());
        }
    }
}

#[cfg(feature = "clustering")]
impl Drop for AttemptTimer {
    fn drop(&mut self) {
        self.record();
    }
}

#[cfg(feature = "clustering")]
async fn orphaned_shadow_streams(
    database: &Database,
    limit: usize,
) -> Result<Vec<SmSessionId>, crate::db::DatabaseError> {
    let conn = database.guard().await?;
    let mut rows = conn
        .query(
            r#"
            SELECT s.stream_id
            FROM ingress_sm_streams s
            LEFT JOIN clustering_claims c
                ON c.entity = ('sm_session:' || s.stream_id)
               AND c.entity_type = 'sm_session'
            WHERE c.entity IS NULL
            ORDER BY s.stream_id ASC
            LIMIT ?
            "#,
            crate::db_params![limit as i64],
        )
        .await?;
    let mut stream_ids = Vec::new();
    while let Some(row) = rows.next().await? {
        let stream_id: String = row.get(0)?;
        stream_ids.push(SmSessionId::new(stream_id));
    }
    Ok(stream_ids)
}

#[cfg(feature = "clustering")]
impl IngressShadowProcessor {
    async fn execute(&self, task: IngressShadowTask, outstanding: Option<&OutstandingSubmission>) {
        let kind = task.kind();
        let stream_id = task.stream_id().clone();
        let claim_epoch = match &task {
            IngressShadowTask::Enroll { .. } => None,
            IngressShadowTask::Submit(submission) => Some(submission.claim_epoch),
            IngressShadowTask::Retire { .. } => None,
        };
        let handled_ordinal = match &task {
            IngressShadowTask::Enroll { .. } => None,
            IngressShadowTask::Submit(submission) => Some(submission.handled_ordinal),
            IngressShadowTask::Retire { .. } => None,
        };
        match task {
            IngressShadowTask::Enroll {
                stream_id: enroll_stream,
            } => {
                let kind = kind.expect("enrollment observations are externally visible");
                let mut attempts = 0_usize;
                let timed = tokio::time::timeout(
                    DEFAULT_TX_DEADLINE,
                    run_with_retry(self.retry_attempts, || {
                        attempts += 1;
                        self.execute_enrollment(&enroll_stream)
                    }),
                )
                .await;
                match timed {
                    Ok(Ok(commit_kind)) => {
                        observe_retry_sequence(attempts, false);
                        observe(IngressShadowObservation::Committed {
                            stream_id,
                            claim_epoch,
                            handled_ordinal,
                            kind: commit_kind,
                        });
                    }
                    Ok(Err(_)) | Err(_) => {
                        observe_retry_sequence(attempts, true);
                        clear_failed_enrollment(&self.enqueued_streams, &enroll_stream);
                        observe(IngressShadowObservation::Failed {
                            kind,
                            stream_id,
                            claim_epoch,
                            handled_ordinal,
                        });
                    }
                }
            }
            IngressShadowTask::Submit(submission) => {
                let mut attempts = 0_usize;
                let timed = tokio::time::timeout(
                    DEFAULT_TX_DEADLINE,
                    run_with_retry(self.retry_attempts, || {
                        attempts += 1;
                        let timer = AttemptTimer::start();
                        let attempt = self.execute_submission(&submission);
                        async move {
                            let result = attempt.await;
                            timer.finish();
                            result
                        }
                    }),
                )
                .await;
                match timed {
                    Ok(Ok(outcome)) => {
                        observe_retry_sequence(attempts, false);
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
                        if let Some(outstanding) = outstanding {
                            finish_outstanding(
                                &outstanding.stream_activity,
                                &outstanding.stream_id,
                                outstanding.seq,
                                OutstandingEnd::Decision,
                            );
                        }
                        if outcome.run_retention_gc {
                            self.run_retention_gc().await;
                        }
                    }
                    Ok(Err(exhausted)) => {
                        observe_retry_sequence(exhausted.attempts, true);
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
                        if let Some(outstanding) = outstanding {
                            finish_outstanding(
                                &outstanding.stream_activity,
                                &outstanding.stream_id,
                                outstanding.seq,
                                OutstandingEnd::Decision,
                            );
                        }
                    }
                    Err(_) => {
                        // A cancelled in-flight attempt has already recorded
                        // itself through `AttemptTimer::drop`.
                        observe_retry_sequence(attempts, true);
                        observe(IngressShadowObservation::Decision {
                            stream_id,
                            claim_epoch,
                            handled_ordinal,
                            class: IngressShadowDecisionClass::Storage,
                            alias: IngressShadowAliasOutcome::None,
                        });
                        if let Some(outstanding) = outstanding {
                            finish_outstanding(
                                &outstanding.stream_activity,
                                &outstanding.stream_id,
                                outstanding.seq,
                                OutstandingEnd::Decision,
                            );
                        }
                    }
                }
            }
            IngressShadowTask::Retire {
                stream_id: retire_stream,
            } => {
                let mut attempts = 0_usize;
                let timed = tokio::time::timeout(
                    DEFAULT_TX_DEADLINE,
                    run_with_retry(self.retry_attempts, || {
                        attempts += 1;
                        self.execute_retirement(&retire_stream)
                    }),
                )
                .await;
                match timed {
                    Ok(Ok(
                        outcome @ (RetirementOutcome::Deleted | RetirementOutcome::StreamMissing),
                    )) => {
                        observe_retry_sequence(attempts, false);
                        forget_retiring_stream(&self.retiring_streams, &retire_stream);
                        if matches!(outcome, RetirementOutcome::Deleted) {
                            self.run_retention_gc().await;
                        }
                    }
                    Ok(Ok(RetirementOutcome::DeferredClaim)) => {
                        observe_retry_sequence(attempts, false);
                        schedule_deferred_retirement_retry(self.tx.clone(), retire_stream.clone());
                    }
                    Ok(Err(_)) | Err(_) => {
                        observe_retry_sequence(attempts, true);
                        let disposition =
                            reschedule_retirement_task(&self.tx, retire_stream.clone());
                        if !matches!(disposition, IngressShadowDisposition::Enqueued) {
                            forget_retiring_stream(&self.retiring_streams, &retire_stream);
                        }
                        tracing::warn!(
                            stream_id = %retire_stream,
                            ?disposition,
                            "ingress shadow stream retirement failed"
                        );
                    }
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

        let (sm_ingress_id, _frontier) =
            match SmIngressStreamRepository::lock(&mut transaction, &fence, &submission.stream_id)
                .await?
            {
                Some(locked) => locked,
                None => (
                    SmIngressStreamRepository::mint(&mut transaction, &submission.stream_id)
                        .await?,
                    0,
                ),
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
                    false,
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
                false,
            ));
        }
        if capture_payload_overflow(&submission.capture.intents)? {
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
                IngressShadowDecisionClass::CaptureOverflow,
                IngressShadowAliasOutcome::None,
                false,
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
        let (message_key, decision, alias) = record_shadow_message(
            &mut transaction,
            sm_ingress_id,
            submission.handled_ordinal,
            submission,
            &digest_input,
            &digest,
        )
        .await?;

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
                false,
            ));
        }

        let effect_outcome = if matches!(alias, IngressShadowAliasOutcome::Existing) {
            EffectIntentRepository::record_all_existing_alias(
                &mut transaction,
                message_key,
                &submission.capture.intents,
            )
            .await?
        } else {
            EffectIntentRepository::record_all(
                &mut transaction,
                message_key,
                &submission.capture.intents,
            )
            .await?
        };
        let decision = if matches!(effect_outcome, EffectIntentWriteOutcome::IntentDivergence) {
            IngressShadowDecisionClass::IntentDivergence
        } else {
            decision
        };
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
        let _ = CanonicalMessageRepository::terminalize(&mut transaction, message_key, Utc::now())
            .await?;
        transaction.commit().await?;
        Ok(ShadowSubmissionOutcome::committed(
            commit_kind,
            decision,
            alias,
            true,
        ))
    }

    async fn execute_retirement(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<RetirementOutcome, IngressUowError> {
        #[cfg(test)]
        if self
            .forced_retirement_retryable_failures
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
        let uow = PostgresIngressUnitOfWork::open_with_node_identity(
            self.database.clone(),
            self.lineage.clone(),
            self.node_identity.clone(),
        )?;
        let mut transaction = uow.begin().await?;
        transaction
            .set_local_timeouts(DEFAULT_LOCK_TIMEOUT_MS, DEFAULT_STATEMENT_TIMEOUT_MS)
            .await?;
        if !SmIngressStreamRepository::fence_claim_absence_for_retirement(
            &mut transaction,
            stream_id,
        )
        .await?
        {
            transaction.commit().await?;
            return Ok(RetirementOutcome::DeferredClaim);
        }
        let Some(sm_ingress_id) =
            SmIngressStreamRepository::lookup_unclaimed(&mut transaction, stream_id).await?
        else {
            transaction.commit().await?;
            return Ok(RetirementOutcome::StreamMissing);
        };
        let message_keys =
            SmIngressRepository::message_keys_for_stream(&mut transaction, sm_ingress_id).await?;
        let terminal_at = Utc::now();
        for key in message_keys {
            let _ =
                CanonicalMessageRepository::terminalize(&mut transaction, key, terminal_at).await?;
        }
        let _ = SmIngressRepository::delete_for_stream(&mut transaction, sm_ingress_id).await?;
        let _ = SmIngressStreamRepository::delete_unclaimed(&mut transaction, stream_id).await?;
        transaction.commit().await?;
        Ok(RetirementOutcome::Deleted)
    }

    async fn run_retention_gc(&self) {
        self.run_retention_gc_with_budget(RetentionGcBudget::DEFAULT)
            .await;
    }

    async fn run_retention_gc_with_budget(&self, budget: RetentionGcBudget) {
        loop {
            // Clear before acquiring: a trigger that lands after this point
            // while the run is in flight is observed after the run.
            self.gc_state
                .rerun_requested
                .store(false, Ordering::Release);
            let Some(in_flight) = RetentionGcInFlightGuard::acquire(&self.gc_state.in_flight)
            else {
                self.gc_state.rerun_requested.store(true, Ordering::Release);
                tracing::debug!("ingress shadow retention GC already in flight; rerun queued");
                return;
            };
            self.run_retention_gc_once(&budget).await;
            drop(in_flight);
            if !self.gc_state.rerun_requested.swap(false, Ordering::AcqRel) {
                return;
            }
        }
    }

    async fn run_retention_gc_once(&self, budget: &RetentionGcBudget) {
        let substrate = match PostgresIngressSubstrate::open(self.database.clone()) {
            Ok(substrate) => substrate,
            Err(error) => {
                tracing::warn!(%error, "ingress shadow retention GC setup failed");
                record_retention_gc_result(
                    waddle_xmpp::telemetry::attributes::IngressGcOutcome::Failed,
                    0,
                );
                return;
            }
        };
        let progress = AliasGcProgress::default();
        let result = tokio::time::timeout(
            budget.hard_deadline,
            substrate.gc_expired_aliases(
                Utc::now(),
                AliasGcBudget {
                    deadline: tokio::time::Instant::now() + budget.cooperative,
                    lock_timeout: budget.lock_timeout,
                    statement_timeout: budget.statement_timeout,
                    scan_timeout: budget.scan_timeout,
                    progress: progress.clone(),
                },
            ),
        )
        .await;
        match result {
            Ok(result) => {
                if let Err(failure) = &result {
                    tracing::warn!(%failure, "ingress shadow retention GC failed");
                }
                let (outcome, deleted_messages) = classify_retention_gc_result(result);
                record_retention_gc_result(outcome, deleted_messages);
            }
            Err(error) => {
                tracing::warn!(%error, "ingress shadow retention GC exceeded hard deadline");
                record_retention_gc_result(
                    waddle_xmpp::telemetry::attributes::IngressGcOutcome::TimedOut,
                    progress.committed(),
                );
            }
        }
    }
}

#[cfg(feature = "clustering")]
struct IngressShadowScheduler {
    rx: mpsc::UnboundedReceiver<QueuedIngressShadowTask>,
    completion_tx: mpsc::UnboundedSender<SmSessionId>,
    completion_rx: mpsc::UnboundedReceiver<SmSessionId>,
    execute: IngressShadowExecutor,
    max_concurrency: usize,
    stream_activity: Arc<std::sync::Mutex<StreamActivityState>>,
    shutdown: Arc<IngressShadowShutdown>,
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
        stream_activity: Arc<std::sync::Mutex<StreamActivityState>>,
        shutdown: Arc<IngressShadowShutdown>,
    ) -> Self {
        let (completion_tx, completion_rx) = mpsc::unbounded_channel();
        Self {
            rx,
            completion_tx,
            completion_rx,
            execute,
            max_concurrency: max_concurrency.max(1),
            stream_activity,
            shutdown,
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
                _ = self.shutdown.force_stop.cancelled() => break,
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
            let stream_activity = self.stream_activity.clone();
            let task_shutdown = self.shutdown.clone();
            let task_finished = Arc::new(AtomicBool::new(false));
            let task_finished_for_completion = task_finished.clone();
            // Only submissions were counted at enqueue time; decrementing for
            // an Enroll/Retire task would consume a still-running submission's
            // pending count and let a claim transfer treat active shadow work
            // as drained.
            let counted_submission = matches!(task.task, IngressShadowTask::Submit(_));
            let QueuedIngressShadowTask {
                task,
                permit,
                outstanding,
            } = task;
            let task_handle = tokio::spawn(async move {
                (execute)(task, outstanding).await;
                drop(permit);
                task_shutdown.finish_active_task(&task_finished_for_completion);
                if counted_submission {
                    note_stream_task_finished(&stream_activity, &stream_id);
                }
                let _ = completion_tx.send(stream_id);
            });
            self.shutdown
                .track_active_task(task_finished, task_handle.abort_handle());
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
    run_retention_gc: bool,
}

#[cfg(feature = "clustering")]
impl ShadowSubmissionOutcome {
    fn committed(
        commit_kind: IngressShadowCommitKind,
        decision: IngressShadowDecisionClass,
        alias: IngressShadowAliasOutcome,
        run_retention_gc: bool,
    ) -> Self {
        Self {
            commit_kind: Some(commit_kind),
            decision,
            alias,
            run_retention_gc,
        }
    }

    fn rolled_back(decision: IngressShadowDecisionClass) -> Self {
        Self {
            commit_kind: None,
            decision,
            alias: IngressShadowAliasOutcome::None,
            run_retention_gc: false,
        }
    }
}

#[cfg(feature = "clustering")]
#[cfg(feature = "clustering")]
async fn record_shadow_message(
    transaction: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    sm_ingress_id: waddle_xmpp::ingress::SmIngressId,
    handled_ordinal: IngressOrdinal,
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
            let key = match SmIngressRepository::lookup(transaction, sm_ingress_id, handled_ordinal)
                .await?
            {
                Some(existing) => existing,
                None => {
                    let key = minted();
                    CanonicalMessageRepository::record(transaction, key, digest).await?;
                    key
                }
            };
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
fn capture_payload_overflow(
    intents: &[waddle_xmpp::ingress::IngressEffectIntent],
) -> Result<bool, IngressUowError> {
    for intent in intents {
        match intent.with_encoded_v1(|_, payload| payload.len()) {
            Ok(_) => {}
            Err(waddle_xmpp::ingress::EffectIntentCodecError::PayloadTooLarge) => return Ok(true),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
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
        if self.capture.markers.iter().any(|marker| {
            matches!(
                marker,
                ShadowDecisionMarker::AmbiguousDispatchToRoomRemote { .. }
            )
        }) {
            return Some(IngressShadowDecisionClass::RemoteRouteAmbiguous);
        }
        if self
            .capture
            .markers
            .iter()
            .any(|marker| matches!(marker, ShadowDecisionMarker::OperationalFenceLoss))
        {
            return Some(IngressShadowDecisionClass::FrontierStale);
        }
        if self
            .capture
            .markers
            .iter()
            .any(|marker| matches!(marker, ShadowDecisionMarker::AuthorizationDenied { .. }))
        {
            return Some(IngressShadowDecisionClass::AuthorizationDenied);
        }
        // A semantic rejection normally represents a rowless decision.  A
        // handler may, however, have already committed a separately valid
        // mutation before a later payload member is rejected.  Keep those
        // frozen intents rather than making the complete submission rowless.
        if self.capture.intents.is_empty()
            && self
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
    use chrono::Duration as ChronoDuration;
    use jid::{BareJid, Jid};
    use sqlx::Connection;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::{oneshot, Notify};
    use tokio::time::Duration;
    use uuid::Uuid;
    use waddle_xmpp::auth::{AuthContextId, AuthContextVersion, PrincipalAuthEpoch};
    use waddle_xmpp::ingress::{ConnectionGeneration, EntityGeneration, NormalizedTarget};
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

    #[tokio::test]
    async fn submission_duration_uses_second_scale_buckets() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        record_submission_attempt_duration(Duration::from_millis(25));
        assert_eq!(
            metrics.histogram_count("ingress.shadow.tx.duration", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.histogram_bounds("ingress.shadow.tx.duration"),
            Some(SHADOW_TX_DURATION_BUCKETS.to_vec())
        );
        assert!(
            SHADOW_TX_DURATION_BUCKETS.contains(&2.0) && SHADOW_TX_DURATION_BUCKETS.contains(&2.5),
            "the p99 gate (2 s) and the attempt deadline (2.5 s) must be bucket edges"
        );
    }

    #[tokio::test]
    async fn timeout_cancelled_submission_attempt_records_once_and_observes_storage() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let duration_before = metrics
            .histogram_count("ingress.shadow.tx.duration", &[])
            .unwrap_or(0);
        let storage_before = metrics
            .counter_sum("ingress.shadow.decisions", &[("class", "storage")])
            .unwrap_or(0);

        let timed = tokio::time::timeout(DEFAULT_TX_DEADLINE, async {
            let timer = AttemptTimer::start();
            std::future::pending::<()>().await;
            timer.finish();
        })
        .await;

        assert!(
            timed.is_err(),
            "the simulated submission must hit its deadline"
        );
        // This is the timeout arm of IngressShadowProcessor::execute: the
        // cancelled AttemptTimer records from Drop, then the worker emits the
        // typed Storage decision for the submission.
        observe(IngressShadowObservation::Decision {
            stream_id: SmSessionId::new("timeout-storage"),
            claim_epoch: None,
            handled_ordinal: None,
            class: IngressShadowDecisionClass::Storage,
            alias: IngressShadowAliasOutcome::None,
        });

        assert_eq!(
            metrics
                .histogram_count("ingress.shadow.tx.duration", &[])
                .unwrap_or(0),
            duration_before + 1,
            "deadline cancellation must export exactly one duration sample"
        );
        assert!(
            metrics
                .histogram_bounds("ingress.shadow.tx.duration")
                .expect("duration histogram must be exported")
                .iter()
                .any(|bound| *bound >= DEFAULT_TX_DEADLINE.as_secs_f64()),
            "the seconds histogram must retain a bucket at or above the 2.5-second deadline"
        );
        assert!(
            histogram_minimum(&metrics, "ingress.shadow.tx.duration")
                .expect("duration histogram must preserve its sample minimum")
                >= DEFAULT_TX_DEADLINE.as_secs_f64(),
            "the cancelled attempt duration must be at least the 2.5-second deadline"
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.decisions", &[("class", "storage")])
                .unwrap_or(0),
            storage_before + 1,
            "deadline cancellation must terminate with the typed Storage decision"
        );
    }

    #[tokio::test]
    async fn finished_attempt_is_not_recorded_again_when_enclosing_future_is_dropped() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let duration_before = metrics
            .histogram_count("ingress.shadow.tx.duration", &[])
            .unwrap_or(0);
        let finished = Arc::new(Notify::new());
        let task = tokio::spawn({
            let finished = finished.clone();
            async move {
                let timer = AttemptTimer::start();
                timer.finish();
                finished.notify_one();
                std::future::pending::<()>().await;
            }
        });

        tokio::time::timeout(Duration::from_millis(250), finished.notified())
            .await
            .expect("attempt should finish before its enclosing future is dropped");
        task.abort();
        let _ = task.await;

        assert_eq!(
            metrics
                .histogram_count("ingress.shadow.tx.duration", &[])
                .unwrap_or(0),
            duration_before + 1,
            "a finished timer must not record a second duration when its future drops"
        );
    }

    #[tokio::test]
    async fn ingress_shadow_enabled_gauge_callback_reads_current_enabled_state() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        init_ingress_shadow_instruments(false, None);
        assert_eq!(
            observable_gauge_value(&metrics, "ingress.shadow.enabled"),
            Some(0),
            "disabled initialization must update the callback source to zero"
        );

        init_ingress_shadow_instruments(
            true,
            Some(Arc::new(std::sync::Mutex::new(
                StreamActivityState::default(),
            ))),
        );
        assert_eq!(
            observable_gauge_value(&metrics, "ingress.shadow.enabled"),
            Some(1),
            "enabled initialization must update the callback source to one"
        );
    }

    fn observable_gauge_value(
        metrics: &waddle_xmpp::telemetry::test_support::MetricsTestGuard,
        name: &str,
    ) -> Option<i64> {
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};

        // `exported()` returns every batch since the guard was acquired; an
        // observable gauge reports its current value at each collection, so
        // the latest batch carries the state under test.
        let mut latest = None;
        for resource in metrics.exported() {
            for scope in resource.scope_metrics() {
                for metric in scope.metrics() {
                    if metric.name() != name {
                        continue;
                    }
                    let AggregatedMetrics::I64(MetricData::Gauge(gauge)) = metric.data() else {
                        continue;
                    };
                    if let Some(point) = gauge.data_points().next() {
                        latest = Some(point.value());
                    }
                }
            }
        }
        latest
    }

    fn histogram_minimum(
        metrics: &waddle_xmpp::telemetry::test_support::MetricsTestGuard,
        name: &str,
    ) -> Option<f64> {
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};

        for resource in metrics.exported() {
            for scope in resource.scope_metrics() {
                for metric in scope.metrics() {
                    if metric.name() != name {
                        continue;
                    }
                    let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data()
                    else {
                        continue;
                    };
                    for point in histogram.data_points() {
                        if let Some(minimum) = point.min() {
                            return Some(minimum);
                        }
                    }
                }
            }
        }
        None
    }

    fn test_handle(
        queue_capacity: usize,
        max_concurrency: usize,
        execute: impl Fn(IngressShadowTask) -> IngressShadowExecuteFuture + Send + Sync + 'static,
    ) -> IngressShadowHandle {
        IngressShadowHandle::spawn_worker(queue_capacity, max_concurrency, Arc::new(execute))
    }

    fn test_handle_with_outstanding(
        queue_capacity: usize,
        max_concurrency: usize,
        execute: impl Fn(IngressShadowTask, Option<OutstandingSubmission>) -> IngressShadowExecuteFuture
            + Send
            + Sync
            + 'static,
    ) -> IngressShadowHandle {
        IngressShadowHandle::spawn_worker_with_enqueued_streams(
            WorkerLimits {
                queue_capacity,
                max_concurrency,
            },
            Arc::new(std::sync::Mutex::new(None)),
            Arc::new(std::sync::Mutex::new(HashSet::new())),
            Arc::new(std::sync::Mutex::new(HashSet::new())),
            None,
            Arc::new(std::sync::Mutex::new(StreamActivityState::default())),
            Arc::new(move |task, outstanding| execute(task, outstanding)),
        )
    }

    fn outstanding_count(handle: &IngressShadowHandle) -> usize {
        match handle.inner.as_ref() {
            IngressShadowInner::Worker {
                stream_activity, ..
            } => stream_activity
                .lock()
                .expect("stream activity mutex must not be poisoned")
                .outstanding
                .len(),
            IngressShadowInner::Disabled => 0,
        }
    }

    fn oldest_outstanding_age_seconds(handle: &IngressShadowHandle) -> f64 {
        let Some(stream_activity) = (match handle.inner.as_ref() {
            IngressShadowInner::Worker {
                stream_activity, ..
            } => Some(stream_activity),
            IngressShadowInner::Disabled => None,
        }) else {
            return 0.0;
        };
        stream_activity
            .lock()
            .expect("stream activity mutex must not be poisoned")
            .outstanding
            .values()
            .next()
            .map_or(0.0, |enqueued_at| enqueued_at.elapsed().as_secs_f64())
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct IngressShadowSubmissionMetrics {
        admissions: u64,
        completions: u64,
        aborted: u64,
    }

    fn submission_metrics(
        metrics: &waddle_xmpp::telemetry::test_support::MetricsTestGuard,
    ) -> IngressShadowSubmissionMetrics {
        IngressShadowSubmissionMetrics {
            admissions: metrics
                .counter_sum("ingress.shadow.admissions", &[])
                .unwrap_or(0),
            completions: metrics
                .counter_sum("ingress.shadow.completions", &[])
                .unwrap_or(0),
            aborted: metrics
                .counter_sum("ingress.shadow.aborted", &[])
                .unwrap_or(0),
        }
    }

    fn ingress_shadow_gc_runs(
        metrics: &waddle_xmpp::telemetry::test_support::MetricsTestGuard,
    ) -> u64 {
        ["completed", "partial", "failed", "timed_out"]
            .into_iter()
            .map(|outcome| {
                metrics
                    .counter_sum("ingress.shadow.gc.runs", &[("outcome", outcome)])
                    .unwrap_or(0)
            })
            .sum()
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
                tx: Arc::new(std::sync::Mutex::new(None)),
                enqueued_streams: Arc::new(std::sync::Mutex::new(HashSet::new())),
                retiring_streams: Arc::new(std::sync::Mutex::new(HashSet::new())),
                forced_alias_serialization_failures: Arc::new(AtomicUsize::new(0)),
                forced_retirement_retryable_failures: Arc::new(AtomicUsize::new(0)),
                gc_state: Arc::new(RetentionGcState::default()),
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

        fn submission_with_archive_intent(
            &self,
            ordinal: u64,
            origin: Option<&str>,
        ) -> IngressShadowSubmission {
            let mut submission = self.submission(ordinal, origin);
            submission.capture.intents.push(
                waddle_xmpp::ingress::IngressEffectIntent::ArchiveAuthoritative {
                    archive: self.principal.bare_jid().clone(),
                    stanza_id: StanzaId::new(
                        "archive-intent",
                        Jid::from(self.principal.bare_jid().clone()),
                    ),
                    by: self.principal.bare_jid().clone(),
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

        async fn message_key_for_ordinal(&self, ordinal: u64) -> Option<MessageKey> {
            let conn = self.db.guard().await.expect("database connection");
            let mut rows = conn
                .query(
                    "SELECT message_key::text FROM ingress_sm_refs WHERE sm_ingress_id = (SELECT sm_ingress_id FROM ingress_sm_streams WHERE stream_id = ?) AND ingress_ordinal = ?::numeric",
                    crate::db_params![
                        self.stream_id.as_str().to_string(),
                        ordinal.to_string(),
                    ],
                )
                .await
                .expect("read ordinal message key");
            rows.next().await.expect("ordinal row").map(|row| {
                MessageKey::from_storage(
                    row.get::<String>(0)
                        .expect("decode message key")
                        .parse()
                        .expect("message key UUID"),
                )
            })
        }

        async fn record_expired_messages(&self, count: usize) -> Vec<MessageKey> {
            let store = PostgresIngressSubstrate::open(self.db.clone())
                .expect("open fixture ingress substrate");
            let digest = waddle_xmpp::ingress::SemanticDigest::from_storage(1, [7; 32])
                .expect("valid fixture semantic digest");
            let first_terminal_at =
                Utc::now() - crate::ingress_substrate::ALIAS_RETENTION - ChronoDuration::days(1);
            let mut transaction = store.begin().await.expect("begin expired message seed");
            let mut keys = Vec::with_capacity(count);
            for index in 0..count {
                let key = MessageKey::new();
                store
                    .record_message(&mut transaction, key, &digest)
                    .await
                    .expect("record expired message");
                assert_eq!(
                    store
                        .terminalize_message(
                            &mut transaction,
                            key,
                            first_terminal_at
                                + ChronoDuration::milliseconds(
                                    i64::try_from(index).expect("fixture index fits i64"),
                                ),
                        )
                        .await
                        .expect("terminalize expired message"),
                    crate::ingress_substrate::TerminalizeOutcome::Terminalized
                );
                keys.push(key);
            }
            transaction
                .commit()
                .await
                .expect("commit expired message seed");
            keys
        }

        async fn message_is_terminal(&self, message_key: MessageKey) -> bool {
            let conn = self.db.guard().await.expect("database connection");
            let mut rows = conn
                .query(
                    "SELECT terminal_at IS NOT NULL FROM ingress_messages WHERE message_key = ?::uuid",
                    crate::db_params![message_key.to_storage().to_string()],
                )
                .await
                .expect("read terminal_at");
            rows.next()
                .await
                .expect("terminal_at row")
                .expect("message row")
                .get(0)
                .expect("decode terminal flag")
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
    fn startup_validation_fails_closed_for_missing_prerequisites() {
        assert!(matches!(
            validate_ingress_shadow_prerequisites(DatabaseDriver::Sqlite, true, true),
            Err(IngressShadowStartupError::PostgresRequired)
        ));
        assert!(matches!(
            validate_ingress_shadow_prerequisites(DatabaseDriver::Postgres, false, true),
            Err(IngressShadowStartupError::ClusteringFeatureRequired)
        ));
        assert!(matches!(
            validate_ingress_shadow_prerequisites(DatabaseDriver::Postgres, true, false),
            Err(IngressShadowStartupError::NodeIdentityRequired)
        ));
    }

    #[tokio::test]
    async fn disabled_configuration_stays_disabled_without_prerequisites() {
        let database = Database::in_memory("ingress-shadow-disabled")
            .await
            .expect("open in-memory database");
        let handle = IngressShadowHandle::new(
            IngressShadowConfig::default(),
            database,
            LineageConfig {
                deployment_uuid: None,
                action: None,
            },
            None,
        )
        .await
        .expect("disabled ingress shadow should not fail startup");
        assert!(!handle.is_enabled());
    }

    #[test]
    fn semantic_rejection_keeps_already_committed_intents() {
        let mut message = Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        message.type_ = MessageType::Groupchat;
        let mut submission = base_submission(message);
        submission
            .capture
            .intents
            .push(waddle_xmpp::ingress::IngressEffectIntent::RouteDirect {
                recipient: "romeo@example.com".parse().expect("recipient"),
                fanout: Vec::new(),
                route_identity: waddle_xmpp::ingress::EffectMessageIdentity::capture_ordinal(1),
            });
        submission.capture.markers = vec![ShadowDecisionMarker::SemanticRejected {
            reason: ShadowSemanticRejectedReason::MalformedPayload,
        }];

        assert_eq!(submission.rowless_decision_marker(), None);
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
    fn rowless_marker_promotes_operational_fence_loss_to_frontier_stale() {
        let mut message = Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        message.type_ = MessageType::Groupchat;
        let mut submission = base_submission(message);
        submission.capture.markers = vec![
            ShadowDecisionMarker::AuthorizationDenied {
                reason: ShadowAuthorizationDeniedReason::Forbidden,
            },
            ShadowDecisionMarker::OperationalFenceLoss,
        ];

        assert_eq!(
            submission.rowless_decision_marker(),
            Some(IngressShadowDecisionClass::FrontierStale)
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
    fn rowless_marker_promotes_ambiguous_remote_route() {
        let mut message = Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        message.type_ = MessageType::Groupchat;
        let mut submission = base_submission(message);
        submission.capture.markers = vec![ShadowDecisionMarker::AmbiguousDispatchToRoomRemote {
            room: "room@conference.example.com".parse().expect("room jid"),
            relay_target: waddle_xmpp::ingress::RelayTargetIdentity::owner_node(
                "node-b", "epoch-b",
            ),
        }];

        assert_eq!(
            submission.rowless_decision_marker(),
            Some(IngressShadowDecisionClass::RemoteRouteAmbiguous)
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
                ..
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
                ..
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
                ..
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
    async fn postgres_existing_alias_intent_divergence_advances_without_new_rows() {
        let Some(fixture) = ShadowFixture::open("intent_divergence").await else {
            return;
        };

        let accepted = fixture.submission_with_archive_intent(1, Some("intent-origin"));
        assert!(matches!(
            fixture.processor.execute_submission(&accepted).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::Accepted,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Inserted,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 1);
        fixture.assert_rows(1, 1, 1, 1).await;

        let subset = fixture.submission(2, Some("intent-origin"));
        assert!(matches!(
            fixture.processor.execute_submission(&subset).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::IntentDivergence,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Existing,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 2);
        fixture.assert_rows(1, 1, 2, 1).await;

        let mut payload_churn = fixture.submission_with_archive_intent(3, Some("intent-origin"));
        payload_churn.capture.intents[0] =
            waddle_xmpp::ingress::IngressEffectIntent::ArchiveAuthoritative {
                archive: fixture.principal.bare_jid().clone(),
                stanza_id: StanzaId::new(
                    "archive-intent",
                    Jid::from(fixture.principal.bare_jid().clone()),
                ),
                by: "juliet@example.com"
                    .parse()
                    .expect("fixture target bare JID"),
            };
        assert!(matches!(
            fixture.processor.execute_submission(&payload_churn).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::IntentDivergence,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Existing,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 3);
        fixture.assert_rows(1, 1, 3, 1).await;
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

        // A per-run schema lets this test poison only the shadow dependency;
        // the processor must roll the transaction back without advancing h.
        fixture
            .execute("DROP TABLE ingress_effect_intents", ())
            .await
            .expect("poison shadow effect table");
        assert!(fixture
            .processor
            .execute_submission(&fixture.submission_with_archive_intent(1, None))
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
    async fn postgres_capture_overflow_advances_frontier_without_rows() {
        let Some(fixture) = ShadowFixture::open("capture_overflow").await else {
            return;
        };

        let mut overflowed = fixture.submission(1, None);
        overflowed
            .capture
            .markers
            .push(ShadowDecisionMarker::Overflow);
        assert!(matches!(
            fixture.processor.execute_submission(&overflowed).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::CaptureOverflow,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::None,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 1);
        fixture.assert_rows(0, 0, 0, 0).await;
        fixture.close().await;
    }

    #[tokio::test]
    async fn oversized_effect_payload_advances_frontier_without_rows() {
        let Some(fixture) = ShadowFixture::open("oversized_payload").await else {
            return;
        };

        let localpart = "a".repeat(900);
        let occupants = (0..96)
            .map(|index| {
                format!("{localpart}{index}@conference.example.com/desktop")
                    .parse()
                    .expect("occupant jid")
            })
            .collect::<Vec<_>>();
        let reflection = occupants.first().cloned().expect("reflection occupant");
        let room: BareJid = "room@conference.example.com".parse().expect("room jid");
        let mut oversized = fixture.submission(1, None);
        oversized.target = NormalizedTarget::Bare(room.clone());
        oversized.message.to = Some(Jid::from(room.clone()));
        oversized.capture.intents.push(
            waddle_xmpp::ingress::IngressEffectIntent::RouteMucGroupchat {
                room,
                occupants,
                reflection,
                room_generation: EntityGeneration::INITIAL,
                route_identity: waddle_xmpp::ingress::EffectMessageIdentity::capture_ordinal(0),
            },
        );

        assert!(matches!(
            fixture.processor.execute_submission(&oversized).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::CaptureOverflow,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::None,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 1);
        fixture.assert_rows(0, 0, 0, 0).await;
        fixture.close().await;
    }

    #[tokio::test]
    async fn terminalized_messages_are_reclaimed_after_stream_retirement_and_gc() {
        let Some(fixture) = ShadowFixture::open("retirement_gc").await else {
            return;
        };

        let accepted = fixture.submission(1, Some("retire-origin"));
        assert!(matches!(
            fixture.processor.execute_submission(&accepted).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::Accepted,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Inserted,
                ..
            })
        ));
        let message_key = fixture
            .message_key_for_ordinal(1)
            .await
            .expect("accepted submission should record an SM ref");
        assert!(
            fixture.message_is_terminal(message_key).await,
            "accepted canonical rows must become terminal so GC can reclaim them later"
        );

        // Production retirement runs after terminal stream completion has
        // removed the SM-session claim; the retirement transaction re-checks
        // that absence and refuses to delete a still-claimed stream.
        fixture
            .execute(
                "DELETE FROM clustering_claims WHERE entity = ?",
                crate::db_params![format!("sm_session:{}", fixture.stream_id.as_str())],
            )
            .await
            .expect("terminal completion removes the SM-session claim before retirement");
        fixture
            .processor
            .execute_retirement(&fixture.stream_id)
            .await
            .expect("retirement should delete stream refs");
        assert_eq!(fixture.count("ingress_sm_streams").await, 0);
        assert_eq!(fixture.count("ingress_sm_refs").await, 0);

        let substrate =
            crate::ingress_substrate::PostgresIngressSubstrate::open(fixture.db.clone())
                .expect("postgres substrate");
        let outcome = substrate
            .gc_expired_aliases(
                Utc::now() + crate::ingress_substrate::ALIAS_RETENTION + ChronoDuration::seconds(1),
                AliasGcBudget {
                    deadline: tokio::time::Instant::now() + DEFAULT_TX_DEADLINE,
                    lock_timeout: Duration::from_millis(250),
                    statement_timeout: Duration::from_secs(1),
                    scan_timeout: Duration::from_secs(1),
                    progress: AliasGcProgress::default(),
                },
            )
            .await
            .expect("gc expired aliases");
        assert_eq!(outcome.deleted_messages, 1);
        fixture.assert_rows(0, 0, 0, 0).await;
        fixture.close().await;
    }

    #[tokio::test]
    async fn retirement_rechecks_claim_absence_before_deleting_shadow_rows() {
        let Some(fixture) = ShadowFixture::open("retirement_claim_recheck").await else {
            return;
        };

        let accepted = fixture.submission(1, Some("claim-still-live"));
        assert!(matches!(
            fixture.processor.execute_submission(&accepted).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::Accepted,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Inserted,
                ..
            })
        ));

        assert!(
            matches!(
                fixture
                    .processor
                    .execute_retirement(&fixture.stream_id)
                    .await
                    .expect("live claim should short-circuit retirement"),
                RetirementOutcome::DeferredClaim
            ),
            "retirement must not delete shadow rows while the exact SM claim still exists"
        );
        assert_eq!(fixture.count("ingress_sm_streams").await, 1);
        fixture.assert_rows(1, 1, 1, 0).await;
        fixture.close().await;
    }

    #[tokio::test]
    async fn retirement_claim_absence_fence_blocks_concurrent_claim_insert_until_commit() {
        let Some(fixture) = ShadowFixture::open("retirement_claim_absence_fence").await else {
            return;
        };
        fixture
            .execute(
                "DELETE FROM clustering_claims WHERE entity = ?",
                crate::db_params![format!("sm_session:{}", fixture.stream_id.as_str())],
            )
            .await
            .expect("remove exact claim to exercise absence fence");

        let uow = PostgresIngressUnitOfWork::open_with_node_identity(
            fixture.db.clone(),
            fixture.processor.lineage.clone(),
            fixture.processor.node_identity.clone(),
        )
        .expect("open fixture ingress uow");
        let mut transaction = uow.begin().await.expect("begin retirement fence tx");
        transaction
            .set_local_timeouts(DEFAULT_LOCK_TIMEOUT_MS, DEFAULT_STATEMENT_TIMEOUT_MS)
            .await
            .expect("set tx timeouts");
        assert!(
            SmIngressStreamRepository::fence_claim_absence_for_retirement(
                &mut transaction,
                &fixture.stream_id,
            )
            .await
            .expect("absence fence query"),
            "fixture must present an actually absent claim row"
        );

        let claim_entity = format!("sm_session:{}", fixture.stream_id.as_str());
        let claim_insert = tokio::spawn({
            let database_url = fixture.db.database_url().to_string();
            let owner = fixture.owner.clone();
            async move {
                let mut conn = sqlx::PgConnection::connect(&database_url)
                    .await
                    .expect("open competing claim writer");
                sqlx::query(
                    "INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(claim_entity)
                .bind("sm_session")
                .bind(owner.node_id)
                .bind(owner.node_epoch)
                .bind(99_i64)
                .execute(&mut conn)
                .await
                .expect("claim insert completes after fence release");
            }
        });

        wait_for_lock_waiter(&fixture.admin, "INSERT INTO clustering_claims").await;
        assert!(
            !claim_insert.is_finished(),
            "concurrent claim insert must wait behind the retirement absence fence"
        );

        transaction
            .commit()
            .await
            .expect("release retirement absence fence");
        claim_insert
            .await
            .expect("join competing claim writer after fence release");
        fixture.close().await;
    }

    #[tokio::test]
    async fn postgres_ambiguous_remote_route_advances_frontier_without_rows() {
        let Some(fixture) = ShadowFixture::open("ambiguous_remote_route").await else {
            return;
        };

        let mut ambiguous = fixture.submission(1, None);
        ambiguous
            .capture
            .markers
            .push(ShadowDecisionMarker::AmbiguousDispatchToRoomRemote {
                room: "room@conference.example.com".parse().expect("room jid"),
                relay_target: waddle_xmpp::ingress::RelayTargetIdentity::owner_node(
                    "node-b", "epoch-b",
                ),
            });
        assert!(matches!(
            fixture.processor.execute_submission(&ambiguous).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::RemoteRouteAmbiguous,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::None,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 1);
        fixture.assert_rows(0, 0, 0, 0).await;
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
    async fn mixed_retry_sequence_still_counts_as_exhausted() {
        let guard = waddle_xmpp::telemetry::test_support::acquire().await;
        let Some(fixture) = ShadowFixture::open("mixed_retry_metrics").await else {
            return;
        };
        fixture
            .execute("DROP TABLE ingress_effect_intents", ())
            .await
            .expect("poison shadow effect table after the first retryable attempt");
        fixture
            .processor
            .forced_alias_serialization_failures
            .store(1, Ordering::SeqCst);
        // Counters are process-global and cumulative; assert the DELTA this
        // execution produces so sibling tests cannot skew the baseline.
        let before = guard
            .counter_sum("ingress.shadow.tx.retries", &[("outcome", "exhausted")])
            .unwrap_or(0);

        fixture
            .processor
            .execute(
                IngressShadowTask::Submit(Box::new(
                    fixture.submission_with_archive_intent(1, Some("mixed-retry-origin")),
                )),
                None,
            )
            .await;

        let after = guard
            .counter_sum("ingress.shadow.tx.retries", &[("outcome", "exhausted")])
            .unwrap_or(0);
        assert_eq!(
            after - before,
            1,
            "attempt history, not only the terminal error class, must count exhausted retry sequences"
        );
        assert_eq!(fixture.frontier().await, 0);
        // The poisoned ingress_effect_intents table no longer exists; the
        // surviving tables plus the unmoved frontier carry the rollback proof.
        assert_eq!(fixture.count("ingress_messages").await, 0);
        assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
        assert_eq!(fixture.count("ingress_sm_refs").await, 0);
        fixture.close().await;
    }

    #[tokio::test]
    async fn missing_stream_row_is_reenrolled_before_submission_runs() {
        let Some(fixture) = ShadowFixture::open("reenroll_missing_stream").await else {
            return;
        };
        fixture
            .execute(
                "DELETE FROM ingress_sm_streams WHERE stream_id = ?",
                crate::db_params![fixture.stream_id.as_str().to_string()],
            )
            .await
            .expect("remove enrolled stream row");

        assert!(matches!(
            fixture
                .processor
                .execute_submission(&fixture.submission(1, Some("reenroll-origin")))
                .await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::Accepted,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Inserted,
                ..
            })
        ));
        assert_eq!(fixture.frontier().await, 1);
        fixture.assert_rows(1, 1, 1, 0).await;
        fixture.close().await;
    }

    #[tokio::test]
    async fn no_origin_retransmit_reuses_the_existing_ordinal_binding() {
        let Some(fixture) = ShadowFixture::open("no_origin_retransmit").await else {
            return;
        };
        let first = fixture.submission(1, None);
        assert!(matches!(
            fixture.processor.execute_submission(&first).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::Accepted,
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::None,
                ..
            })
        ));
        let original_key = fixture
            .message_key_for_ordinal(1)
            .await
            .expect("first no-origin submit should bind ordinal 1");

        assert!(matches!(
            fixture.processor.execute_submission(&first).await,
            Ok(ShadowSubmissionOutcome {
                decision: IngressShadowDecisionClass::Accepted,
                commit_kind: Some(IngressShadowCommitKind::Idempotent),
                alias: IngressShadowAliasOutcome::None,
                ..
            })
        ));
        assert_eq!(
            fixture.message_key_for_ordinal(1).await,
            Some(original_key),
            "the retransmit must reuse the canonical message already bound to the handled ordinal"
        );
        fixture.assert_rows(1, 0, 1, 0).await;
        fixture.close().await;
    }

    #[tokio::test]
    async fn successful_submission_runs_production_retention_gc() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let Some(fixture) = ShadowFixture::open("production_retention_gc").await else {
            return;
        };
        let stale = fixture.submission(1, None);
        assert!(matches!(
            fixture.processor.execute_submission(&stale).await,
            Ok(ShadowSubmissionOutcome {
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                ..
            })
        ));
        let stale_key = fixture
            .message_key_for_ordinal(1)
            .await
            .expect("seed submission should create a canonical row");
        fixture
            .execute(
                "DELETE FROM ingress_sm_refs WHERE message_key = ?::uuid",
                crate::db_params![stale_key.to_storage().to_string()],
            )
            .await
            .expect("orphan the terminalized message so GC can reclaim it");
        fixture
            .execute(
                "UPDATE ingress_messages SET terminal_at = ?::timestamptz WHERE message_key = ?::uuid",
                crate::db_params![
                    (Utc::now()
                        - crate::ingress_substrate::ALIAS_RETENTION
                        - ChronoDuration::seconds(1))
                    .to_rfc3339(),
                    stale_key.to_storage().to_string(),
                ],
            )
            .await
            .expect("age the terminal row beyond retention");

        // The commit itself no longer runs GC inline: the worker reports the
        // committed outcome first and honors `run_retention_gc` afterwards.
        // Assert the outcome requests GC, then drive it the way the worker
        // does.
        assert!(matches!(
            fixture
                .processor
                .execute_submission(&fixture.submission(2, Some("gc-trigger-origin")))
                .await,
            Ok(ShadowSubmissionOutcome {
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                run_retention_gc: true,
                ..
            })
        ));
        let completed_before = metrics
            .counter_sum("ingress.shadow.gc.runs", &[("outcome", "completed")])
            .unwrap_or(0);
        let reclaimed_before = metrics
            .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
            .unwrap_or(0);
        fixture.processor.run_retention_gc().await;
        assert_eq!(fixture.frontier().await, 2);
        fixture.assert_rows(1, 1, 1, 0).await;
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "completed")])
                .unwrap_or(0),
            completed_before + 1,
            "production GC must record its completed outcome"
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
                .unwrap_or(0),
            reclaimed_before + 1,
            "production GC must report the terminalized message it reclaimed"
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn retention_gc_reports_partial_progress_then_completes_without_double_counting() {
        const BACKLOG: usize = 258;

        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let Some(fixture) = ShadowFixture::open("retention_gc_partial").await else {
            return;
        };
        fixture.record_expired_messages(BACKLOG).await;
        fixture
            .execute(
                "CREATE FUNCTION waddle_test_slow_gc_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.05); RETURN OLD; END $$",
                (),
            )
            .await
            .expect("create deterministic GC pacing function");
        fixture
            .execute(
                "CREATE TRIGGER waddle_test_slow_gc_delete BEFORE DELETE ON ingress_messages FOR EACH ROW EXECUTE FUNCTION waddle_test_slow_gc_delete()",
                (),
            )
            .await
            .expect("install deterministic GC pacing trigger");
        let partial_before = metrics
            .counter_sum("ingress.shadow.gc.runs", &[("outcome", "partial")])
            .unwrap_or(0);
        let completed_before = metrics
            .counter_sum("ingress.shadow.gc.runs", &[("outcome", "completed")])
            .unwrap_or(0);
        let reclaimed_before = metrics
            .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
            .unwrap_or(0);

        fixture
            .processor
            .run_retention_gc_with_budget(RetentionGcBudget {
                cooperative: Duration::from_secs(1),
                ..RetentionGcBudget::DEFAULT
            })
            .await;
        let remaining = usize::try_from(fixture.count("ingress_messages").await)
            .expect("remaining message count fits usize");
        let deleted = BACKLOG - remaining;
        assert!(deleted > 0 && deleted < BACKLOG);
        assert_eq!(deleted + remaining, BACKLOG);
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "partial")])
                .unwrap_or(0),
            partial_before + 1
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
                .unwrap_or(0),
            reclaimed_before + u64::try_from(deleted).expect("deleted count fits u64")
        );

        fixture
            .execute(
                "DROP TRIGGER waddle_test_slow_gc_delete ON ingress_messages",
                (),
            )
            .await
            .expect("remove deterministic GC pacing trigger");
        fixture
            .execute("DROP FUNCTION waddle_test_slow_gc_delete()", ())
            .await
            .expect("remove deterministic GC pacing function");
        fixture
            .processor
            .run_retention_gc_with_budget(RetentionGcBudget {
                cooperative: Duration::from_secs(30),
                ..RetentionGcBudget::DEFAULT
            })
            .await;

        assert_eq!(fixture.count("ingress_messages").await, 0);
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "completed")])
                .unwrap_or(0),
            completed_before + 1
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
                .unwrap_or(0),
            reclaimed_before + u64::try_from(BACKLOG).expect("backlog count fits u64")
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn retention_gc_records_timed_out_outcome_when_the_epoch_row_is_locked() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let Some(fixture) = ShadowFixture::open("retention_gc_timeout").await else {
            return;
        };
        assert!(matches!(
            fixture
                .processor
                .execute_submission(&fixture.submission(1, Some("gc-timeout-origin")))
                .await,
            Ok(ShadowSubmissionOutcome {
                commit_kind: Some(IngressShadowCommitKind::Advanced),
                alias: IngressShadowAliasOutcome::Inserted,
                ..
            })
        ));
        let message_key = fixture
            .message_key_for_ordinal(1)
            .await
            .expect("seed submission should create a canonical row");
        fixture
            .execute(
                "DELETE FROM ingress_sm_refs WHERE message_key = ?::uuid",
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await
            .expect("orphan terminalized message so retention GC can reclaim it");
        fixture
            .execute(
                "UPDATE ingress_messages SET terminal_at = ?::timestamptz WHERE message_key = ?::uuid",
                crate::db_params![
                    (Utc::now()
                        - crate::ingress_substrate::ALIAS_RETENTION
                        - ChronoDuration::seconds(1))
                    .to_rfc3339(),
                    message_key.to_storage().to_string(),
                ],
            )
            .await
            .expect("age terminal message beyond alias retention");

        // GC waits on the epoch row FOR SHARE first (a locked candidate row is
        // skipped instead), so an exclusive epoch lock is what forces the
        // per-operation lock timeout.
        let mut lock = fixture.db.begin().await.expect("begin epoch lock tx");
        lock.query(
            "SELECT 1 FROM ingress_protocol_epoch WHERE id = 1 FOR UPDATE",
            (),
        )
        .await
        .expect("lock the protocol epoch row");
        let timed_out_before = metrics
            .counter_sum("ingress.shadow.gc.runs", &[("outcome", "timed_out")])
            .unwrap_or(0);
        let completed_before = metrics
            .counter_sum("ingress.shadow.gc.runs", &[("outcome", "completed")])
            .unwrap_or(0);
        let reclaimed_before = metrics
            .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
            .unwrap_or(0);

        let processor = fixture.processor.clone();
        let gc = tokio::spawn(async move { processor.run_retention_gc().await });
        wait_for_lock_waiter(&fixture.admin, "FOR SHARE").await;
        gc.await
            .expect("join production retention GC after its lock timeout");
        lock.commit().await.expect("release epoch row lock");

        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "timed_out")])
                .unwrap_or(0),
            timed_out_before + 1,
            "blocked production GC must record the timed_out outcome"
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "completed")])
                .unwrap_or(0),
            completed_before,
            "a timed-out production GC must not record a completed outcome"
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
                .unwrap_or(0),
            reclaimed_before,
            "a timeout before the first commit must not report reclaimed messages"
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn retention_gc_queues_a_rerun_while_another_run_is_in_flight() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let Some(fixture) = ShadowFixture::open("retention_gc_coalescing").await else {
            return;
        };
        fixture.record_expired_messages(1).await;
        let runs_before = ingress_shadow_gc_runs(&metrics);
        let completed_before = metrics
            .counter_sum("ingress.shadow.gc.runs", &[("outcome", "completed")])
            .unwrap_or(0);
        let state = &fixture.processor.gc_state;

        state.in_flight.store(true, Ordering::Release);
        fixture.processor.run_retention_gc().await;
        assert_eq!(
            ingress_shadow_gc_runs(&metrics),
            runs_before,
            "a coalesced trigger must not run or record a GC run"
        );
        assert_eq!(
            fixture.count("ingress_messages").await,
            1,
            "a coalesced trigger must not touch the database"
        );
        assert!(
            state.in_flight.load(Ordering::Acquire),
            "a coalesced trigger must not release a flag it did not acquire"
        );
        assert!(
            state.rerun_requested.load(Ordering::Acquire),
            "a coalesced trigger must be kept as a pending rerun"
        );

        // The in-flight run finishing observes the pending rerun and loops:
        // one run reclaims the row, the rerun finds nothing and completes.
        state.in_flight.store(false, Ordering::Release);
        fixture.processor.run_retention_gc().await;
        assert_eq!(fixture.count("ingress_messages").await, 0);
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "completed")])
                .unwrap_or(0),
            completed_before + 1,
            "a fresh trigger with no pending rerun runs exactly once"
        );
        assert!(!state.in_flight.load(Ordering::Acquire));
        assert!(!state.rerun_requested.load(Ordering::Acquire));

        fixture.record_expired_messages(1).await;
        state.rerun_requested.store(true, Ordering::Release);
        fixture.processor.run_retention_gc().await;
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "completed")])
                .unwrap_or(0),
            completed_before + 2,
            "a stale pending rerun is cleared before the run, not replayed after it"
        );
        assert_eq!(fixture.count("ingress_messages").await, 0);
        assert!(!state.rerun_requested.load(Ordering::Acquire));
        fixture.close().await;
    }

    #[tokio::test]
    async fn retention_gc_hard_envelope_reports_committed_progress_and_releases_the_flag() {
        const BACKLOG: usize = 8;

        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let Some(fixture) = ShadowFixture::open("retention_gc_hard_envelope").await else {
            return;
        };
        fixture.record_expired_messages(BACKLOG).await;
        fixture
            .execute(
                "CREATE FUNCTION waddle_test_slow_gc_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.05); RETURN OLD; END $$",
                (),
            )
            .await
            .expect("create deterministic GC pacing function");
        fixture
            .execute(
                "CREATE TRIGGER waddle_test_slow_gc_delete BEFORE DELETE ON ingress_messages FOR EACH ROW EXECUTE FUNCTION waddle_test_slow_gc_delete()",
                (),
            )
            .await
            .expect("install deterministic GC pacing trigger");
        let timed_out_before = metrics
            .counter_sum("ingress.shadow.gc.runs", &[("outcome", "timed_out")])
            .unwrap_or(0);
        let reclaimed_before = metrics
            .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
            .unwrap_or(0);

        fixture
            .processor
            .run_retention_gc_with_budget(RetentionGcBudget {
                cooperative: Duration::from_secs(30),
                hard_deadline: Duration::from_millis(150),
                ..RetentionGcBudget::DEFAULT
            })
            .await;

        let remaining = usize::try_from(fixture.count("ingress_messages").await)
            .expect("remaining message count fits usize");
        assert!(remaining > 0 && remaining < BACKLOG);
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "timed_out")])
                .unwrap_or(0),
            timed_out_before + 1
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
                .unwrap_or(0),
            reclaimed_before + u64::try_from(BACKLOG - remaining).expect("count fits u64"),
            "a hard-envelope cancellation must still report the committed deletions"
        );
        assert!(
            !fixture.processor.gc_state.in_flight.load(Ordering::Acquire),
            "a cancelled run must release the in-flight flag"
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn retention_gc_failure_after_progress_reports_reclaimed_messages() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let Some(fixture) = ShadowFixture::open("retention_gc_failure_progress").await else {
            return;
        };
        let keys = fixture.record_expired_messages(2).await;
        fixture
            .execute(
                &format!(
                    "CREATE FUNCTION waddle_test_fail_gc_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.message_key = '{}'::uuid THEN RAISE EXCEPTION 'forced GC failure' USING ERRCODE = 'P0001'; END IF; RETURN OLD; END $$",
                    keys[1].to_storage()
                ),
                (),
            )
            .await
            .expect("create deterministic GC failure function");
        fixture
            .execute(
                "CREATE TRIGGER waddle_test_fail_gc_delete AFTER DELETE ON ingress_messages FOR EACH ROW EXECUTE FUNCTION waddle_test_fail_gc_delete()",
                (),
            )
            .await
            .expect("install deterministic GC failure trigger");
        let failed_before = metrics
            .counter_sum("ingress.shadow.gc.runs", &[("outcome", "failed")])
            .unwrap_or(0);
        let reclaimed_before = metrics
            .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
            .unwrap_or(0);

        fixture.processor.run_retention_gc().await;

        assert_eq!(fixture.count("ingress_messages").await, 1);
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "failed")])
                .unwrap_or(0),
            failed_before + 1
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.reclaimed_messages", &[])
                .unwrap_or(0),
            reclaimed_before + 1,
            "a returned substrate failure must retain committed progress"
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn retention_gc_records_failed_outcome_when_database_is_not_postgres() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let database = Database::from_config(
            "ingress-shadow-gc-non-postgres",
            &DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:"),
        )
        .await
        .expect("open deterministic non-Postgres database");
        let processor = IngressShadowProcessor {
            database,
            lineage: LineageConfig {
                deployment_uuid: None,
                action: None,
            },
            node_identity: SharedNodeIdentity::new(NodeIdentity::new("gc-node", "epoch-a")),
            retry_attempts: 0,
            tx: Arc::new(std::sync::Mutex::new(None)),
            enqueued_streams: Arc::new(std::sync::Mutex::new(HashSet::new())),
            retiring_streams: Arc::new(std::sync::Mutex::new(HashSet::new())),
            forced_alias_serialization_failures: Arc::new(AtomicUsize::new(0)),
            forced_retirement_retryable_failures: Arc::new(AtomicUsize::new(0)),
            gc_state: Arc::new(RetentionGcState::default()),
        };
        let failed_before = metrics
            .counter_sum("ingress.shadow.gc.runs", &[("outcome", "failed")])
            .unwrap_or(0);

        processor.run_retention_gc().await;

        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.gc.runs", &[("outcome", "failed")])
                .unwrap_or(0),
            failed_before + 1,
            "Postgres-only GC setup failure must export the failed outcome"
        );
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
    async fn drain_and_join_is_bounded_and_finishes_after_running_work_completes() {
        let release_submit = Arc::new(Notify::new());
        let submit_started = Arc::new(Notify::new());
        let handle = test_handle(4, 1, {
            let release_submit = release_submit.clone();
            let submit_started = submit_started.clone();
            move |task| {
                let release_submit = release_submit.clone();
                let submit_started = submit_started.clone();
                Box::pin(async move {
                    match task {
                        IngressShadowTask::Submit(_) => {
                            submit_started.notify_waiters();
                            release_submit.notified().await;
                        }
                        IngressShadowTask::Enroll { .. } | IngressShadowTask::Retire { .. } => {}
                    }
                })
            }
        });
        let shutdown = handle
            .shutdown()
            .expect("worker should expose shutdown state");
        let stream_id = SmSessionId::new("drain-stream");
        assert_eq!(
            handle.try_enroll_stream(stream_id.clone()),
            IngressShadowDisposition::Enqueued
        );
        let mut submission = base_submission(Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        ))));
        submission.stream_id = stream_id;
        assert_eq!(
            handle.try_submit(submission),
            IngressShadowDisposition::Enqueued
        );
        tokio::time::timeout(Duration::from_millis(250), submit_started.notified())
            .await
            .expect("submit should start");

        assert!(
            !handle.drain_and_join(Duration::from_millis(20)).await,
            "bounded drain should time out while work is still running"
        );
        release_submit.notify_waiters();
        tokio::time::timeout(Duration::from_millis(250), shutdown.wait_for_completion())
            .await
            .expect("worker should finish after the running task completes");
    }

    #[tokio::test]
    async fn active_submission_is_aborted_by_forced_shutdown() {
        let submit_started = Arc::new(Notify::new());
        let handle = test_handle_with_outstanding(2, 1, {
            let submit_started = submit_started.clone();
            move |task, outstanding| {
                let submit_started = submit_started.clone();
                Box::pin(async move {
                    if matches!(task, IngressShadowTask::Submit(_)) {
                        let _outstanding = outstanding.expect("submit must have an obligation");
                        submit_started.notify_waiters();
                        std::future::pending::<()>().await;
                    }
                })
            }
        });
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;

        let started = submit_started.notified();
        assert_eq!(
            handle.try_submit(base_submission(Message::new(Some(Jid::from(
                "room@conference.example.com"
                    .parse::<BareJid>()
                    .expect("room jid")
            ))))),
            IngressShadowDisposition::Enqueued
        );
        tokio::time::timeout(Duration::from_millis(250), started)
            .await
            .expect("submission should start");
        let before = submission_metrics(&metrics);

        assert!(
            !handle.drain_and_join(Duration::from_millis(20)).await,
            "tiny timeout should force stop while active"
        );
        // No extra barrier: `drain_and_join(false)` itself must have waited
        // for every admitted submission to release (forced-teardown contract).
        assert_eq!(
            submission_metrics(&metrics),
            IngressShadowSubmissionMetrics {
                admissions: before.admissions,
                completions: before.completions + 1,
                aborted: before.aborted + 1,
            },
            "forced shutdown must record one completed and aborted admitted submission"
        );
        tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                if outstanding_count(&handle) == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("outstanding map should empty after forced shutdown");
    }

    #[tokio::test]
    async fn failed_submission_then_outstanding_drop_completes_once_as_aborted() {
        let failed_observed = Arc::new(Notify::new());
        let handle = test_handle_with_outstanding(2, 1, {
            let failed_observed = failed_observed.clone();
            move |task, outstanding| {
                let failed_observed = failed_observed.clone();
                Box::pin(async move {
                    if matches!(task, IngressShadowTask::Submit(_)) {
                        let outstanding = outstanding.expect("submit must have an obligation");
                        observe(IngressShadowObservation::Failed {
                            kind: IngressShadowRequestKind::Submit,
                            stream_id: outstanding.stream_id.clone(),
                            claim_epoch: None,
                            handled_ordinal: None,
                        });
                        failed_observed.notify_one();
                        drop(outstanding);
                    }
                })
            }
        });
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let before = submission_metrics(&metrics);
        let failed = failed_observed.notified();

        assert_eq!(
            handle.try_submit(base_submission(Message::new(Some(Jid::from(
                "room@conference.example.com"
                    .parse::<BareJid>()
                    .expect("room jid"),
            ))))),
            IngressShadowDisposition::Enqueued
        );
        tokio::time::timeout(Duration::from_millis(250), failed)
            .await
            .expect("worker should observe Failed before its outstanding guard drops");
        assert!(handle.drain_and_join(Duration::from_millis(250)).await);

        assert_eq!(
            submission_metrics(&metrics),
            IngressShadowSubmissionMetrics {
                admissions: before.admissions + 1,
                completions: before.completions + 1,
                aborted: before.aborted + 1,
            },
            "Failed is not a completion; the armed guard must supply one aborted completion"
        );
        assert_eq!(
            outstanding_count(&handle),
            0,
            "dropping the armed guard must release its obligation exactly once"
        );
    }

    /// Forced teardown right after admission: whether the scheduler has not
    /// dispatched the task yet, has spawned it but it was never polled, or it
    /// is in flight, exactly one completion (aborted or decided) must exist
    /// per admission by the time `drain_and_join(false)` returns.
    #[tokio::test]
    async fn submission_forced_down_immediately_after_admission_balances_regardless_of_poll_state()
    {
        let handle = test_handle_with_outstanding(4, 1, |task, outstanding| {
            Box::pin(async move {
                if matches!(task, IngressShadowTask::Submit(_)) {
                    let _outstanding = outstanding.expect("submit must have an obligation");
                    std::future::pending::<()>().await;
                }
            })
        });
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let before = submission_metrics(&metrics);
        for index in 0..3 {
            let mut submission = base_submission(Message::new(Some(jid::Jid::from(
                "room@conference.example.com"
                    .parse::<BareJid>()
                    .expect("room jid"),
            ))));
            submission.stream_id = SmSessionId::new(format!("immediate-{index}"));
            assert_eq!(
                handle.try_submit(submission),
                IngressShadowDisposition::Enqueued
            );
        }
        assert_eq!(outstanding_count(&handle), 3);

        assert!(!handle.drain_and_join(Duration::ZERO).await);
        let after = submission_metrics(&metrics);
        assert_eq!(after.admissions, before.admissions + 3);
        assert_eq!(
            after.completions,
            before.completions + 3,
            "every admission must be terminal once the forced barrier returns"
        );
        assert_eq!(after.aborted, before.aborted + 3);
        assert_eq!(outstanding_count(&handle), 0);
    }

    /// Deterministic spawned-before-first-poll abort on the production
    /// primitives: `force_stop` has already fired when the scheduler tracks a
    /// freshly spawned task, so `track_active_task` aborts it before its
    /// first poll (mod.rs `track_active_task`). The admitted obligation it
    /// carries must still be released exactly once as `Aborted`, and the
    /// outstanding-map barrier must observe that.
    #[tokio::test]
    async fn submission_spawned_but_never_polled_is_aborted_exactly_once() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let before = submission_metrics(&metrics);
        let stream_activity = Arc::new(std::sync::Mutex::new(StreamActivityState::default()));
        let shutdown = Arc::new(IngressShadowShutdown::default());
        let stream_id = SmSessionId::new("spawned-unpolled");
        let outstanding = register_outstanding(&stream_activity, stream_id);
        let polled = Arc::new(AtomicBool::new(false));
        let task_polled = polled.clone();
        let task = tokio::spawn(async move {
            task_polled.store(true, Ordering::Release);
            let _outstanding = outstanding;
            std::future::pending::<()>().await;
        });

        shutdown.force_stop();
        shutdown.track_active_task(Arc::new(AtomicBool::new(false)), task.abort_handle());
        tokio::time::timeout(
            FORCED_TEARDOWN_JOIN,
            wait_for_outstanding_drained(&stream_activity),
        )
        .await
        .expect("the aborted, never-polled task must release its obligation");

        let join_error = task.await.expect_err("the task must be cancelled");
        assert!(join_error.is_cancelled());
        assert!(
            !polled.load(Ordering::Acquire),
            "the task must have been aborted before its first poll"
        );
        let after = submission_metrics(&metrics);
        assert_eq!(after.aborted, before.aborted + 1);
        assert_eq!(after.completions, before.completions + 1);
        assert_eq!(outstanding_count_in(&stream_activity), 0);
    }

    fn outstanding_count_in(stream_activity: &Arc<std::sync::Mutex<StreamActivityState>>) -> usize {
        stream_activity_lock(stream_activity).outstanding.len()
    }

    #[tokio::test]
    async fn queued_submission_is_aborted_by_forced_shutdown() {
        let submit_started = Arc::new(Notify::new());
        let handle = test_handle_with_outstanding(2, 1, {
            let submit_started = submit_started.clone();
            move |task, outstanding| {
                let submit_started = submit_started.clone();
                Box::pin(async move {
                    if matches!(task, IngressShadowTask::Submit(_)) {
                        let _outstanding = outstanding.expect("submit must have an obligation");
                        submit_started.notify_waiters();
                        std::future::pending::<()>().await;
                    }
                })
            }
        });
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let message = Message::new(Some(jid::Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        let mut first = base_submission(message);
        let mut second = base_submission(Message::new(Some(jid::Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        ))));
        let stream_id = SmSessionId::new("queued-stream");
        first.stream_id = stream_id.clone();
        second.stream_id = stream_id;

        let started = submit_started.notified();
        assert_eq!(handle.try_submit(first), IngressShadowDisposition::Enqueued);
        tokio::time::timeout(Duration::from_millis(250), started)
            .await
            .expect("first queued submission should start");
        assert_eq!(
            handle.try_submit(second),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            outstanding_count(&handle),
            2,
            "the second admitted submission must remain queued behind the active stream"
        );
        let before = submission_metrics(&metrics);

        assert!(
            !handle.drain_and_join(Duration::from_millis(20)).await,
            "tiny timeout should force stop while active"
        );
        // No extra barrier: `drain_and_join(false)` itself must have waited
        // for every admitted submission to release (forced-teardown contract).
        assert_eq!(
            submission_metrics(&metrics),
            IngressShadowSubmissionMetrics {
                admissions: before.admissions,
                completions: before.completions + 2,
                aborted: before.aborted + 2,
            },
            "the active and queued submissions must each be aborted exactly once"
        );
        tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                if outstanding_count(&handle) == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("outstanding map should empty after forced shutdown");
    }

    #[tokio::test]
    async fn closed_intake_rejects_submit() {
        let handle = test_handle_with_outstanding(1, 1, |_task, _outstanding| Box::pin(async {}));
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let before = submission_metrics(&metrics);
        let IngressShadowInner::Worker { tx, .. } = handle.inner.as_ref() else {
            panic!("worker test handle should be available");
        };
        close_worker_intake(tx);

        assert_eq!(
            handle.try_submit(base_submission(Message::new(Some(jid::Jid::from(
                "room@conference.example.com"
                    .parse::<BareJid>()
                    .expect("room jid")
            ))))),
            IngressShadowDisposition::Closed
        );
        assert_eq!(
            submission_metrics(&metrics),
            before,
            "a closed intake must not admit, complete, or abort a submission"
        );
        assert_eq!(
            outstanding_count(&handle),
            0,
            "closed intake should not leak outstanding submissions"
        );
        assert!(handle.drain_and_join(Duration::from_millis(250)).await);
    }

    #[tokio::test]
    async fn completion_does_not_double_count_when_outstanding_is_dropped_after_task_finishes() {
        let submit_started = Arc::new(Notify::new());
        let submit_done = Arc::new(Notify::new());
        let release_submit = Arc::new(Notify::new());
        let handle = test_handle_with_outstanding(2, 1, {
            let submit_started = submit_started.clone();
            let submit_done = submit_done.clone();
            let release_submit = release_submit.clone();
            move |task, outstanding| {
                let submit_started = submit_started.clone();
                let submit_done = submit_done.clone();
                let release_submit = release_submit.clone();
                Box::pin(async move {
                    if matches!(task, IngressShadowTask::Submit(_)) {
                        let outstanding = outstanding.expect("submit must have an obligation");
                        submit_started.notify_waiters();
                        release_submit.notified().await;
                        observe(IngressShadowObservation::Decision {
                            stream_id: outstanding.stream_id.clone(),
                            claim_epoch: None,
                            handled_ordinal: None,
                            class: IngressShadowDecisionClass::Accepted,
                            alias: IngressShadowAliasOutcome::None,
                        });
                        finish_outstanding(
                            &outstanding.stream_activity,
                            &outstanding.stream_id,
                            outstanding.seq,
                            OutstandingEnd::Decision,
                        );
                        submit_done.notify_waiters();
                    }
                })
            }
        });
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let started = submit_started.notified();
        let done = submit_done.notified();
        assert_eq!(
            handle.try_submit(base_submission(Message::new(Some(Jid::from(
                "room@conference.example.com"
                    .parse::<BareJid>()
                    .expect("room jid")
            ))))),
            IngressShadowDisposition::Enqueued
        );
        tokio::time::timeout(Duration::from_millis(250), started)
            .await
            .expect("submit task should start");
        let before = submission_metrics(&metrics);
        release_submit.notify_waiters();
        tokio::time::timeout(Duration::from_millis(250), done)
            .await
            .expect("submit task should finish quickly");

        assert!(handle.drain_and_join(Duration::from_millis(250)).await);
        assert_eq!(
            outstanding_count(&handle),
            0,
            "the terminal decision must release the outstanding submission before its handle drops"
        );
        assert_eq!(
            submission_metrics(&metrics),
            IngressShadowSubmissionMetrics {
                admissions: before.admissions,
                completions: before.completions + 1,
                aborted: before.aborted,
            },
            "the terminal decision must complete once and suppress the drop abort"
        );
    }

    #[tokio::test]
    async fn oldest_outstanding_submission_age_tracks_inflight_work() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let handle = test_handle_with_outstanding(2, 1, {
            let started = started.clone();
            let release = release.clone();
            move |task, outstanding| {
                let started = started.clone();
                let release = release.clone();
                Box::pin(async move {
                    if matches!(task, IngressShadowTask::Submit(_)) {
                        let outstanding = outstanding.expect("submit must have an obligation");
                        started.notify_waiters();
                        release.notified().await;
                        finish_outstanding(
                            &outstanding.stream_activity,
                            &outstanding.stream_id,
                            outstanding.seq,
                            OutstandingEnd::Decision,
                        );
                    }
                })
            }
        });
        let message = Message::new(Some(Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        let mut submission = base_submission(message);
        submission.stream_id = SmSessionId::new("age-stream");
        let submit_started = started.notified();
        assert_eq!(
            handle.try_submit(submission),
            IngressShadowDisposition::Enqueued
        );
        tokio::time::timeout(Duration::from_millis(250), submit_started)
            .await
            .expect("submission should start");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let age_during = oldest_outstanding_age_seconds(&handle);
        assert!(age_during > 0.0);
        release.notify_waiters();

        assert!(handle.drain_and_join(Duration::from_millis(250)).await);

        tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                if oldest_outstanding_age_seconds(&handle) == 0.0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("outstanding map should be empty after completion");

        assert_eq!(
            oldest_outstanding_age_seconds(&handle),
            0.0,
            "no outstanding submissions should result in age 0"
        );
    }

    #[tokio::test]
    async fn tasks_registered_after_force_stop_are_aborted_immediately() {
        let shutdown = Arc::new(IngressShadowShutdown::default());
        shutdown.force_stop();

        let task_handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        shutdown.track_active_task(Arc::new(AtomicBool::new(false)), task_handle.abort_handle());

        let join_error = tokio::time::timeout(Duration::from_millis(250), task_handle)
            .await
            .expect("force-stopped registration should abort promptly")
            .expect_err("aborted task should not complete successfully");
        assert!(join_error.is_cancelled());
        assert!(
            shutdown
                .active_task_aborts
                .lock()
                .expect("shadow active task abort handles must not be poisoned")
                .is_empty(),
            "late-registered tasks must not remain tracked after force-stop"
        );
    }

    #[tokio::test]
    async fn postgres_claim_lock_timeout_keeps_enqueue_prompt() {
        let Some(fixture) = ShadowFixture::open("claim_lock_timeout").await else {
            return;
        };
        let (started_tx, mut started_rx) = mpsc::unbounded_channel::<&'static str>();
        let (finished_tx, mut finished_rx) = mpsc::unbounded_channel::<&'static str>();
        let processor = fixture.processor.clone();
        let handle = test_handle(4, 1, move |task| {
            let processor = processor.clone();
            let started_tx = started_tx.clone();
            let finished_tx = finished_tx.clone();
            Box::pin(async move {
                let label = match &task {
                    IngressShadowTask::Enroll { .. } => "enroll",
                    IngressShadowTask::Submit(_) => "submit",
                    IngressShadowTask::Retire { .. } => "retire",
                };
                started_tx.send(label).expect("record worker start");
                processor.execute(task, None).await;
                finished_tx.send(label).expect("record worker finish");
            })
        });
        assert_eq!(
            handle.try_enroll_stream(fixture.stream_id.clone()),
            IngressShadowDisposition::Enqueued,
            "pre-enrolling the handle removes the synthetic enroll race from the lock waiter"
        );
        let started_enroll = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("shadow enrollment should start promptly")
            .expect("enrollment start recorded");
        assert_eq!(started_enroll, "enroll");
        let finished_enroll = tokio::time::timeout(Duration::from_millis(250), finished_rx.recv())
            .await
            .expect("shadow enrollment should drain promptly")
            .expect("enrollment finish recorded");
        assert_eq!(finished_enroll, "enroll");

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

        let enqueue_started = std::time::Instant::now();
        let disposition = handle.try_submit(fixture.submission(1, Some("lock-timeout-origin")));
        let enqueue_elapsed = enqueue_started.elapsed();
        assert_eq!(disposition, IngressShadowDisposition::Enqueued);
        assert!(
            enqueue_elapsed < Duration::from_millis(50),
            "enqueue should remain prompt while the worker blocks on PostgreSQL row locks, took {enqueue_elapsed:?}"
        );

        let started_submit = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("shadow worker should start promptly")
            .expect("worker start recorded");
        assert_eq!(started_submit, "submit");
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
        let finished_submit = tokio::time::timeout(Duration::from_secs(1), finished_rx.recv())
            .await
            .expect("lock_timeout should end the blocked shadow attempt")
            .expect("worker finish recorded");
        assert_eq!(finished_submit, "submit");
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
        let enrollment_finished = Arc::new(Notify::new());
        let started_order = Arc::new(AtomicUsize::new(0));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let handle = test_handle(2, 2, {
            let release_first = release_first.clone();
            let enrollment_finished = enrollment_finished.clone();
            let started_order = started_order.clone();
            move |task| {
                let release_first = release_first.clone();
                let enrollment_finished = enrollment_finished.clone();
                let started_order = started_order.clone();
                let started_tx = started_tx.clone();
                Box::pin(async move {
                    match task {
                        IngressShadowTask::Enroll { .. } => {
                            enrollment_finished.notify_waiters();
                        }
                        IngressShadowTask::Submit(_) => {
                            let order = started_order.fetch_add(1, Ordering::SeqCst);
                            started_tx.send(order).expect("record start order");
                            if order == 0 {
                                release_first.notified().await;
                            }
                        }
                        IngressShadowTask::Retire { .. } => {}
                    }
                })
            }
        });
        let stream_a = SmSessionId::new("stream-a");
        let drained_enrollment = enrollment_finished.notified();
        let new_submission = |stream_id: SmSessionId| {
            let message = Message::new(Some(jid::Jid::from(
                "room@conference.example.com"
                    .parse::<BareJid>()
                    .expect("room jid"),
            )));
            let mut submission = base_submission(message);
            submission.stream_id = stream_id;
            submission
        };

        assert_eq!(
            handle.try_enroll_stream(stream_a.clone()),
            IngressShadowDisposition::Enqueued
        );
        tokio::time::timeout(Duration::from_millis(250), drained_enrollment)
            .await
            .expect("explicit enrollment should drain before submit FIFO assertions");
        assert_eq!(
            handle.try_submit(new_submission(stream_a.clone())),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_submit(new_submission(stream_a.clone())),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_submit(new_submission(stream_a)),
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

    #[test]
    fn closed_submit_admission_rolls_back_stream_activity() {
        let stream_id = SmSessionId::new("stream-a");
        let tx = Arc::new(std::sync::Mutex::new(None));
        let enqueued_streams = Arc::new(std::sync::Mutex::new(HashSet::from([stream_id.clone()])));
        let retiring_streams = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let stream_activity = Arc::new(std::sync::Mutex::new(StreamActivityState::default()));
        let retirement_retry_dispatcher = RetirementRetryDispatcher {
            state: Arc::new(std::sync::Mutex::new(RetirementRetryState::default())),
            notify: Arc::new(Notify::new()),
            capacity: 1,
        };
        let submission_capacity = Arc::new(Semaphore::new(1));
        let enrollment_capacity = Arc::new(Semaphore::new(1));

        let disposition = try_send_worker_task(
            WorkerTaskContext {
                tx: &tx,
                enqueued_streams: &enqueued_streams,
                retiring_streams: &retiring_streams,
                retirement_retry_dispatcher: &retirement_retry_dispatcher,
                stream_activity: &stream_activity,
                submission_capacity: &submission_capacity,
                enrollment_capacity: &enrollment_capacity,
                retirement_capacity: 1,
            },
            IngressShadowTask::Submit(Box::new(base_submission(Message::new(Some(Jid::from(
                "room@conference.example.com"
                    .parse::<BareJid>()
                    .expect("room jid"),
            )))))),
        );

        assert_eq!(disposition, IngressShadowDisposition::Closed);
        assert!(
            stream_is_idle(&stream_activity, &stream_id),
            "a failed publish must restore the stream to idle"
        );
        assert!(
            wait_for_stream_idle_notifier(&stream_activity, &stream_id).is_none(),
            "a failed publish must not strand an idle waiter behind leaked activity"
        );
    }

    #[tokio::test]
    async fn stream_idle_wait_includes_queued_shadow_submission() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let handle = IngressShadowHandle::spawn_test_worker(2, 1, {
            let started = started.clone();
            let release = release.clone();
            move |kind, _stream_id| {
                let started = started.clone();
                let release = release.clone();
                async move {
                    if kind == IngressShadowTestTaskKind::Submit {
                        started.notify_waiters();
                        release.notified().await;
                    }
                }
            }
        });
        let stream_id = SmSessionId::new("stream-a");
        let message = Message::new(Some(jid::Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));

        let started_wait = started.notified();
        assert_eq!(
            handle.try_submit(base_submission(message)),
            IngressShadowDisposition::Enqueued
        );
        tokio::time::timeout(Duration::from_millis(250), started_wait)
            .await
            .expect("submission should start");
        assert!(
            !handle
                .wait_for_stream_idle(&stream_id, Duration::from_millis(25))
                .await,
            "claim transfer must not treat active shadow work as drained"
        );

        release.notify_waiters();
        assert!(
            handle
                .wait_for_stream_idle(&stream_id, Duration::from_millis(250))
                .await,
            "the stream becomes transferable only after its shadow work finishes"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn immediate_submission_completion_does_not_leave_pending_activity() {
        let handle = IngressShadowHandle::spawn_test_worker(8, 8, |_kind, _stream_id| async {});
        let stream_id = SmSessionId::new("stream-a");

        for _ in 0..128 {
            let message = Message::new(Some(jid::Jid::from(
                "room@conference.example.com"
                    .parse::<BareJid>()
                    .expect("room jid"),
            )));
            assert_eq!(
                handle.try_submit(base_submission(message)),
                IngressShadowDisposition::Enqueued
            );
            tokio::task::yield_now().await;
            assert!(
                handle
                    .wait_for_stream_idle(&stream_id, Duration::from_millis(250))
                    .await,
                "fast submissions must not leave leaked pending activity behind"
            );
        }
    }

    #[tokio::test]
    async fn retirement_admission_is_bounded_by_unique_streams() {
        let release_retirements = Arc::new(Notify::new());
        let handle = test_handle(2, 1, {
            let release_retirements = release_retirements.clone();
            move |task| {
                let release_retirements = release_retirements.clone();
                Box::pin(async move {
                    if matches!(task, IngressShadowTask::Retire { .. }) {
                        release_retirements.notified().await;
                    }
                })
            }
        });

        assert_eq!(
            handle.try_send(IngressShadowTask::Retire {
                stream_id: SmSessionId::new("retire-a"),
            }),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_send(IngressShadowTask::Retire {
                stream_id: SmSessionId::new("retire-b"),
            }),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_send(IngressShadowTask::Retire {
                stream_id: SmSessionId::new("retire-c"),
            }),
            IngressShadowDisposition::QueueFull,
            "terminal stream cleanup must obey the same bounded admission surface as other shadow work"
        );

        release_retirements.notify_waiters();
    }

    #[test]
    fn retirement_retry_inventory_is_bounded_and_requests_durable_rescan() {
        let dispatcher = RetirementRetryDispatcher {
            state: Arc::new(std::sync::Mutex::new(RetirementRetryState::default())),
            notify: Arc::new(Notify::new()),
            capacity: 1,
        };

        schedule_retirement_task_retry(&dispatcher, SmSessionId::new("retire-a"));
        schedule_retirement_task_retry(&dispatcher, SmSessionId::new("retire-b"));
        schedule_retirement_task_retry(&dispatcher, SmSessionId::new("retire-c"));

        let state = dispatcher.state.lock().expect("retry dispatcher mutex");
        assert_eq!(state.queued.len(), 1);
        assert_eq!(state.queued_members.len(), 1);
        assert!(
            state.scan_requested,
            "overflow is recovered from durable rows"
        );
    }

    #[test]
    fn retirement_retry_scan_keeps_rescan_after_full_or_duplicate_pages() {
        let mut empty_state = RetirementRetryState::default();
        queue_scanned_retirements(
            &mut empty_state,
            vec![
                SmSessionId::new("retire-a"),
                SmSessionId::new("retire-b"),
                SmSessionId::new("retire-c"),
            ],
            2,
        );
        assert_eq!(
            empty_state.queued.iter().cloned().collect::<Vec<_>>(),
            vec![SmSessionId::new("retire-a"), SmSessionId::new("retire-b")]
        );
        assert!(
            empty_state.scan_requested,
            "a full SQL page must keep the durable rescan armed for the next page"
        );

        let mut duplicate_state = RetirementRetryState::default();
        duplicate_state
            .queued_members
            .insert(SmSessionId::new("retire-a"));
        duplicate_state
            .queued
            .push_back(SmSessionId::new("retire-a"));
        duplicate_state
            .queued_members
            .insert(SmSessionId::new("retire-b"));
        duplicate_state
            .queued
            .push_back(SmSessionId::new("retire-b"));
        queue_scanned_retirements(
            &mut duplicate_state,
            vec![
                SmSessionId::new("retire-a"),
                SmSessionId::new("retire-b"),
                SmSessionId::new("retire-c"),
            ],
            2,
        );
        assert_eq!(
            duplicate_state.queued.iter().cloned().collect::<Vec<_>>(),
            vec![SmSessionId::new("retire-a"), SmSessionId::new("retire-b")]
        );
        assert!(
            duplicate_state.scan_requested,
            "already-queued rows still require another scan when the SQL page was full"
        );
    }

    #[tokio::test]
    async fn queue_full_retirement_is_retried_after_capacity_frees() {
        let tx = Arc::new(std::sync::Mutex::new(None));
        let enqueued_streams = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let retiring_streams = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let release_first = Arc::new(Notify::new());
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let handle = IngressShadowHandle::spawn_worker_with_enqueued_streams(
            WorkerLimits {
                queue_capacity: 1,
                max_concurrency: 1,
            },
            tx,
            enqueued_streams,
            retiring_streams.clone(),
            None,
            Arc::new(std::sync::Mutex::new(StreamActivityState::default())),
            Arc::new({
                let release_first = release_first.clone();
                move |task, _outstanding| {
                    let release_first = release_first.clone();
                    let retiring_streams = retiring_streams.clone();
                    let started_tx = started_tx.clone();
                    Box::pin(async move {
                        if let IngressShadowTask::Retire { stream_id } = task {
                            started_tx
                                .send(stream_id.clone())
                                .expect("record retirement start");
                            if stream_id.as_str() == "retire-a" {
                                release_first.notified().await;
                            }
                            forget_retiring_stream(&retiring_streams, &stream_id);
                        }
                    })
                }
            }),
        );

        assert_eq!(
            handle.try_send(IngressShadowTask::Retire {
                stream_id: SmSessionId::new("retire-a"),
            }),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
                .await
                .expect("first retirement should start")
                .expect("first retirement start recorded"),
            SmSessionId::new("retire-a")
        );

        assert_eq!(
            handle.try_send(IngressShadowTask::Retire {
                stream_id: SmSessionId::new("retire-b"),
            }),
            IngressShadowDisposition::QueueFull
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), started_rx.recv())
                .await
                .is_err(),
            "the retry loop must wait for retirement capacity instead of starting immediately"
        );

        release_first.notify_waiters();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
                .await
                .expect("queue-full retirement should be retried after capacity frees")
                .expect("retried retirement start recorded"),
            SmSessionId::new("retire-b")
        );
    }

    #[tokio::test]
    async fn failed_retirement_is_requeued_for_a_later_attempt() {
        let Some(fixture) = ShadowFixture::open("retirement_requeue").await else {
            return;
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tx = Arc::new(std::sync::Mutex::new(Some(tx)));
        let processor = IngressShadowProcessor {
            tx,
            forced_retirement_retryable_failures: Arc::new(AtomicUsize::new(5)),
            ..fixture.processor.clone()
        };
        processor
            .retiring_streams
            .lock()
            .expect("retiring stream mutex")
            .insert(fixture.stream_id.clone());

        processor
            .execute(
                IngressShadowTask::Retire {
                    stream_id: fixture.stream_id.clone(),
                },
                None,
            )
            .await;

        assert!(matches!(
            rx.recv().await,
            Some(QueuedIngressShadowTask {
                task: IngressShadowTask::Retire { stream_id },
                ..
            }) if stream_id == fixture.stream_id
        ));
        fixture.close().await;
    }

    #[tokio::test]
    async fn saturated_submission_queue_still_accepts_sm_enrollment() {
        let hold_submission = Arc::new(Notify::new());
        let (enrolled_tx, mut enrolled_rx) = mpsc::unbounded_channel();
        let handle = test_handle(1, 1, {
            let hold_submission = hold_submission.clone();
            let enrolled_tx = enrolled_tx.clone();
            move |task| {
                let hold_submission = hold_submission.clone();
                let enrolled_tx = enrolled_tx.clone();
                Box::pin(async move {
                    match task {
                        IngressShadowTask::Submit(_) => hold_submission.notified().await,
                        IngressShadowTask::Enroll { .. } => {
                            let _ = enrolled_tx.send(());
                        }
                        IngressShadowTask::Retire { .. } => {}
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
        // The submission auto-enqueued its own stream's enrollment; wait for
        // that enroll task to execute (releasing its admission permit) so this
        // test isolates SUBMIT capacity exhaustion, which is its actual
        // subject — the submission itself stays parked on `hold_submission`.
        tokio::time::timeout(Duration::from_millis(250), enrolled_rx.recv())
            .await
            .expect("auto-enqueued enrollment should execute")
            .expect("enrollment execution recorded");
        assert_eq!(
            handle.try_enroll_stream(SmSessionId::new("fresh-sm-stream")),
            IngressShadowDisposition::Enqueued,
            "fresh SM enables must enqueue even when submit work has exhausted capacity"
        );

        hold_submission.notify_waiters();
    }

    #[tokio::test]
    async fn failed_enrollment_allows_a_later_submit_to_enqueue_enrollment_again() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tx = Arc::new(std::sync::Mutex::new(Some(tx)));
        let enqueued_streams = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let enrollment_capacity = Arc::new(Semaphore::new(1));
        let stream_id = SmSessionId::new("retry-enrollment-stream");
        enqueued_streams
            .lock()
            .expect("enqueued stream mutex")
            .insert(stream_id.clone());

        clear_failed_enrollment(&enqueued_streams, &stream_id);

        assert_eq!(
            ensure_stream_enrollment_task(&tx, &enqueued_streams, &enrollment_capacity, &stream_id),
            IngressShadowDisposition::Enqueued
        );
        assert!(matches!(
            rx.recv().await,
            Some(QueuedIngressShadowTask {
                task: IngressShadowTask::Enroll { stream_id: queued },
                ..
            }) if queued == stream_id
        ));
    }

    #[tokio::test]
    async fn first_submit_for_unseen_stream_prepends_enrollment() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let handle = test_handle(1, 1, {
            move |task| {
                let started_tx = started_tx.clone();
                Box::pin(async move {
                    match task {
                        IngressShadowTask::Submit(_) => {
                            started_tx
                                .send("submit".to_string())
                                .expect("record submit start");
                        }
                        IngressShadowTask::Enroll { stream_id } => {
                            started_tx
                                .send(stream_id.to_string())
                                .expect("record enroll start");
                        }
                        IngressShadowTask::Retire { .. } => {}
                    }
                })
            }
        });
        let message = Message::new(Some(jid::Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        let mut submission = base_submission(message);
        submission.stream_id = SmSessionId::new("fresh-stream");
        assert_eq!(
            handle.try_submit(submission),
            IngressShadowDisposition::Enqueued
        );

        let started_enroll = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("synthetic enrollment should start first")
            .expect("enrollment start recorded");
        assert_eq!(started_enroll, "fresh-stream");
        let started_submit = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("submit should start after enrollment")
            .expect("submission start recorded");
        assert_eq!(started_submit, "submit");
    }

    #[tokio::test]
    async fn saturated_enable_keeps_enrollment_ahead_of_the_first_submission() {
        let release_blocker = Arc::new(Notify::new());
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let handle = test_handle(1, 1, {
            let release_blocker = release_blocker.clone();
            move |task| {
                let release_blocker = release_blocker.clone();
                let started_tx = started_tx.clone();
                Box::pin(async move {
                    match task {
                        IngressShadowTask::Submit(submission)
                            if submission.stream_id.as_str() == "blocking-stream" =>
                        {
                            started_tx
                                .send("blocking-submit".to_string())
                                .expect("record blocker start");
                            release_blocker.notified().await;
                        }
                        IngressShadowTask::Enroll { stream_id } => {
                            started_tx
                                .send(format!("enroll:{stream_id}"))
                                .expect("record enrollment start");
                        }
                        IngressShadowTask::Submit(submission) => {
                            started_tx
                                .send(format!("submit:{}", submission.stream_id))
                                .expect("record submission start");
                        }
                        IngressShadowTask::Retire { .. } => {}
                    }
                })
            }
        });
        let message = Message::new(Some(jid::Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        let mut blocking = base_submission(message.clone());
        blocking.stream_id = SmSessionId::new("blocking-stream");
        let mut first_for_fresh = base_submission(message);
        first_for_fresh.stream_id = SmSessionId::new("fresh-stream");

        assert_eq!(
            handle.try_submit(blocking),
            IngressShadowDisposition::Enqueued
        );
        let started_blocking_enroll =
            tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
                .await
                .expect("blocking stream enrollment should start first")
                .expect("blocking enrollment start recorded");
        assert_eq!(started_blocking_enroll, "enroll:blocking-stream");
        let started_blocker = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("blocking submission should start")
            .expect("blocker start recorded");
        assert_eq!(started_blocker, "blocking-submit");
        assert_eq!(
            handle.try_enroll_stream(SmSessionId::new("fresh-stream")),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_submit(first_for_fresh),
            IngressShadowDisposition::QueueFull,
            "the first fresh submission still obeys submit capacity after its enrollment is queued"
        );

        release_blocker.notify_waiters();
        let started_enroll = tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
            .await
            .expect("fresh enrollment should run after the blocker")
            .expect("fresh enrollment start recorded");
        assert_eq!(started_enroll, "enroll:fresh-stream");
    }

    #[tokio::test]
    async fn enrollment_admission_is_bounded_without_blocking_submissions() {
        let release_enrollment = Arc::new(Notify::new());
        let enrollment_released = Arc::new(AtomicBool::new(false));
        let handle = test_handle(2, 1, {
            let release_enrollment = release_enrollment.clone();
            let enrollment_released = enrollment_released.clone();
            move |task| {
                let release_enrollment = release_enrollment.clone();
                let enrollment_released = enrollment_released.clone();
                Box::pin(async move {
                    if matches!(task, IngressShadowTask::Enroll { .. }) {
                        while !enrollment_released.load(Ordering::Acquire) {
                            release_enrollment.notified().await;
                        }
                    }
                })
            }
        });
        let stream_a = SmSessionId::new("bounded-enrollment-a");
        let stream_b = SmSessionId::new("bounded-enrollment-b");
        let stream_c = SmSessionId::new("bounded-enrollment-c");

        assert_eq!(
            handle.try_enroll_stream(stream_a.clone()),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_enroll_stream(stream_b.clone()),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            handle.try_enroll_stream(stream_c),
            IngressShadowDisposition::QueueFull,
            "unique SM enrollments must stop at their dedicated queue capacity"
        );

        let message = Message::new(Some(jid::Jid::from(
            "room@conference.example.com"
                .parse::<BareJid>()
                .expect("room jid"),
        )));
        let mut submission = base_submission(message);
        submission.stream_id = stream_a;
        assert_eq!(
            handle.try_submit(submission),
            IngressShadowDisposition::Enqueued,
            "an already-enrolled stream must retain independent submission admission"
        );

        enrollment_released.store(true, Ordering::Release);
        release_enrollment.notify_waiters();
    }

    #[tokio::test]
    async fn forgetting_a_terminal_stream_allows_fresh_enrollment() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let handle = test_handle(2, 1, move |task| {
            let started_tx = started_tx.clone();
            Box::pin(async move {
                if let IngressShadowTask::Enroll { stream_id } = task {
                    started_tx.send(stream_id).expect("record enrollment");
                }
            })
        });
        let stream_id = SmSessionId::new("terminal-stream");

        assert_eq!(
            handle.try_enroll_stream(stream_id.clone()),
            IngressShadowDisposition::Enqueued
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
                .await
                .expect("initial enrollment should run"),
            Some(stream_id.clone())
        );

        handle.forget_stream(&stream_id);

        assert_eq!(
            handle.try_enroll_stream(stream_id.clone()),
            IngressShadowDisposition::Enqueued,
            "a terminated stream must not retain the enrollment gate"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), started_rx.recv())
                .await
                .expect("fresh enrollment should run"),
            Some(stream_id)
        );
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
