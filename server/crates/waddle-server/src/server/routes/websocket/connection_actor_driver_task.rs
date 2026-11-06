//! Dormant supervised task boundary for the production-compiled connection owner.
//!
//! The task owns `ConnectionActorCore` and its `WsConnState`. Callers submit
//! typed work and observe operation IDs; canceling a waiter does not cancel the
//! handler or release its retained input. This module is not selected by `/ws`.

use super::connection_actor_core::{
    ConnectionActorCore, ConnectionActorDriverCompletion, ConnectionActorDriverError,
};
use super::frame::ResponseBatch;
use super::replay::DriveOutcome;
use super::{OutboundStanza, WebSocketState};
use crate::clustering::NodeAdmissionPermit;
use crate::server::routes::websocket::supervised_io::StopReason;
use jid::BareJid;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::ConnectionRef;

const MAX_PENDING_OPERATIONS: usize = 256;
const DEFAULT_CONTROL_CAPACITY: usize = 8;
const DEFAULT_BYTE_CAPACITY: usize = 32 * 1024 * 1024;
const DEFAULT_RESULT_RESERVATION_BYTES: usize = 64 * 1024;

/// Bounded admission for one dark actor task. The byte value is admission
/// accounting: input bytes plus a result reservation. Handler outputs are
/// measured only after effects return; an overrun is retained and seals
/// admission, so this is not a hard memory bound or an activation guarantee.
#[derive(Debug, Clone, Copy)]
pub(super) struct ConnectionActorDriverTaskLimits {
    data_capacity: usize,
    control_capacity: usize,
    byte_capacity: usize,
    result_reservation_bytes: usize,
}

impl Default for ConnectionActorDriverTaskLimits {
    fn default() -> Self {
        Self {
            data_capacity: MAX_PENDING_OPERATIONS,
            control_capacity: DEFAULT_CONTROL_CAPACITY,
            byte_capacity: DEFAULT_BYTE_CAPACITY,
            result_reservation_bytes: DEFAULT_RESULT_RESERVATION_BYTES,
        }
    }
}

impl ConnectionActorDriverTaskLimits {
    #[cfg(test)]
    fn with_bounds(
        data_capacity: usize,
        control_capacity: usize,
        byte_capacity: usize,
        result_reservation_bytes: usize,
    ) -> Self {
        Self {
            data_capacity,
            control_capacity,
            byte_capacity,
            result_reservation_bytes,
        }
    }

    fn validate(self) -> Result<Self, ConnectionActorDriverTaskAdmissionError> {
        if self.data_capacity == 0
            || self.data_capacity > MAX_PENDING_OPERATIONS
            || self.control_capacity == 0
            || self.byte_capacity == 0
            || self.byte_capacity > u32::MAX as usize
            || self.result_reservation_bytes > self.byte_capacity
        {
            return Err(ConnectionActorDriverTaskAdmissionError::InvalidLimits);
        }
        Ok(self)
    }
}

/// Supported typed operation results retained until the caller acknowledges.
#[derive(Debug)]
pub(super) struct ConnectionActorDriverTaskResult {
    pub(super) value: ConnectionActorDriverTaskResultValue,
    _reservation: Arc<DriverAdmissionReservation>,
}

#[derive(Debug)]
pub(super) enum ConnectionActorDriverTaskResultValue {
    InboundFrame(
        Result<ConnectionActorDriverCompletion<ResponseBatch>, ConnectionActorDriverError>,
    ),
    PeerStanza(Result<ConnectionActorDriverCompletion<DriveOutcome>, ConnectionActorDriverError>),
    #[cfg(any(test, feature = "test-support"))]
    Paused(&'static str),
    #[cfg(any(test, feature = "test-support"))]
    Immediate,
    #[cfg(any(test, feature = "test-support"))]
    ImmediateResult(&'static str),
}

/// Why an operation does not have a returned typed handler result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectionActorDriverTaskUnknownReason {
    WorkerAborted,
    WorkerStopped,
    AuthorityRevoked,
    ResultReservationExceeded,
}

/// Current state of one accepted operation. Accepted inputs stay in the
/// shared record until explicit acknowledgement, including after worker abort.
#[derive(Debug, Clone)]
pub(super) enum ConnectionActorDriverTaskSnapshot {
    Accepted,
    Running,
    Completed(Arc<ConnectionActorDriverTaskResult>),
    CompletionUnknown {
        reason: ConnectionActorDriverTaskUnknownReason,
        result: Option<Arc<ConnectionActorDriverTaskResult>>,
    },
    NotStarted,
}

/// A per-connection actor-owned handler task. Dropping an unattached
/// caller-facing handle requests stop; after endpoint attachment, the app-owned
/// cleanup record controls stop and join while retaining accepted operations.
pub(super) struct ConnectionActorDriverTask {
    shared: Arc<DriverShared>,
    data: mpsc::Sender<DriverCommand>,
    control: mpsc::Sender<DriverControl>,
    cleanup: ConnectionActorDriverTaskCleanup,
    drop_stops: bool,
}

/// App-owned handle for observing and terminating one driver worker. The
/// endpoint cleanup record retains this beside the physical I/O custodian;
/// it has no pointer back to that record or its index.
#[derive(Clone)]
pub(super) struct ConnectionActorDriverTaskCleanup {
    inner: Arc<DriverCleanupInner>,
}

struct DriverCleanupInner {
    shared: Arc<DriverShared>,
    abort: AbortHandle,
    shutdown_deadline_armed: AtomicBool,
    _data: mpsc::Sender<DriverCommand>,
    _control: mpsc::Sender<DriverControl>,
    supervisor: AsyncMutex<SupervisorJoinState>,
}

struct SupervisorJoinState {
    handle: Option<JoinHandle<()>>,
    joined: bool,
}

struct DriverShared {
    connection_ref: ConnectionRef,
    permit: NodeAdmissionPermit,
    shutdown: CancellationToken,
    lifecycle: Arc<super::connection_actor_core::ConnectionActorLifecycle>,
    stop: CancellationToken,
    worker_done: CancellationToken,
    count_permits: Arc<Semaphore>,
    byte_permits: Arc<Semaphore>,
    limits: ConnectionActorDriverTaskLimits,
    state: Mutex<DriverLedger>,
    settlement_callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    joined: Mutex<bool>,
    joined_changed: Notify,
}

struct DriverLedger {
    accepting: bool,
    stop_reason: Option<StopReason>,
    stop_started_at: Option<Instant>,
    next_id: u64,
    operations: HashMap<u64, Arc<DriverOperation>>,
}

struct DriverOperation {
    id: u64,
    connection_ref: ConnectionRef,
    input: ConnectionActorDriverTaskInput,
    _reservation: Arc<DriverAdmissionReservation>,
    snapshot: Mutex<ConnectionActorDriverTaskSnapshot>,
    changed: Notify,
    runs: std::sync::atomic::AtomicUsize,
}

#[derive(Debug)]
struct DriverAdmissionReservation {
    _count: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
    input_bytes: usize,
    result_reservation_bytes: usize,
}

#[derive(Debug, Clone)]
pub(super) enum ConnectionActorDriverTaskInput {
    InboundFrame {
        parsed: waddle_xmpp::protocol::frame::ParsedFrame,
        domain: BareJid,
        wire_bytes: usize,
    },
    PeerStanza(Box<OutboundStanza>),
    #[cfg(any(test, feature = "test-support"))]
    Paused {
        entered: Arc<Notify>,
        release: Arc<Notify>,
        result: &'static str,
    },
    #[cfg(any(test, feature = "test-support"))]
    StartPaused {
        dequeued: Arc<Notify>,
        release: Arc<Notify>,
        effect_runs: Arc<std::sync::atomic::AtomicUsize>,
    },
    #[cfg(any(test, feature = "test-support"))]
    Panic,
    #[cfg(any(test, feature = "test-support"))]
    Immediate {
        input_bytes: usize,
    },
    #[cfg(any(test, feature = "test-support"))]
    ImmediateResult(&'static str),
}

type DriverInput = ConnectionActorDriverTaskInput;

struct DriverCommand {
    operation: Arc<DriverOperation>,
}

enum DriverControl {
    Barrier(oneshot::Sender<()>),
}

impl ConnectionActorDriverTaskInput {
    fn input_bytes(&self) -> usize {
        match self {
            Self::InboundFrame {
                parsed,
                domain,
                wire_bytes,
            } => (*wire_bytes)
                .max(accounted_debug_bytes(parsed))
                .saturating_add(domain.as_str().len()),
            // Count a typed representation only. XML serialization belongs
            // to the eventual socket adapter, not actor admission.
            Self::PeerStanza(stanza) => accounted_debug_bytes(stanza),
            #[cfg(any(test, feature = "test-support"))]
            Self::Paused { .. } => 0,
            #[cfg(any(test, feature = "test-support"))]
            Self::StartPaused { .. } => 0,
            #[cfg(any(test, feature = "test-support"))]
            Self::Panic => 0,
            #[cfg(any(test, feature = "test-support"))]
            Self::Immediate { input_bytes } => *input_bytes,
            #[cfg(any(test, feature = "test-support"))]
            Self::ImmediateResult(result) => result.len(),
        }
    }
}

impl ConnectionActorDriverTaskResult {
    fn accounted_bytes(&self) -> usize {
        accounted_debug_bytes(&self.value)
    }
}

struct CountingFormatter(usize);

impl std::fmt::Write for CountingFormatter {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.0 = self.0.saturating_add(value.len());
        Ok(())
    }
}

fn accounted_debug_bytes(value: &impl std::fmt::Debug) -> usize {
    let mut formatter = CountingFormatter(0);
    let _ = std::write!(&mut formatter, "{value:?}");
    formatter.0
}

/// A stable ID and observation handle for one submitted operation.
#[derive(Clone)]
pub(super) struct ConnectionActorDriverTicket {
    operation: Arc<DriverOperation>,
    shared: Arc<DriverShared>,
}

impl ConnectionActorDriverTask {
    /// Spawn a production-compiled owner task. The worker owns `core` for its
    /// full lifetime; the separate supervisor retains and joins its handle.
    pub(super) fn spawn(
        state: super::state::WsConnState,
        websocket: Arc<WebSocketState>,
        permit: NodeAdmissionPermit,
        shutdown: CancellationToken,
    ) -> Result<Self, ConnectionActorDriverTaskAdmissionError> {
        Self::spawn_with_limits(
            state,
            websocket,
            permit,
            shutdown,
            ConnectionActorDriverTaskLimits::default(),
        )
    }

    fn spawn_with_limits(
        state: super::state::WsConnState,
        websocket: Arc<WebSocketState>,
        permit: NodeAdmissionPermit,
        shutdown: CancellationToken,
        limits: ConnectionActorDriverTaskLimits,
    ) -> Result<Self, ConnectionActorDriverTaskAdmissionError> {
        let limits = limits.validate()?;
        let core = ConnectionActorCore::new(state);
        let connection_ref = core.connection_ref().clone();
        let lifecycle = Arc::clone(&core.lifecycle);
        let stop = CancellationToken::new();
        let worker_done = CancellationToken::new();
        let shared = Arc::new(DriverShared {
            connection_ref,
            permit: permit.clone(),
            shutdown: shutdown.clone(),
            lifecycle,
            stop: stop.clone(),
            worker_done: worker_done.clone(),
            count_permits: Arc::new(Semaphore::new(limits.data_capacity)),
            byte_permits: Arc::new(Semaphore::new(limits.byte_capacity)),
            limits,
            state: Mutex::new(DriverLedger {
                accepting: true,
                stop_reason: None,
                stop_started_at: None,
                next_id: 1,
                operations: HashMap::new(),
            }),
            settlement_callback: Mutex::new(None),
            joined: Mutex::new(false),
            joined_changed: Notify::new(),
        });
        let (data, receiver) = mpsc::channel(limits.data_capacity);
        let (control, control_receiver) = mpsc::channel(limits.control_capacity);
        let worker_shared = Arc::clone(&shared);
        let worker_permit = permit.clone();
        let worker_shutdown = shutdown.clone();
        let worker = tokio::spawn(run_driver(DriverWorker {
            core,
            websocket,
            permit: worker_permit,
            shutdown: worker_shutdown,
            stop,
            shared: worker_shared,
            receiver,
            control_receiver,
        }));
        let abort = worker.abort_handle();
        let supervisor = tokio::spawn(supervise_driver(worker, Arc::clone(&shared)));
        let cleanup = ConnectionActorDriverTaskCleanup {
            inner: Arc::new(DriverCleanupInner {
                shared: Arc::clone(&shared),
                abort,
                shutdown_deadline_armed: AtomicBool::new(false),
                _data: data.clone(),
                _control: control.clone(),
                supervisor: AsyncMutex::new(SupervisorJoinState {
                    handle: Some(supervisor),
                    joined: false,
                }),
            }),
        };
        tokio::spawn(watch_driver_authority(
            Arc::clone(&shared),
            permit,
            shutdown,
            worker_done,
        ));
        Ok(Self {
            shared,
            data,
            control,
            cleanup,
            drop_stops: true,
        })
    }

    pub(super) fn connection_ref(&self) -> &ConnectionRef {
        &self.shared.connection_ref
    }

    /// Admit one frame operation without an await boundary. The operation ID
    /// and source input become shared custody before the worker can run it.
    pub(super) fn submit_inbound_frame(
        &self,
        parsed: waddle_xmpp::protocol::frame::ParsedFrame,
        domain: BareJid,
        wire_bytes: usize,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        if wire_bytes > waddle_xmpp::protocol::frame::MAX_FRAME_SIZE {
            return Err(ConnectionActorDriverTaskAdmissionError::Overloaded);
        }
        self.submit(ConnectionActorDriverTaskInput::InboundFrame {
            parsed,
            domain,
            wire_bytes,
        })
    }

    /// Admit one recipient-pass operation while preserving the original typed
    /// envelope in the ledger.
    pub(super) fn submit_peer_stanza(
        &self,
        stanza: OutboundStanza,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        if stanza.kind != super::DeliveryKind::PeerStanza {
            return Err(ConnectionActorDriverTaskAdmissionError::WrongOperationKind);
        }
        self.submit(ConnectionActorDriverTaskInput::PeerStanza(Box::new(stanza)))
    }

    #[cfg(any(test, feature = "test-support"))]
    fn submit_paused(
        &self,
        entered: Arc<Notify>,
        release: Arc<Notify>,
        result: &'static str,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        self.submit(ConnectionActorDriverTaskInput::Paused {
            entered,
            release,
            result,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    fn submit_start_paused(
        &self,
        dequeued: Arc<Notify>,
        release: Arc<Notify>,
        effect_runs: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        self.submit(ConnectionActorDriverTaskInput::StartPaused {
            dequeued,
            release,
            effect_runs,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    fn submit_panic(
        &self,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        self.submit(ConnectionActorDriverTaskInput::Panic)
    }

    fn submit(
        &self,
        input: ConnectionActorDriverTaskInput,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        let input_bytes = input.input_bytes();
        let charged_bytes = input_bytes
            .checked_add(self.shared.limits.result_reservation_bytes)
            .filter(|bytes| *bytes <= self.shared.limits.byte_capacity)
            .ok_or(ConnectionActorDriverTaskAdmissionError::Overloaded)?;
        let count = self
            .shared
            .count_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ConnectionActorDriverTaskAdmissionError::Overloaded)?;
        let charged_bytes = u32::try_from(charged_bytes)
            .map_err(|_| ConnectionActorDriverTaskAdmissionError::Overloaded)?;
        let bytes = self
            .shared
            .byte_permits
            .clone()
            .try_acquire_many_owned(charged_bytes)
            .map_err(|_| ConnectionActorDriverTaskAdmissionError::Overloaded)?;
        let reservation = Arc::new(DriverAdmissionReservation {
            _count: count,
            _bytes: bytes,
            input_bytes,
            result_reservation_bytes: self.shared.limits.result_reservation_bytes,
        });
        let result = self.shared.permit.with_current(|| {
            let mut ledger = self.shared.lock_state();
            if !ledger.accepting
                || self.shared.stop.is_cancelled()
                || self.shared.shutdown.is_cancelled()
            {
                return Err(ConnectionActorDriverTaskAdmissionError::Stopped);
            }
            let id = ledger.next_id;
            ledger.next_id = ledger
                .next_id
                .checked_add(1)
                .ok_or(ConnectionActorDriverTaskAdmissionError::OperationIdExhausted)?;
            let operation = Arc::new(DriverOperation {
                id,
                connection_ref: self.shared.connection_ref.clone(),
                input,
                _reservation: reservation,
                snapshot: Mutex::new(ConnectionActorDriverTaskSnapshot::Accepted),
                changed: Notify::new(),
                runs: std::sync::atomic::AtomicUsize::new(0),
            });
            ledger.operations.insert(id, Arc::clone(&operation));
            match self.data.try_send(DriverCommand {
                operation: Arc::clone(&operation),
            }) {
                Ok(()) => Ok(ConnectionActorDriverTicket {
                    operation,
                    shared: Arc::clone(&self.shared),
                }),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    ledger.operations.remove(&id);
                    Err(ConnectionActorDriverTaskAdmissionError::Overloaded)
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    ledger.operations.remove(&id);
                    Err(ConnectionActorDriverTaskAdmissionError::Stopped)
                }
            }
        });
        match result {
            Ok(result) => result,
            Err(_) => {
                self.shared.request_stop(StopReason::LeaseLost);
                Err(ConnectionActorDriverTaskAdmissionError::Stopped)
            }
        }
    }

    /// Stop admission immediately and let a started effectful handler return
    /// into retained completion custody before the worker exits.
    pub(super) fn request_stop(&self, reason: StopReason) {
        self.cleanup.request_stop(reason);
    }

    /// Force-abort the owned worker and wait until the independent supervisor
    /// has joined it and classified queued/active records.
    pub(super) async fn force_abort_and_join(&self) {
        self.cleanup.force_abort_and_join().await;
    }

    pub(super) async fn wait_joined(&self) {
        self.cleanup.wait_joined().await;
    }

    /// Resolve after the actor task services a control marker. This queue is
    /// independently bounded from data and is serviced between handlers only.
    pub(super) async fn control_barrier(
        &self,
    ) -> Result<(), ConnectionActorDriverTaskControlError> {
        self.try_control_barrier()?
            .await
            .map_err(|_| ConnectionActorDriverTaskControlError::Stopped)
    }

    fn try_control_barrier(
        &self,
    ) -> Result<oneshot::Receiver<()>, ConnectionActorDriverTaskControlError> {
        let (complete, wait) = oneshot::channel();
        self.control
            .try_send(DriverControl::Barrier(complete))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    ConnectionActorDriverTaskControlError::Overloaded
                }
                mpsc::error::TrySendError::Closed(_) => {
                    ConnectionActorDriverTaskControlError::Stopped
                }
            })?;
        Ok(wait)
    }

    /// Reacquire an unresolved operation by its exact connection identity and
    /// monotonic ID after the original caller or ticket has disappeared.
    pub(super) fn lookup(
        &self,
        connection_ref: &ConnectionRef,
        id: u64,
    ) -> Option<ConnectionActorDriverTicket> {
        self.cleanup.lookup(connection_ref, id)
    }

    pub(super) fn cleanup_handle(&self) -> ConnectionActorDriverTaskCleanup {
        self.cleanup.clone()
    }

    pub(super) fn mark_app_owned(&mut self) {
        self.drop_stops = false;
    }

    #[cfg(any(test, feature = "test-support"))]
    fn submit_immediate(
        &self,
        input_bytes: usize,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        self.submit(ConnectionActorDriverTaskInput::Immediate { input_bytes })
    }

    #[cfg(any(test, feature = "test-support"))]
    fn submit_with_custom_result(
        &self,
        result: &'static str,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        self.submit(ConnectionActorDriverTaskInput::ImmediateResult(result))
    }
}

impl Drop for ConnectionActorDriverTask {
    fn drop(&mut self) {
        if self.drop_stops {
            self.cleanup.request_stop(StopReason::Requested);
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(super) enum ConnectionActorDriverTaskAdmissionError {
    #[error("connection driver task is stopping")]
    Stopped,
    #[error("connection driver operation capacity is full")]
    Overloaded,
    #[error("peer recipient pass accepts only peer stanzas")]
    WrongOperationKind,
    #[error("connection driver operation IDs are exhausted")]
    OperationIdExhausted,
    #[error("connection driver task limits are invalid")]
    InvalidLimits,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(super) enum ConnectionActorDriverTaskControlError {
    #[error("connection driver control queue is full")]
    Overloaded,
    #[error("connection driver task is stopped")]
    Stopped,
}

#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionActorDriverTestUnknownReason {
    WorkerAborted,
    WorkerStopped,
    AuthorityRevoked,
    ResultReservationExceeded,
}

#[cfg(feature = "test-support")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionActorDriverTestSnapshot {
    Accepted,
    Running,
    Completed,
    CompletionUnknown(ConnectionActorDriverTestUnknownReason),
    NotStarted,
}

#[cfg(feature = "test-support")]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConnectionActorDriverTestAdmissionError {
    #[error("connection driver is stopping")]
    Stopped,
    #[error("connection driver capacity is full")]
    Overloaded,
}

#[cfg(feature = "test-support")]
#[derive(Default)]
pub struct ConnectionActorDriverTestGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(feature = "test-support")]
impl ConnectionActorDriverTestGate {
    pub async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    pub fn release(&self) {
        self.release.notify_one();
    }
}

#[cfg(feature = "test-support")]
#[derive(Clone)]
pub struct ConnectionActorDriverTestTicket {
    ticket: ConnectionActorDriverTicket,
}

#[cfg(feature = "test-support")]
impl ConnectionActorDriverTestTicket {
    pub(super) fn from_ticket(ticket: ConnectionActorDriverTicket) -> Self {
        Self { ticket }
    }

    pub fn id(&self) -> u64 {
        self.ticket.id()
    }

    pub fn connection_ref(&self) -> &ConnectionRef {
        self.ticket.connection_ref()
    }

    pub fn snapshot(&self) -> ConnectionActorDriverTestSnapshot {
        map_test_snapshot(self.ticket.snapshot())
    }

    pub async fn wait(&self) -> ConnectionActorDriverTestSnapshot {
        map_test_snapshot(self.ticket.wait().await)
    }

    pub fn run_count(&self) -> usize {
        self.ticket
            .operation
            .runs
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn retains_input(&self) -> bool {
        self.ticket.operation._reservation.input_bytes > 0
            || !matches!(
                &self.ticket.operation.input,
                DriverInput::Immediate { input_bytes: 0 }
            )
    }

    pub fn acknowledge(self) -> bool {
        self.ticket.acknowledge().is_ok()
    }
}

#[cfg(feature = "test-support")]
fn map_test_snapshot(
    snapshot: ConnectionActorDriverTaskSnapshot,
) -> ConnectionActorDriverTestSnapshot {
    match snapshot {
        ConnectionActorDriverTaskSnapshot::Accepted => ConnectionActorDriverTestSnapshot::Accepted,
        ConnectionActorDriverTaskSnapshot::Running => ConnectionActorDriverTestSnapshot::Running,
        ConnectionActorDriverTaskSnapshot::Completed(_) => {
            ConnectionActorDriverTestSnapshot::Completed
        }
        ConnectionActorDriverTaskSnapshot::CompletionUnknown { reason, .. } => {
            let reason = match reason {
                ConnectionActorDriverTaskUnknownReason::WorkerAborted => {
                    ConnectionActorDriverTestUnknownReason::WorkerAborted
                }
                ConnectionActorDriverTaskUnknownReason::WorkerStopped => {
                    ConnectionActorDriverTestUnknownReason::WorkerStopped
                }
                ConnectionActorDriverTaskUnknownReason::AuthorityRevoked => {
                    ConnectionActorDriverTestUnknownReason::AuthorityRevoked
                }
                ConnectionActorDriverTaskUnknownReason::ResultReservationExceeded => {
                    ConnectionActorDriverTestUnknownReason::ResultReservationExceeded
                }
            };
            ConnectionActorDriverTestSnapshot::CompletionUnknown(reason)
        }
        ConnectionActorDriverTaskSnapshot::NotStarted => {
            ConnectionActorDriverTestSnapshot::NotStarted
        }
    }
}

/// Test-only producer handle for the production compiled driver task. An
/// attached task remains app-owned when this caller handle is dropped.
#[cfg(feature = "test-support")]
pub struct ConnectionActorDriverTestHandle {
    task: ConnectionActorDriverTask,
}

#[cfg(feature = "test-support")]
impl ConnectionActorDriverTestHandle {
    pub(super) fn from_attached(task: ConnectionActorDriverTask) -> Self {
        Self { task }
    }

    pub fn connection_ref(&self) -> &ConnectionRef {
        self.task.connection_ref()
    }

    pub fn submit_paused(
        &self,
        gate: &ConnectionActorDriverTestGate,
    ) -> Result<ConnectionActorDriverTestTicket, ConnectionActorDriverTestAdmissionError> {
        self.task
            .submit_paused(
                Arc::clone(&gate.entered),
                Arc::clone(&gate.release),
                "paused test operation",
            )
            .map(ConnectionActorDriverTestTicket::from_ticket)
            .map_err(map_test_admission_error)
    }

    pub fn submit_immediate(
        &self,
    ) -> Result<ConnectionActorDriverTestTicket, ConnectionActorDriverTestAdmissionError> {
        self.task
            .submit_immediate(0)
            .map(ConnectionActorDriverTestTicket::from_ticket)
            .map_err(map_test_admission_error)
    }

    pub fn lookup(
        &self,
        connection_ref: &ConnectionRef,
        id: u64,
    ) -> Option<ConnectionActorDriverTestTicket> {
        self.task
            .lookup(connection_ref, id)
            .map(ConnectionActorDriverTestTicket::from_ticket)
    }

    pub fn request_stop(&self, reason: StopReason) {
        self.task.request_stop(reason);
    }

    pub async fn wait_stopped(&self) -> Instant {
        self.task.shared.stop.cancelled().await;
        self.task
            .shared
            .lock_state()
            .stop_started_at
            .unwrap_or_else(Instant::now)
    }

    pub async fn wait_joined(&self) {
        self.task.wait_joined().await;
    }
}

#[cfg(feature = "test-support")]
fn map_test_admission_error(
    error: ConnectionActorDriverTaskAdmissionError,
) -> ConnectionActorDriverTestAdmissionError {
    match error {
        ConnectionActorDriverTaskAdmissionError::Overloaded => {
            ConnectionActorDriverTestAdmissionError::Overloaded
        }
        ConnectionActorDriverTaskAdmissionError::Stopped
        | ConnectionActorDriverTaskAdmissionError::WrongOperationKind
        | ConnectionActorDriverTaskAdmissionError::OperationIdExhausted
        | ConnectionActorDriverTaskAdmissionError::InvalidLimits => {
            ConnectionActorDriverTestAdmissionError::Stopped
        }
    }
}

impl ConnectionActorDriverTicket {
    pub(super) fn id(&self) -> u64 {
        self.operation.id
    }

    pub(super) fn connection_ref(&self) -> &ConnectionRef {
        &self.operation.connection_ref
    }

    pub(super) fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        self.operation.connection_ref == *connection_ref
            && self.shared.connection_ref == *connection_ref
    }

    #[cfg(any(test, feature = "test-support"))]
    fn retains_input(&self) -> bool {
        self.operation._reservation.input_bytes > 0
            || !matches!(
                &self.operation.input,
                DriverInput::Immediate { input_bytes: 0 }
            )
    }

    pub(super) fn input_snapshot(&self) -> ConnectionActorDriverTaskInput {
        self.operation.input.clone()
    }

    #[cfg(any(test, feature = "test-support"))]
    fn run_count(&self) -> usize {
        self.operation
            .runs
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(super) fn snapshot(&self) -> ConnectionActorDriverTaskSnapshot {
        self.operation.lock_snapshot().clone()
    }

    /// Wait cancellation only drops this waiter. The operation record and its
    /// original input stay in the ledger until this ticket is acknowledged.
    pub(super) async fn wait(&self) -> ConnectionActorDriverTaskSnapshot {
        loop {
            let changed = self.operation.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let snapshot = self.snapshot();
            if !matches!(
                snapshot,
                ConnectionActorDriverTaskSnapshot::Accepted
                    | ConnectionActorDriverTaskSnapshot::Running
            ) {
                return snapshot;
            }
            changed.await;
        }
    }

    /// Remove the operation from ID lookup after the caller has moved returned
    /// work to its next custody owner. Admission permits remain held by any
    /// surviving ticket or result handles.
    pub(super) fn acknowledge(self) -> Result<(), ConnectionActorDriverTaskAckError> {
        if matches!(
            self.snapshot(),
            ConnectionActorDriverTaskSnapshot::Accepted
                | ConnectionActorDriverTaskSnapshot::Running
        ) {
            return Err(ConnectionActorDriverTaskAckError::NotTerminal);
        }
        let mut ledger = self.shared.lock_state();
        let Some(current) = ledger.operations.get(&self.operation.id) else {
            return Err(ConnectionActorDriverTaskAckError::AlreadyAcknowledged);
        };
        if !Arc::ptr_eq(current, &self.operation)
            || current.connection_ref != self.shared.connection_ref
        {
            return Err(ConnectionActorDriverTaskAckError::WrongOwner);
        }
        ledger.operations.remove(&self.operation.id);
        drop(ledger);
        self.shared.operation_acknowledged();
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(super) enum ConnectionActorDriverTaskAckError {
    #[error("operation has not reached a terminal state")]
    NotTerminal,
    #[error("operation was already acknowledged")]
    AlreadyAcknowledged,
    #[error("operation belongs to another connection owner")]
    WrongOwner,
}

impl DriverShared {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, DriverLedger> {
        self.state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }

    fn request_stop(&self, reason: StopReason) {
        let mut ledger = self.lock_state();
        if ledger.stop_reason.is_none() {
            ledger.stop_reason = Some(reason);
            ledger.stop_started_at = Some(Instant::now());
        }
        let effective_reason = ledger.stop_reason.unwrap_or(reason);
        ledger.accepting = false;
        // Admission, operation start, and result publication use this same
        // ledger lock. Mark the core stopped before releasing the gate so a
        // worker cannot pass its core preflight after stop has won the ledger.
        self.lifecycle.request_stop(effective_reason);
        self.stop.cancel();
    }

    fn begin_operation(&self, operation: &Arc<DriverOperation>) -> bool {
        let ledger = self.lock_state();
        if !ledger.accepting
            || self.stop.is_cancelled()
            || self.shutdown.is_cancelled()
            || operation.connection_ref != self.connection_ref
            || !ledger
                .operations
                .get(&operation.id)
                .is_some_and(|current| Arc::ptr_eq(current, operation))
        {
            return false;
        }
        let mut snapshot = operation.lock_snapshot();
        if !matches!(*snapshot, ConnectionActorDriverTaskSnapshot::Accepted) {
            return false;
        }
        *snapshot = ConnectionActorDriverTaskSnapshot::Running;
        operation
            .runs
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        operation.changed.notify_waiters();
        true
    }

    fn publish_completion(
        &self,
        operation: &DriverOperation,
        result: Arc<ConnectionActorDriverTaskResult>,
        exceeds_reservation: bool,
        authority_revoked: bool,
    ) {
        let mut ledger = self.lock_state();
        let unknown_reason = if exceeds_reservation {
            if ledger.stop_reason.is_none() {
                ledger.stop_reason = Some(StopReason::OutboundOverload);
                ledger.stop_started_at = Some(Instant::now());
            }
            ledger.accepting = false;
            self.stop.cancel();
            Some(ConnectionActorDriverTaskUnknownReason::ResultReservationExceeded)
        } else if authority_revoked {
            Some(ConnectionActorDriverTaskUnknownReason::AuthorityRevoked)
        } else if !ledger.accepting {
            Some(match ledger.stop_reason {
                Some(StopReason::LeaseLost) => {
                    ConnectionActorDriverTaskUnknownReason::AuthorityRevoked
                }
                _ => ConnectionActorDriverTaskUnknownReason::WorkerStopped,
            })
        } else {
            None
        };
        match unknown_reason {
            Some(reason) => {
                operation.set_snapshot(ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                    reason,
                    result: Some(result),
                })
            }
            None => operation.set_snapshot(ConnectionActorDriverTaskSnapshot::Completed(result)),
        }
        drop(ledger);
        if exceeds_reservation {
            self.request_stop(StopReason::OutboundOverload);
        }
    }

    fn operation_acknowledged(&self) {
        let callback = self
            .settlement_callback
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    fn set_settlement_callback(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        *self
            .settlement_callback
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = Some(callback);
    }

    async fn wait_joined(&self) {
        loop {
            let notified = self.joined_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if *self
                .joined
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            {
                return;
            }
            notified.await;
        }
    }

    fn mark_worker_joined(&self, aborted: bool) {
        let mut ledger = self.lock_state();
        ledger.accepting = false;
        for operation in ledger.operations.values() {
            let mut snapshot = operation.lock_snapshot();
            match *snapshot {
                ConnectionActorDriverTaskSnapshot::Accepted => {
                    *snapshot = ConnectionActorDriverTaskSnapshot::NotStarted;
                    operation.changed.notify_waiters();
                }
                ConnectionActorDriverTaskSnapshot::Running if aborted => {
                    *snapshot = ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                        reason: ConnectionActorDriverTaskUnknownReason::WorkerAborted,
                        result: None,
                    };
                    operation.changed.notify_waiters();
                }
                ConnectionActorDriverTaskSnapshot::Running => {
                    *snapshot = ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                        reason: ConnectionActorDriverTaskUnknownReason::WorkerStopped,
                        result: None,
                    };
                    operation.changed.notify_waiters();
                }
                _ => {}
            }
        }
        drop(ledger);
        *self
            .joined
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = true;
        self.joined_changed.notify_waiters();
    }
}

impl ConnectionActorDriverTaskCleanup {
    pub(super) fn connection_ref(&self) -> &ConnectionRef {
        &self.inner.shared.connection_ref
    }

    pub(super) fn request_stop(&self, reason: StopReason) {
        self.inner.shared.request_stop(reason);
    }

    pub(super) fn set_settlement_callback(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        self.inner.shared.set_settlement_callback(callback);
    }

    pub(super) fn lookup(
        &self,
        connection_ref: &ConnectionRef,
        id: u64,
    ) -> Option<ConnectionActorDriverTicket> {
        if connection_ref != &self.inner.shared.connection_ref {
            return None;
        }
        let ledger = self.inner.shared.lock_state();
        let operation = ledger.operations.get(&id)?.clone();
        (operation.connection_ref == *connection_ref).then(|| ConnectionActorDriverTicket {
            operation,
            shared: Arc::clone(&self.inner.shared),
        })
    }

    pub(super) fn is_settled(&self) -> bool {
        let ledger = self.inner.shared.lock_state();
        let joined = *self
            .inner
            .shared
            .joined
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        joined && ledger.operations.is_empty()
    }

    pub(super) fn is_joined(&self) -> bool {
        *self
            .inner
            .shared
            .joined
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }

    /// Arm the one grace timer for this driver. Its deadline is based on the
    /// synchronous first-stop timestamp in the ledger, so executor scheduling
    /// delay cannot extend the configured grace. The worker supervisor remains
    /// the sole owner of the JoinHandle and reports its join independently.
    pub(super) fn arm_shutdown_deadline(&self, grace: Duration) {
        if self
            .inner
            .shutdown_deadline_armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let shared = Arc::clone(&self.inner.shared);
        let abort = self.inner.abort.clone();
        tokio::spawn(async move {
            shared.stop.cancelled().await;
            let started_at = shared
                .lock_state()
                .stop_started_at
                .unwrap_or_else(Instant::now);
            let deadline = started_at.checked_add(grace).unwrap_or_else(Instant::now);
            tokio::select! {
                biased;
                _ = shared.wait_joined() => {}
                _ = tokio::time::sleep_until(deadline) => abort.abort(),
            }
        });
    }

    /// Stop admission, let the active handler return for `grace`, then abort
    /// and join the worker if it remains stalled. Accepted queued inputs are
    /// classified as `NotStarted`; the active input remains
    /// `CompletionUnknown` after a forced abort.
    pub(super) async fn shutdown_and_join(&self, grace: Duration) {
        self.request_stop(StopReason::Requested);
        self.arm_shutdown_deadline(grace);
        self.wait_joined().await;
    }

    async fn force_abort_and_join(&self) {
        self.request_stop(StopReason::ShutdownDeadline);
        self.inner.abort.abort();
        self.join_supervisor().await;
    }

    async fn wait_joined(&self) {
        self.inner.shared.wait_joined().await;
        self.join_supervisor().await;
    }

    async fn join_supervisor(&self) {
        let mut state = self.inner.supervisor.lock().await;
        if state.joined {
            return;
        }
        if let Some(supervisor) = state.handle.as_mut() {
            let _ = supervisor.await;
        }
        state.handle = None;
        state.joined = true;
    }
}

impl DriverOperation {
    fn lock_snapshot(&self) -> std::sync::MutexGuard<'_, ConnectionActorDriverTaskSnapshot> {
        self.snapshot
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }
}

struct DriverWorker {
    core: ConnectionActorCore,
    websocket: Arc<WebSocketState>,
    permit: NodeAdmissionPermit,
    shutdown: CancellationToken,
    stop: CancellationToken,
    shared: Arc<DriverShared>,
    receiver: mpsc::Receiver<DriverCommand>,
    control_receiver: mpsc::Receiver<DriverControl>,
}

async fn run_driver(worker: DriverWorker) {
    let DriverWorker {
        mut core,
        websocket,
        permit,
        shutdown,
        stop,
        shared,
        mut receiver,
        mut control_receiver,
    } = worker;
    loop {
        let event = tokio::select! {
            biased;
            _ = stop.cancelled() => return,
            _ = shutdown.cancelled() => {
                shared.request_stop(StopReason::ShutdownDeadline);
                return;
            }
            _ = permit.revoked() => {
                shared.request_stop(StopReason::LeaseLost);
                return;
            }
            control = control_receiver.recv() => DriverEvent::Control(control),
            data = receiver.recv() => DriverEvent::Data(data),
        };
        let command = match event {
            DriverEvent::Control(Some(DriverControl::Barrier(complete))) => {
                let _ = complete.send(());
                continue;
            }
            DriverEvent::Control(None) => continue,
            DriverEvent::Data(Some(command)) => command,
            DriverEvent::Data(None) => return,
        };
        if command.operation.connection_ref != shared.connection_ref {
            command
                .operation
                .set_snapshot(ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                    reason: ConnectionActorDriverTaskUnknownReason::WorkerAborted,
                    result: None,
                });
            continue;
        }
        #[cfg(any(test, feature = "test-support"))]
        if let DriverInput::StartPaused {
            dequeued, release, ..
        } = &command.operation.input
        {
            dequeued.notify_one();
            release.notified().await;
        }
        match permit.with_current(|| shared.begin_operation(&command.operation)) {
            Ok(true) => {}
            Ok(false) => {
                if shutdown.is_cancelled() {
                    shared.request_stop(StopReason::ShutdownDeadline);
                }
                return;
            }
            Err(_) => {
                shared.request_stop(StopReason::LeaseLost);
                return;
            }
        }
        let mut active =
            ActiveOperationGuard::new(Arc::clone(&command.operation), Arc::clone(&shared));
        let result = match &command.operation.input {
            DriverInput::InboundFrame {
                parsed,
                domain,
                wire_bytes,
            } => ConnectionActorDriverTaskResultValue::InboundFrame(
                core.process_inbound_frame(
                    parsed.clone(),
                    domain,
                    *wire_bytes,
                    websocket.as_ref(),
                    &permit,
                    &shutdown,
                )
                .await,
            ),
            DriverInput::PeerStanza(stanza) => ConnectionActorDriverTaskResultValue::PeerStanza(
                core.interpret_peer_stanza(websocket.as_ref(), stanza, &permit, &shutdown)
                    .await,
            ),
            #[cfg(any(test, feature = "test-support"))]
            DriverInput::Paused {
                entered,
                release,
                result,
            } => {
                entered.notify_one();
                release.notified().await;
                ConnectionActorDriverTaskResultValue::Paused(result)
            }
            #[cfg(any(test, feature = "test-support"))]
            DriverInput::StartPaused { effect_runs, .. } => {
                effect_runs.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                ConnectionActorDriverTaskResultValue::Immediate
            }
            #[cfg(any(test, feature = "test-support"))]
            DriverInput::Panic => panic!("synthetic driver panic"),
            #[cfg(any(test, feature = "test-support"))]
            DriverInput::Immediate { .. } => ConnectionActorDriverTaskResultValue::Immediate,
            #[cfg(any(test, feature = "test-support"))]
            DriverInput::ImmediateResult(result) => {
                ConnectionActorDriverTaskResultValue::ImmediateResult(result)
            }
        };
        let mut result = Some(result);
        if permit
            .with_current(|| active.complete(result.take().expect("operation result is present")))
            .is_err()
        {
            active.complete_non_current(result.take().expect("revoked operation result remains"));
        }
    }
}

enum DriverEvent {
    Control(Option<DriverControl>),
    Data(Option<DriverCommand>),
}

struct ActiveOperationGuard {
    operation: Arc<DriverOperation>,
    shared: Arc<DriverShared>,
    complete: bool,
}

impl ActiveOperationGuard {
    fn new(operation: Arc<DriverOperation>, shared: Arc<DriverShared>) -> Self {
        Self {
            operation,
            shared,
            complete: false,
        }
    }

    fn complete(&mut self, value: ConnectionActorDriverTaskResultValue) {
        let result = Arc::new(ConnectionActorDriverTaskResult {
            value,
            _reservation: Arc::clone(&self.operation._reservation),
        });
        let exceeds_reservation =
            result.accounted_bytes() > self.operation._reservation.result_reservation_bytes;
        self.shared
            .publish_completion(&self.operation, result, exceeds_reservation, false);
        self.complete = true;
    }

    fn complete_non_current(&mut self, value: ConnectionActorDriverTaskResultValue) {
        let result = Arc::new(ConnectionActorDriverTaskResult {
            value,
            _reservation: Arc::clone(&self.operation._reservation),
        });
        let exceeds_reservation =
            result.accounted_bytes() > self.operation._reservation.result_reservation_bytes;
        self.shared.request_stop(StopReason::LeaseLost);
        self.shared
            .publish_completion(&self.operation, result, exceeds_reservation, true);
        self.complete = true;
    }
}

impl Drop for ActiveOperationGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.operation
                .set_snapshot(ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                    reason: ConnectionActorDriverTaskUnknownReason::WorkerAborted,
                    result: None,
                });
        }
    }
}

async fn supervise_driver(worker: JoinHandle<()>, shared: Arc<DriverShared>) {
    let aborted = worker.await.is_err();
    if aborted {
        shared.lifecycle.request_stop(StopReason::ShutdownDeadline);
        shared.stop.cancel();
    }
    shared.worker_done.cancel();
    shared.mark_worker_joined(aborted);
}

async fn watch_driver_authority(
    shared: Arc<DriverShared>,
    permit: NodeAdmissionPermit,
    shutdown: CancellationToken,
    worker_done: CancellationToken,
) {
    tokio::select! {
        _ = worker_done.cancelled() => {}
        _ = shutdown.cancelled() => shared.request_stop(StopReason::ShutdownDeadline),
        _ = permit.revoked() => shared.request_stop(StopReason::LeaseLost),
    }
}

impl DriverOperation {
    fn set_snapshot(&self, next: ConnectionActorDriverTaskSnapshot) {
        *self.lock_snapshot() = next;
        self.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::NodeLifecycle;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use std::time::Duration;

    async fn test_task() -> ConnectionActorDriverTask {
        test_task_with_limits(ConnectionActorDriverTaskLimits::default()).await
    }

    async fn test_task_with_limits(
        limits: ConnectionActorDriverTaskLimits,
    ) -> ConnectionActorDriverTask {
        let websocket = create_test_websocket_state().await;
        let permit = NodeLifecycle::new().admit().expect("serving permit");
        ConnectionActorDriverTask::spawn_with_limits(
            super::super::state::WsConnState::new(),
            websocket,
            permit,
            CancellationToken::new(),
            limits,
        )
        .expect("valid test limits")
    }

    #[tokio::test]
    async fn cancelled_waiter_recovers_retained_result_by_exact_operation_id() {
        let task = test_task().await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let ticket = task
            .submit_paused(Arc::clone(&entered), Arc::clone(&release), "retained")
            .expect("operation accepted");
        let id = ticket.id();
        let connection_ref = task.connection_ref().clone();

        let waiter_ticket = ticket.clone();
        let waiter = tokio::spawn(async move { waiter_ticket.wait().await });
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("worker started handler");
        waiter.abort();
        let _ = waiter.await;
        drop(ticket);
        release.notify_one();

        let recovered = task
            .lookup(&connection_ref, id)
            .expect("accepted ID remains recoverable");
        assert!(recovered.is_for(&connection_ref));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), recovered.wait())
                .await
                .expect("retained completion"),
            ConnectionActorDriverTaskSnapshot::Completed(result)
                if matches!(result.value, ConnectionActorDriverTaskResultValue::Paused("retained"))
        ));
        assert_eq!(
            recovered.run_count(),
            1,
            "caller retry did not reexecute ID"
        );
        recovered.acknowledge().expect("caller settled result");
        task.request_stop(StopReason::Requested);
        task.wait_joined().await;
    }

    #[tokio::test]
    async fn data_saturation_keeps_control_admission_and_service_independent() {
        let task = test_task_with_limits(ConnectionActorDriverTaskLimits::with_bounds(
            MAX_PENDING_OPERATIONS,
            1,
            1024 * 1024,
            128,
        ))
        .await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut tickets = vec![task
            .submit_paused(Arc::clone(&entered), Arc::clone(&release), "active")
            .expect("first data operation")];
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("worker is held in the first handler");
        for _ in 1..MAX_PENDING_OPERATIONS {
            tickets.push(task.submit_immediate(0).expect("data slot available"));
        }
        assert!(matches!(
            task.submit_immediate(0),
            Err(ConnectionActorDriverTaskAdmissionError::Overloaded)
        ));

        let control = task
            .try_control_barrier()
            .expect("control has separate bounded admission");
        assert!(matches!(
            task.try_control_barrier(),
            Err(ConnectionActorDriverTaskControlError::Overloaded)
        ));
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), control)
            .await
            .expect("control barrier is serviced after the active handler")
            .expect("worker serviced control");

        task.request_stop(StopReason::Requested);
        task.wait_joined().await;
        for ticket in tickets {
            assert!(!matches!(
                ticket.snapshot(),
                ConnectionActorDriverTaskSnapshot::Accepted
                    | ConnectionActorDriverTaskSnapshot::Running
            ));
            ticket.acknowledge().expect("terminal operation is settled");
        }
    }

    #[tokio::test]
    async fn byte_admission_remains_reserved_until_completion_is_acknowledged() {
        let task =
            test_task_with_limits(ConnectionActorDriverTaskLimits::with_bounds(4, 1, 100, 32))
                .await;
        let first = task.submit_immediate(40).expect("first byte reservation");
        assert!(matches!(
            task.submit_immediate(30),
            Err(ConnectionActorDriverTaskAdmissionError::Overloaded)
        ));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), first.wait())
                .await
                .expect("operation completes"),
            ConnectionActorDriverTaskSnapshot::Completed(_)
        ));
        first.acknowledge().expect("explicit result settlement");
        task.request_stop(StopReason::Requested);
        task.wait_joined().await;
    }

    #[tokio::test]
    async fn result_reservation_overrun_retains_result_and_seals_admission() {
        let task =
            test_task_with_limits(ConnectionActorDriverTaskLimits::with_bounds(4, 1, 64, 4)).await;
        let ticket = task
            .submit_with_custom_result("oversized typed result")
            .expect("small operation input is admitted");
        let snapshot = tokio::time::timeout(Duration::from_secs(1), ticket.wait())
            .await
            .expect("worker retains completed value");
        let ConnectionActorDriverTaskSnapshot::CompletionUnknown {
            reason: ConnectionActorDriverTaskUnknownReason::ResultReservationExceeded,
            result: Some(result),
        } = snapshot
        else {
            panic!("overrun must retain its typed result and be marked unknown");
        };
        assert!(matches!(
            result.value,
            ConnectionActorDriverTaskResultValue::ImmediateResult("oversized typed result")
        ));
        assert!(matches!(
            task.submit_immediate(0),
            Err(ConnectionActorDriverTaskAdmissionError::Stopped)
        ));
        task.wait_joined().await;
        ticket
            .acknowledge()
            .expect("caller settles retained result");
    }

    #[tokio::test]
    async fn forced_worker_abort_retains_running_input_and_marks_queued_not_started() {
        let task = test_task().await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let running = task
            .submit_paused(Arc::clone(&entered), Arc::clone(&release), "never returned")
            .expect("running operation accepted");
        let queued = task
            .submit_immediate(12)
            .expect("queued operation accepted");
        let connection_ref = task.connection_ref().clone();
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("worker entered effectful handler");

        task.force_abort_and_join().await;

        assert!(running.is_for(&connection_ref));
        assert!(running.retains_input());
        assert!(matches!(
            running.input_snapshot(),
            ConnectionActorDriverTaskInput::Paused { .. }
        ));
        assert!(matches!(
            running.snapshot(),
            ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                reason: ConnectionActorDriverTaskUnknownReason::WorkerAborted,
                result: None,
            }
        ));
        assert!(matches!(
            queued.snapshot(),
            ConnectionActorDriverTaskSnapshot::NotStarted
        ));
        assert!(matches!(
            queued.input_snapshot(),
            ConnectionActorDriverTaskInput::Immediate { input_bytes: 12 }
        ));
        running
            .acknowledge()
            .expect("recovered active input settled");
        queued.acknowledge().expect("queued input settled");
    }

    #[tokio::test]
    async fn app_owned_driver_survives_caller_handle_drop_and_recovers_once() {
        let mut task = test_task().await;
        let cleanup = task.cleanup_handle();
        let connection_ref = task.connection_ref().clone();
        let successor = ConnectionRef::fresh(connection_ref.node_lease().clone());
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let ticket = task
            .submit_paused(Arc::clone(&entered), Arc::clone(&release), "owned")
            .expect("operation accepted");
        let id = ticket.id();
        task.mark_app_owned();

        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("worker entered operation");
        drop(ticket);
        drop(task);
        assert!(
            !cleanup.inner.shared.stop.is_cancelled(),
            "dropping an app-owned caller handle does not stop its worker"
        );
        assert!(cleanup.lookup(&successor, id).is_none());
        release.notify_one();

        let recovered = cleanup
            .lookup(&connection_ref, id)
            .expect("exact app record reacquires the operation");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), recovered.wait())
                .await
                .expect("handler completion is retained"),
            ConnectionActorDriverTaskSnapshot::Completed(result)
                if matches!(result.value, ConnectionActorDriverTaskResultValue::Paused("owned"))
        ));
        assert_eq!(recovered.run_count(), 1);
        assert!(
            !cleanup.is_joined(),
            "dropping the producer handle does not close the app-owned worker"
        );
        recovered
            .acknowledge()
            .expect("operation acknowledged once");
        assert!(cleanup.lookup(&connection_ref, id).is_none());
        cleanup.request_stop(StopReason::Requested);
        cleanup.wait_joined().await;
    }

    #[tokio::test]
    async fn bounded_shutdown_aborts_and_joins_then_classifies_active_and_queued() {
        let mut task = test_task().await;
        let cleanup = task.cleanup_handle();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let active = task
            .submit_paused(Arc::clone(&entered), Arc::clone(&release), "stalled")
            .expect("active operation accepted");
        let queued = task
            .submit_immediate(12)
            .expect("queued operation accepted");
        task.mark_app_owned();
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("worker entered stalled handler");
        drop(task);

        tokio::time::timeout(
            Duration::from_secs(1),
            cleanup.shutdown_and_join(Duration::from_millis(10)),
        )
        .await
        .expect("deadline abort and supervisor join are bounded");

        assert!(cleanup.is_joined());
        assert!(matches!(
            active.snapshot(),
            ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                reason: ConnectionActorDriverTaskUnknownReason::WorkerAborted,
                result: None,
            }
        ));
        assert!(matches!(
            queued.snapshot(),
            ConnectionActorDriverTaskSnapshot::NotStarted
        ));
        active.acknowledge().expect("active custody acknowledged");
        queued.acknowledge().expect("queued custody acknowledged");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_deadline_is_measured_from_the_synchronous_first_stop() {
        let task = test_task().await;
        let cleanup = task.cleanup_handle();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let active = task
            .submit_paused(Arc::clone(&entered), Arc::clone(&release), "held")
            .expect("active operation accepted");
        let queued = task.submit_immediate(0).expect("queued operation accepted");
        entered.notified().await;
        tokio::time::pause();
        let grace = Duration::from_secs(10);
        cleanup.arm_shutdown_deadline(grace);

        task.request_stop(StopReason::Requested);
        // Leave the timer task unscheduled past its deadline. It must use the
        // stop Instant recorded synchronously under DriverLedger, not start a
        // fresh grace when the detached timer gets CPU time.
        tokio::time::advance(grace + Duration::from_secs(1)).await;
        tokio::time::timeout(Duration::from_secs(1), cleanup.wait_joined())
            .await
            .expect("the original first-stop deadline has already elapsed");

        assert!(matches!(
            active.snapshot(),
            ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                reason: ConnectionActorDriverTaskUnknownReason::WorkerAborted,
                result: None,
            }
        ));
        assert!(matches!(
            queued.snapshot(),
            ConnectionActorDriverTaskSnapshot::NotStarted
        ));
        active.acknowledge().expect("active custody acknowledged");
        queued.acknowledge().expect("queued custody acknowledged");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_and_submit_share_one_admission_linearization_gate() {
        let task = Arc::new(test_task().await);
        let cleanup = task.cleanup_handle();
        let start = Arc::new(std::sync::Barrier::new(3));
        let submit_task = Arc::clone(&task);
        let submit_start = Arc::clone(&start);
        let submit = tokio::task::spawn_blocking(move || {
            submit_start.wait();
            submit_task.submit_immediate(0)
        });
        let stop_cleanup = cleanup.clone();
        let stop_start = Arc::clone(&start);
        let stop = tokio::task::spawn_blocking(move || {
            stop_start.wait();
            stop_cleanup.request_stop(StopReason::Requested);
        });
        start.wait();
        let submitted = submit.await.expect("submit thread joins");
        stop.await.expect("stop thread joins");

        assert!(
            task.submit_immediate(0).is_err(),
            "later admission is sealed"
        );
        cleanup.wait_joined().await;
        match submitted {
            Ok(ticket) => {
                assert!(matches!(
                    ticket.snapshot(),
                    ConnectionActorDriverTaskSnapshot::Completed(_)
                        | ConnectionActorDriverTaskSnapshot::CompletionUnknown { .. }
                        | ConnectionActorDriverTaskSnapshot::NotStarted
                ));
                ticket
                    .acknowledge()
                    .expect("the winner of the admission gate remains recoverable");
            }
            Err(error) => {
                assert_eq!(error, ConnectionActorDriverTaskAdmissionError::Stopped)
            }
        }
    }

    #[tokio::test]
    async fn stop_before_handler_return_wins_completion_snapshot_publication() {
        let task = test_task().await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let active = task
            .submit_paused(Arc::clone(&entered), Arc::clone(&release), "late result")
            .expect("active operation accepted");
        let queued = task
            .submit_immediate(4)
            .expect("queued operation accepted before stop");
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("handler reached deterministic pause");

        task.request_stop(StopReason::Requested);
        assert!(matches!(
            task.submit_immediate(0),
            Err(ConnectionActorDriverTaskAdmissionError::Stopped)
        ));
        release.notify_one();

        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), active.wait())
                .await
                .expect("late handler result remains observable"),
            ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                reason: ConnectionActorDriverTaskUnknownReason::WorkerStopped,
                result: Some(result),
            } if matches!(result.value, ConnectionActorDriverTaskResultValue::Paused("late result"))
        ));
        task.wait_joined().await;
        assert!(matches!(
            queued.snapshot(),
            ConnectionActorDriverTaskSnapshot::NotStarted
        ));
        active.acknowledge().expect("late result custody settled");
        queued.acknowledge().expect("queued input custody settled");
    }

    #[tokio::test]
    async fn stop_after_dequeue_before_start_claim_keeps_operation_not_started() {
        let task = test_task().await;
        let dequeued = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let effect_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ticket = task
            .submit_start_paused(
                Arc::clone(&dequeued),
                Arc::clone(&release),
                Arc::clone(&effect_runs),
            )
            .expect("operation accepted");

        tokio::time::timeout(Duration::from_secs(1), dequeued.notified())
            .await
            .expect("worker dequeued and paused before the start claim");
        assert!(matches!(
            ticket.snapshot(),
            ConnectionActorDriverTaskSnapshot::Accepted
        ));

        task.request_stop(StopReason::Requested);
        release.notify_one();
        task.wait_joined().await;

        assert!(matches!(
            ticket.snapshot(),
            ConnectionActorDriverTaskSnapshot::NotStarted
        ));
        assert_eq!(
            ticket.run_count(),
            0,
            "the start claim never won the ledger"
        );
        assert_eq!(
            effect_runs.load(std::sync::atomic::Ordering::Acquire),
            0,
            "the handler/effect body was never entered"
        );
        ticket.acknowledge().expect("not-started custody settled");
    }

    #[tokio::test]
    async fn panic_keeps_active_unknown_and_queued_not_started() {
        let task = test_task().await;
        let panicking = task.submit_panic().expect("panic operation accepted");
        let queued = task
            .submit_immediate(0)
            .expect("second operation accepted before worker runs");
        task.wait_joined().await;
        assert!(matches!(
            panicking.snapshot(),
            ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                reason: ConnectionActorDriverTaskUnknownReason::WorkerAborted,
                result: None,
            }
        ));
        assert!(matches!(
            queued.snapshot(),
            ConnectionActorDriverTaskSnapshot::NotStarted
        ));
        panicking.acknowledge().expect("panicked operation settled");
        queued.acknowledge().expect("queued operation settled");
    }

    #[tokio::test]
    async fn lookup_ack_and_stop_remain_available_while_control_waits_behind_handler() {
        let task = test_task().await;
        let completed = task.submit_immediate(0).expect("first operation accepted");
        let completed_id = completed.id();
        let connection_ref = task.connection_ref().clone();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), completed.wait())
                .await
                .expect("first operation completes"),
            ConnectionActorDriverTaskSnapshot::Completed(_)
        ));

        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let active = task
            .submit_paused(Arc::clone(&entered), Arc::clone(&release), "after stop")
            .expect("second operation accepted");
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("second handler is stalled");
        let barrier = task
            .try_control_barrier()
            .expect("control admission is independent");

        let recovered = task
            .lookup(&connection_ref, completed_id)
            .expect("exact lookup does not wait for the worker");
        assert!(matches!(
            recovered.snapshot(),
            ConnectionActorDriverTaskSnapshot::Completed(_)
        ));
        recovered
            .acknowledge()
            .expect("completed result acknowledged");
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), barrier)
            .await
            .expect("control barrier runs after the handler returns")
            .expect("control marker completed");
        assert!(!matches!(
            active.wait().await,
            ConnectionActorDriverTaskSnapshot::Accepted
                | ConnectionActorDriverTaskSnapshot::Running
        ));
        task.request_stop(StopReason::LeaseLost);
        assert!(matches!(
            task.submit_immediate(0),
            Err(ConnectionActorDriverTaskAdmissionError::Stopped)
        ));
        task.wait_joined().await;
        active.acknowledge().expect("active operation settled");
    }

    #[tokio::test]
    async fn concurrent_join_callers_both_observe_the_supervisor_join() {
        let task = test_task().await;
        let cleanup = task.cleanup_handle();
        task.request_stop(StopReason::Requested);
        tokio::join!(
            task.wait_joined(),
            cleanup.shutdown_and_join(Duration::from_secs(1)),
        );
        assert!(cleanup.is_joined());
    }

    #[tokio::test]
    async fn lease_revocation_seals_admission_while_a_handler_is_stalled() {
        let websocket = create_test_websocket_state().await;
        let lifecycle = NodeLifecycle::new();
        let task = ConnectionActorDriverTask::spawn(
            super::super::state::WsConnState::new(),
            websocket,
            lifecycle.admit().expect("serving permit"),
            CancellationToken::new(),
        )
        .expect("default limits are valid");
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let active = task
            .submit_paused(
                Arc::clone(&entered),
                Arc::clone(&release),
                "handler finished",
            )
            .expect("active operation accepted");
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("worker is stalled in operation");

        lifecycle.begin_drain();
        tokio::time::timeout(Duration::from_secs(1), task.shared.stop.cancelled())
            .await
            .expect("independent authority watcher seals task promptly");
        assert!(matches!(
            task.submit_immediate(0),
            Err(ConnectionActorDriverTaskAdmissionError::Stopped)
        ));
        release.notify_one();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), active.wait())
                .await
                .expect("active handler returns under its own custody"),
            ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                reason: ConnectionActorDriverTaskUnknownReason::AuthorityRevoked,
                result: Some(_),
            }
        ));
        task.wait_joined().await;
        active.acknowledge().expect("completion explicitly settled");
    }
}
