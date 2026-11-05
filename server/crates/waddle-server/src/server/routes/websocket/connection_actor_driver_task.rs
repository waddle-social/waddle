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
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::{AbortHandle, JoinHandle};
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
    #[cfg(test)]
    Paused(&'static str),
    #[cfg(test)]
    Immediate,
    #[cfg(test)]
    ImmediateResult(&'static str),
}

/// Why an operation does not have a returned typed handler result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectionActorDriverTaskUnknownReason {
    WorkerAborted,
    WorkerStopped,
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

/// A per-connection actor-owned handler task. Dropping its caller-facing
/// handle requests stop, while the task and its join supervisor remain detached
/// and retain every accepted operation record.
pub(super) struct ConnectionActorDriverTask {
    shared: Arc<DriverShared>,
    data: mpsc::Sender<DriverCommand>,
    control: mpsc::Sender<DriverControl>,
    abort: AbortHandle,
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
    joined: Mutex<bool>,
    joined_changed: Notify,
}

struct DriverLedger {
    accepting: bool,
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
    #[cfg(test)]
    Paused {
        entered: Arc<Notify>,
        release: Arc<Notify>,
        result: &'static str,
    },
    #[cfg(test)]
    Immediate {
        input_bytes: usize,
    },
    #[cfg(test)]
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
            #[cfg(test)]
            Self::Paused { .. } => 0,
            #[cfg(test)]
            Self::Immediate { input_bytes } => *input_bytes,
            #[cfg(test)]
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
                next_id: 1,
                operations: HashMap::new(),
            }),
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
        tokio::spawn(supervise_driver(worker, Arc::clone(&shared)));
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
            abort,
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

    #[cfg(test)]
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
        self.shared.request_stop(reason);
    }

    /// Force-abort the owned worker and wait until the independent supervisor
    /// has joined it and classified queued/active records.
    pub(super) async fn force_abort_and_join(&self) {
        self.shared.request_stop(StopReason::ShutdownDeadline);
        self.abort.abort();
        self.shared.wait_joined().await;
    }

    pub(super) async fn wait_joined(&self) {
        self.shared.wait_joined().await;
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
        if connection_ref != &self.shared.connection_ref {
            return None;
        }
        let ledger = self.shared.lock_state();
        let operation = ledger.operations.get(&id)?.clone();
        (operation.connection_ref == *connection_ref).then(|| ConnectionActorDriverTicket {
            operation,
            shared: Arc::clone(&self.shared),
        })
    }

    #[cfg(test)]
    fn submit_immediate(
        &self,
        input_bytes: usize,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        self.submit(ConnectionActorDriverTaskInput::Immediate { input_bytes })
    }

    #[cfg(test)]
    fn submit_with_custom_result(
        &self,
        result: &'static str,
    ) -> Result<ConnectionActorDriverTicket, ConnectionActorDriverTaskAdmissionError> {
        self.submit(ConnectionActorDriverTaskInput::ImmediateResult(result))
    }
}

impl Drop for ConnectionActorDriverTask {
    fn drop(&mut self) {
        self.shared.request_stop(StopReason::Requested);
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

    #[cfg(test)]
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

    #[cfg(test)]
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
        self.lifecycle.request_stop(reason);
        let mut ledger = self.lock_state();
        ledger.accepting = false;
        self.stop.cancel();
        drop(ledger);
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
        command
            .operation
            .set_snapshot(ConnectionActorDriverTaskSnapshot::Running);
        command
            .operation
            .runs
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
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
            #[cfg(test)]
            DriverInput::Paused {
                entered,
                release,
                result,
            } => {
                entered.notify_one();
                release.notified().await;
                ConnectionActorDriverTaskResultValue::Paused(result)
            }
            #[cfg(test)]
            DriverInput::Immediate { .. } => ConnectionActorDriverTaskResultValue::Immediate,
            #[cfg(test)]
            DriverInput::ImmediateResult(result) => {
                ConnectionActorDriverTaskResultValue::ImmediateResult(result)
            }
        };
        active.complete(result);
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
        if exceeds_reservation {
            self.shared.request_stop(StopReason::OutboundOverload);
            self.operation
                .set_snapshot(ConnectionActorDriverTaskSnapshot::CompletionUnknown {
                    reason: ConnectionActorDriverTaskUnknownReason::ResultReservationExceeded,
                    result: Some(result),
                });
        } else {
            self.operation
                .set_snapshot(ConnectionActorDriverTaskSnapshot::Completed(result));
        }
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
            ConnectionActorDriverTaskSnapshot::Completed(_)
        ));
        task.wait_joined().await;
        active.acknowledge().expect("completion explicitly settled");
    }
}
