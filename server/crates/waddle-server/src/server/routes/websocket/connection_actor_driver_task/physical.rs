//! Test-only physical continuation owner. This is not selected by `/ws`.

use super::super::connection_actor_core::fixture::{
    ContinuationLimits, ContinuationPhase, ContinuationTicket, OutputContinuation,
};
use super::super::connection_actor_core::ConnectionActorCore;
use super::super::frame::{ClassifiedInboundFrame, ClassifiedSmRequest};
use super::super::supervised_io::{IoCustodianHandle, QueueItem, StopReason, WriteProgress};
use super::{
    ActiveOperationGuard, ConnectionActorDriverCompletion, ConnectionActorDriverTaskInput,
    ConnectionActorDriverTaskResultValue, DriverAdmissionReservation, DriverControl,
    DriverOperation, DriverOutputBound, DriverWorker, MAX_SM_ACK_OUTPUT_BYTES,
};
use crate::server::routes::interpret::OrderedRelayInboundSequence;
use crate::websocket_transport::{
    InputError, OutputError, RetainedWebSocketInput, WebSocketInput, WebSocketOutboundFrame,
    WebSocketOutput, WebSocketWriteReceipt,
};
use axum::extract::ws::Message;
use futures::future::BoxFuture;
use std::collections::VecDeque;
use std::fmt;
use std::future::pending;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use waddle_xmpp::ownership::ConnectionRef;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::registry::{
    ActorOutboundCustody, ActorOutboundId, ActorOutboundReceiver, DeliveryKind, OutboundStanza,
};
use waddle_xmpp::stream_management::SmAck;
use waddle_xmpp::Stanza;

#[derive(Debug, Clone, Copy)]
pub struct PhysicalLimits {
    pub max_frame_bytes: usize,
    pub max_replay_frames: usize,
    pub max_replay_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalSnapshot {
    pub phase: Option<ContinuationPhase>,
    pub progress: Option<WriteProgress>,
    pub appended: u32,
    pub attempted: u32,
    pub flushed: u64,
    pub peer_acked: u32,
    pub replay_frames: usize,
    pub replay_bytes: usize,
    pub checkpoint_h: Option<u32>,
    pub control_pending: bool,
    pub direct_pending: bool,
    /// The exact reaper accepted the driver's stop request. This does not say
    /// which reason won the physical stop fence.
    pub stop_forwarded: bool,
    pub enrollment: PhysicalEnrollment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalEnrollment {
    NotStarted,
    Running,
    Enrolled,
    Unknown,
}

/// Explicit post-join transfer of the fixture's process-local custody.
pub struct PhysicalRecovery {
    pub connection: ConnectionRef,
    /// Exact operations and their admission reservations transferred from the
    /// driver ledger, including work that never reached the worker.
    pub operations: Vec<super::ConnectionActorDriverTestTicket>,
    pub active: Option<PhysicalProducerRecovery>,
    pub current: Option<PhysicalWriteRecovery>,
    pub pending_direct: Option<PhysicalDirectRecovery>,
    pub control: Option<PhysicalControlRecovery>,
    pub replay: Vec<PhysicalReplayRecovery>,
    pub checkpoint_h: Option<u32>,
    pub stream: SmSessionId,
    pub enrollment: PhysicalEnrollment,
    ingress: Arc<crate::ingress::IngressAuthority>,
}

impl PhysicalRecovery {
    /// Retire the exact fixture stream after custody has been transferred.
    pub async fn retire_stream(
        &self,
    ) -> Result<crate::ingress::IngressRetirementOutcome, crate::ingress_uow::IngressUowError> {
        self.ingress.forget_stream(&self.stream).await
    }
}

pub struct PhysicalProducerRecovery {
    pub operation_id: u64,
    pub ordinal: u64,
    pub phase: ContinuationPhase,
    pub retained_credits: (usize, usize),
    producer: Arc<PhysicalProducer>,
    ticket: ContinuationTicket,
}

impl PhysicalProducerRecovery {
    /// Original input remains retained even if its effect was aborted.
    pub fn input_stanza(&self) -> &Stanza {
        &self.producer.stanza
    }

    pub fn requested_frames(&self) -> u64 {
        self.producer.frame_count
    }

    /// A caller-requested snapshot is outside the driver's admission budget.
    pub fn stanza(&self) -> Option<Stanza> {
        self.producer
            .lock_model()
            .stanza_snapshot(&self.ticket)
            .ok()
    }
}

pub struct PhysicalWriteRecovery {
    pub kind: PhysicalWriteKind,
    pub receipt: Option<WebSocketWriteReceipt>,
}

pub enum PhysicalWriteKind {
    Producer { operation_id: u64, ordinal: u64 },
    Direct(PhysicalDirectRecovery),
    Acknowledgement(SmAck),
}

pub struct PhysicalDirectRecovery {
    pub id: ActorOutboundId,
    pub outbound: Arc<OutboundStanza>,
    pub custody: Arc<ActorOutboundCustody>,
}

pub enum PhysicalControlRecovery {
    Request { ack: Option<SmAck>, started: bool },
    PeerAck(SmAck),
    Unsupported,
}

pub struct PhysicalReplayRecovery {
    pub sequence: u32,
    pub payload: PhysicalReplayPayload,
    pub receipt: Option<WebSocketWriteReceipt>,
}

pub enum PhysicalReplayPayload {
    Producer {
        stanza: Stanza,
        operation_id: u64,
    },
    Direct {
        id: ActorOutboundId,
        outbound: Arc<OutboundStanza>,
        custody: Arc<ActorOutboundCustody>,
    },
}

pub(in crate::server::routes::websocket) struct PhysicalParts {
    input: WebSocketInput<axum::Error>,
    output: Option<WebSocketOutput<axum::Error>>,
    direct: ActorOutboundReceiver,
    custodian: IoCustodianHandle,
    limits: PhysicalLimits,
    first_handled: Option<OrderedRelayInboundSequence>,
    stream: SmSessionId,
}

impl PhysicalParts {
    pub(in crate::server::routes::websocket) fn new(
        input: WebSocketInput<axum::Error>,
        output: WebSocketOutput<axum::Error>,
        direct: ActorOutboundReceiver,
        custodian: IoCustodianHandle,
        limits: PhysicalLimits,
        first_handled: OrderedRelayInboundSequence,
        stream: SmSessionId,
    ) -> Option<Self> {
        if limits.max_frame_bytes == 0
            || limits.max_replay_frames == 0
            || limits.max_replay_frames >= (u32::MAX as usize / 2)
            || limits.max_replay_bytes < limits.max_frame_bytes
        {
            return None;
        }
        Some(Self {
            input,
            output: Some(output),
            direct,
            custodian,
            limits,
            first_handled: Some(first_handled),
            stream,
        })
    }
}

pub(super) struct PhysicalShared {
    connection: ConnectionRef,
    core: Arc<AsyncMutex<ConnectionActorCore>>,
    direct_custody: Arc<ActorOutboundCustody>,
    stream: SmSessionId,
    ingress: Arc<crate::ingress::IngressAuthority>,
    producer_claimed: AtomicBool,
    limits: PhysicalLimits,
    ledger: Mutex<PhysicalLedger>,
    control_count: Arc<Semaphore>,
    control_bytes: Arc<Semaphore>,
    changed: Notify,
}

#[derive(Default)]
struct PhysicalLedger {
    active: Option<(Arc<DriverOperation>, ContinuationTicket)>,
    current: Option<CurrentWrite>,
    control: Option<PendingControl>,
    pending_direct: Option<ActorOutboundId>,
    replay: VecDeque<ReplayEntry>,
    replay_bytes: usize,
    appended: u32,
    attempted: u32,
    flushed: u64,
    peer_acked: u32,
    checkpoint_h: Option<u32>,
    reconciled: bool,
    stop_forwarded: bool,
    enrollment: Option<PhysicalEnrollment>,
}

struct CurrentWrite {
    kind: CurrentKind,
    receipt: Arc<OnceLock<WebSocketWriteReceipt>>,
}

enum CurrentKind {
    Producer {
        operation: Arc<DriverOperation>,
        ticket: ContinuationTicket,
    },
    Direct(ActorOutboundId),
    Acknowledgement(SmAck),
}

struct ReplayEntry {
    sequence: u32,
    payload: ReplayPayload,
    charged_bytes: usize,
    receipt: Arc<OnceLock<WebSocketWriteReceipt>>,
}

enum ReplayPayload {
    Producer {
        stanza: Stanza,
        operation: Arc<DriverOperation>,
    },
    Direct {
        id: ActorOutboundId,
        outbound: Arc<OutboundStanza>,
    },
}

enum PendingControl {
    Request {
        _input: QueueItem<ControlInput>,
        request: ClassifiedSmRequest,
        reservation: Arc<DriverAdmissionReservation>,
        ack: Option<SmAck>,
        started: bool,
    },
    PeerAck {
        _input: QueueItem<ControlInput>,
        ack: SmAck,
        _count: OwnedSemaphorePermit,
        _bytes: OwnedSemaphorePermit,
    },
    Unsupported {
        _input: QueueItem<ControlInput>,
    },
}

enum ControlInput {
    Request,
    PeerAck,
    Unsupported,
}

impl PhysicalShared {
    pub(super) fn new(
        connection: ConnectionRef,
        core: ConnectionActorCore,
        parts: PhysicalParts,
        ingress: Arc<crate::ingress::IngressAuthority>,
    ) -> (Arc<Self>, PhysicalParts) {
        let limits = parts.limits;
        (
            Arc::new(Self {
                connection,
                core: Arc::new(AsyncMutex::new(core)),
                direct_custody: Arc::clone(parts.direct.custody()),
                stream: parts.stream.clone(),
                ingress,
                producer_claimed: AtomicBool::new(false),
                limits,
                ledger: Mutex::new(PhysicalLedger::default()),
                control_count: Arc::new(Semaphore::new(1)),
                control_bytes: Arc::new(Semaphore::new(MAX_SM_ACK_OUTPUT_BYTES)),
                changed: Notify::new(),
            }),
            parts,
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PhysicalLedger> {
        self.ledger
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }

    fn try_core(&self, connection: &ConnectionRef) -> Option<OwnedMutexGuard<ConnectionActorCore>> {
        (connection == &self.connection)
            .then(|| Arc::clone(&self.core).try_lock_owned().ok())
            .flatten()
    }

    pub(super) fn snapshot(&self) -> PhysicalSnapshot {
        let ledger = self.lock();
        let mut phase = ledger.active.as_ref().and_then(|(operation, ticket)| {
            let ConnectionActorDriverTaskInput::PhysicalProducer(producer) = &operation.input
            else {
                return None;
            };
            producer.lock_model().phase(ticket).ok()
        });
        if let Some(CurrentWrite {
            kind: CurrentKind::Producer { .. },
            receipt,
        }) = ledger.current.as_ref()
        {
            phase = match receipt.get().map(WebSocketWriteReceipt::progress) {
                Some(WriteProgress::Attempted) => Some(ContinuationPhase::Attempted),
                Some(WriteProgress::Completed) => Some(ContinuationPhase::Flushed),
                Some(WriteProgress::Uncertain) => Some(ContinuationPhase::Uncertain),
                _ => phase,
            };
        }
        PhysicalSnapshot {
            phase,
            progress: ledger
                .current
                .as_ref()
                .and_then(|write| write.receipt.get().map(WebSocketWriteReceipt::progress)),
            appended: ledger.appended,
            attempted: ledger.attempted,
            flushed: ledger.flushed,
            peer_acked: ledger.peer_acked,
            replay_frames: ledger.replay.len(),
            replay_bytes: ledger.replay_bytes,
            checkpoint_h: ledger.checkpoint_h,
            control_pending: ledger.control.is_some(),
            direct_pending: ledger.pending_direct.is_some(),
            stop_forwarded: ledger.stop_forwarded,
            enrollment: ledger.enrollment.unwrap_or(PhysicalEnrollment::NotStarted),
        }
    }

    fn control_pending(&self) -> bool {
        self.lock().control.is_some()
    }

    fn direct_pending(&self) -> bool {
        self.lock().pending_direct.is_some()
    }

    fn replay_has_room(&self, bytes: usize) -> bool {
        let ledger = self.lock();
        ledger.replay.len() < self.limits.max_replay_frames
            && ledger
                .replay_bytes
                .checked_add(bytes)
                .is_some_and(|total| total <= self.limits.max_replay_bytes)
    }

    pub(super) fn is_settled(&self) -> bool {
        let ledger = self.lock();
        ledger.reconciled
            || (matches!(
                ledger.enrollment,
                None | Some(PhysicalEnrollment::NotStarted)
            ) && ledger.active.is_none()
                && ledger.current.is_none()
                && ledger.control.is_none()
                && ledger.pending_direct.is_none()
                && ledger.replay.is_empty())
    }

    pub(super) fn direct_reconciled(&self) -> bool {
        self.lock().reconciled
    }

    fn direct_recovery(&self, id: &ActorOutboundId) -> Option<PhysicalDirectRecovery> {
        if id.connection() != &self.connection {
            return None;
        }
        Some(PhysicalDirectRecovery {
            id: id.clone(),
            outbound: self.direct_custody.snapshot(id)?,
            custody: Arc::clone(&self.direct_custody),
        })
    }

    /// Called only after the exact endpoint confirms both pumps and driver
    /// joined. The caller transfers this result to a new explicit owner.
    pub(super) fn take_joined_recovery(
        &self,
        connection: &ConnectionRef,
    ) -> Option<PhysicalRecovery> {
        let _core = self.try_core(connection)?;
        let mut ledger = self.lock();
        if ledger.reconciled {
            return None;
        }
        let terminal = |receipt: &Arc<OnceLock<WebSocketWriteReceipt>>| {
            receipt.get().is_none_or(|receipt| {
                matches!(
                    receipt.progress(),
                    WriteProgress::Completed
                        | WriteProgress::Uncertain
                        | WriteProgress::NotAttempted
                )
            })
        };
        if ledger
            .current
            .as_ref()
            .is_some_and(|current| !terminal(&current.receipt))
            || ledger.replay.iter().any(|entry| !terminal(&entry.receipt))
        {
            return None;
        }
        let active = match ledger.active.as_ref() {
            Some((operation, ticket)) => {
                let ConnectionActorDriverTaskInput::PhysicalProducer(producer) = &operation.input
                else {
                    return None;
                };
                let model = producer.lock_model();
                Some(PhysicalProducerRecovery {
                    operation_id: operation.id,
                    ordinal: ticket.ordinal(),
                    phase: model.phase(ticket).ok()?,
                    retained_credits: model.retained_credits(),
                    producer: Arc::clone(producer),
                    ticket: ticket.clone(),
                })
            }
            None => None,
        };
        let current = match ledger.current.as_ref() {
            Some(current) => Some(PhysicalWriteRecovery {
                kind: match &current.kind {
                    CurrentKind::Producer { operation, ticket } => PhysicalWriteKind::Producer {
                        operation_id: operation.id,
                        ordinal: ticket.ordinal(),
                    },
                    CurrentKind::Direct(id) => PhysicalWriteKind::Direct(self.direct_recovery(id)?),
                    CurrentKind::Acknowledgement(ack) => PhysicalWriteKind::Acknowledgement(*ack),
                },
                receipt: current.receipt.get().cloned(),
            }),
            None => None,
        };
        let pending_direct = match ledger.pending_direct.as_ref() {
            Some(id) => Some(self.direct_recovery(id)?),
            None => None,
        };
        ledger.active = None;
        ledger.current = None;
        ledger.pending_direct = None;
        let control = ledger.control.take().map(|control| match control {
            PendingControl::Request { ack, started, .. } => {
                PhysicalControlRecovery::Request { ack, started }
            }
            PendingControl::PeerAck { ack, .. } => PhysicalControlRecovery::PeerAck(ack),
            PendingControl::Unsupported { .. } => PhysicalControlRecovery::Unsupported,
        });
        let replay = ledger
            .replay
            .drain(..)
            .map(|entry| PhysicalReplayRecovery {
                sequence: entry.sequence,
                payload: match entry.payload {
                    ReplayPayload::Producer { stanza, operation } => {
                        PhysicalReplayPayload::Producer {
                            stanza,
                            operation_id: operation.id,
                        }
                    }
                    ReplayPayload::Direct { id, outbound } => PhysicalReplayPayload::Direct {
                        id,
                        outbound,
                        custody: Arc::clone(&self.direct_custody),
                    },
                },
                receipt: entry.receipt.get().cloned(),
            })
            .collect();
        ledger.replay_bytes = 0;
        let recovery = PhysicalRecovery {
            connection: self.connection.clone(),
            operations: Vec::new(),
            active,
            current,
            pending_direct,
            control,
            replay,
            checkpoint_h: ledger.checkpoint_h,
            stream: self.stream.clone(),
            enrollment: match ledger.enrollment.unwrap_or(PhysicalEnrollment::NotStarted) {
                PhysicalEnrollment::Running => PhysicalEnrollment::Unknown,
                other => other,
            },
            ingress: Arc::clone(&self.ingress),
        };
        ledger.reconciled = true;
        drop(ledger);
        self.changed.notify_waiters();
        Some(recovery)
    }
}

pub(in crate::server::routes::websocket) struct PhysicalProducer {
    stanza: Stanza,
    frame_count: u64,
    input_bytes: usize,
    max_frame_bytes: usize,
    model: Mutex<OutputContinuation>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    owner: Weak<PhysicalShared>,
}

impl fmt::Debug for PhysicalProducer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PhysicalProducer")
            .field("frame_count", &self.frame_count)
            .field("input_bytes", &self.input_bytes)
            .finish_non_exhaustive()
    }
}

impl PhysicalProducer {
    pub(super) fn new(
        connection: &ConnectionRef,
        stanza: Stanza,
        frame_count: u64,
        max_frame_bytes: usize,
        entered: Arc<Notify>,
        release: Arc<Notify>,
        owner: &Arc<PhysicalShared>,
    ) -> Option<Self> {
        if connection != &owner.connection
            || frame_count == 0
            || max_frame_bytes == 0
            || max_frame_bytes > owner.limits.max_frame_bytes
        {
            return None;
        }
        let mut counter = CappedCount {
            count: 0,
            limit: max_frame_bytes,
        };
        stanza.to_element().write_to(&mut counter).ok()?;
        let model = OutputContinuation::new(
            connection.clone(),
            ContinuationLimits {
                max_frames: 1,
                max_serialized_bytes: max_frame_bytes,
                max_frame_bytes,
            },
        )
        .ok()?;
        owner
            .producer_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Self {
            stanza,
            frame_count,
            input_bytes: counter.count,
            max_frame_bytes,
            model: Mutex::new(model),
            entered,
            release,
            owner: Arc::downgrade(owner),
        })
    }

    pub(super) fn input_bytes(&self) -> usize {
        self.input_bytes
    }

    pub(super) fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes
    }

    pub(super) fn recovery_input(&self) -> (Stanza, u64) {
        (self.stanza.clone(), self.frame_count)
    }

    fn lock_model(&self) -> std::sync::MutexGuard<'_, OutputContinuation> {
        self.model
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }
}

impl Drop for PhysicalProducer {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.producer_claimed.store(false, Ordering::Release);
        }
    }
}

struct CappedCount {
    count: usize,
    limit: usize,
}

impl Write for CappedCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.count = self
            .count
            .checked_add(bytes.len())
            .filter(|count| *count <= self.limit)
            .ok_or_else(|| io::Error::other("producer exceeds its byte reservation"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn retain_inbound(
    shared: &Arc<PhysicalShared>,
    incoming: Option<Result<RetainedWebSocketInput, InputError<axum::Error>>>,
) -> bool {
    let Some(Ok(RetainedWebSocketInput::Frame(item))) = incoming else {
        return false;
    };
    let Message::Text(text) = &item.value else {
        shared.lock().control = Some(PendingControl::Unsupported {
            _input: item.map(|_| ControlInput::Unsupported),
        });
        return false;
    };
    let classified = ClassifiedInboundFrame::classify(text.as_str());
    let request = classified.sm_request();
    let ack = classified.sm_ack();
    let Ok(count) = Arc::clone(&shared.control_count).try_acquire_owned() else {
        return false;
    };
    let Ok(bytes) =
        Arc::clone(&shared.control_bytes).try_acquire_many_owned(MAX_SM_ACK_OUTPUT_BYTES as u32)
    else {
        return false;
    };
    let control = match (request, ack) {
        (Some(request), None) => PendingControl::Request {
            _input: item.map(|_| ControlInput::Request),
            request,
            reservation: Arc::new(DriverAdmissionReservation {
                _count: count,
                _bytes: bytes,
                input_bytes: classified.wire_bytes(),
                output_bound: DriverOutputBound::SmAck,
            }),
            ack: None,
            started: false,
        },
        (None, Some(ack)) => PendingControl::PeerAck {
            _input: item.map(|_| ControlInput::PeerAck),
            ack,
            _count: count,
            _bytes: bytes,
        },
        _ => PendingControl::Unsupported {
            _input: item.map(|_| ControlInput::Unsupported),
        },
    };
    let accepted = !matches!(control, PendingControl::Unsupported { .. });
    shared.lock().control = Some(control);
    shared.changed.notify_waiters();
    accepted
}

struct CheckpointResult {
    result: Result<
        ConnectionActorDriverCompletion<Option<SmAck>>,
        super::super::connection_actor_core::ConnectionActorDriverError,
    >,
    checkpoint_h: Option<u32>,
}

fn begin_checkpoint(
    worker: &DriverWorker,
    parts: &PhysicalParts,
    shared: &Arc<PhysicalShared>,
) -> Option<BoxFuture<'static, CheckpointResult>> {
    let (request, reservation) = {
        let ledger = shared.lock();
        match ledger.control.as_ref()? {
            PendingControl::Request {
                request,
                reservation,
                started: false,
                ..
            } => (*request, Arc::clone(reservation)),
            _ => return None,
        }
    };
    // The app-owned escrow exists before this guard is taken. The guard is
    // acquired synchronously before constructing the future, so dropping an
    // unpolled or aborted future only unlocks the retained exact core.
    let mut core = shared.try_core(&worker.shared.connection_ref)?;
    {
        let mut ledger = shared.lock();
        let Some(PendingControl::Request { started, .. }) = ledger.control.as_mut() else {
            return None;
        };
        if *started {
            return None;
        }
        *started = true;
    }
    let websocket = Arc::clone(&worker.websocket);
    let permit = worker.permit.clone();
    let shutdown = worker.shutdown.clone();
    let stream = parts.stream.clone();
    Some(Box::pin(async move {
        let result = match reservation.sm_request(&request) {
            Some(reserved) => {
                core.process_sm_request(
                    reserved,
                    &websocket,
                    &permit,
                    &shutdown,
                    #[cfg(test)]
                    None,
                )
                .await
            }
            None => Err(
                super::super::connection_actor_core::ConnectionActorDriverError::OutputReservationMissing,
            ),
        };
        let checkpoint_h = websocket
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&stream)
            .await
            .ok()
            .flatten()
            .map(|h| h.to_storage());
        CheckpointResult {
            result,
            checkpoint_h,
        }
    }))
}

type AdmissionResult = (
    WebSocketOutput<axum::Error>,
    Result<(), OutputError<axum::Error>>,
);

struct Runtime {
    enrollment: Option<BoxFuture<'static, bool>>,
    effect: Option<BoxFuture<'static, ()>>,
    checkpoint: Option<BoxFuture<'static, CheckpointResult>>,
    admission: Option<BoxFuture<'static, AdmissionResult>>,
    attempt: Option<BoxFuture<'static, WriteProgress>>,
    terminal: Option<BoxFuture<'static, WriteProgress>>,
    active: Option<ActiveOperationGuard>,
    effect_done: bool,
    stopping: bool,
}

impl Runtime {
    fn new() -> Self {
        Self {
            enrollment: None,
            effect: None,
            checkpoint: None,
            admission: None,
            attempt: None,
            terminal: None,
            active: None,
            effect_done: false,
            stopping: false,
        }
    }

    fn drained(&self) -> bool {
        self.enrollment.is_none()
            && self.effect.is_none()
            && self.checkpoint.is_none()
            && self.admission.is_none()
            && self.attempt.is_none()
            && self.terminal.is_none()
    }
}

async fn poll_slot<T>(slot: &mut Option<BoxFuture<'static, T>>) -> T {
    match slot {
        Some(future) => future.await,
        None => pending().await,
    }
}

fn stop(worker: &DriverWorker, parts: &PhysicalParts, runtime: &mut Runtime, reason: StopReason) {
    if !runtime.stopping {
        runtime.stopping = true;
        worker.shared.request_stop(reason);
        let reason = worker.shared.lock_state().stop_reason.unwrap_or(reason);
        let accepted = parts.custodian.request_physical_stop(reason);
        if let Some(shared) = &worker.shared.physical {
            shared.lock().stop_forwarded = accepted;
            shared.changed.notify_waiters();
        }
    }
}

fn start_producer(
    worker: &DriverWorker,
    shared: &Arc<PhysicalShared>,
    runtime: &mut Runtime,
    operation: Arc<DriverOperation>,
) -> bool {
    let ConnectionActorDriverTaskInput::PhysicalProducer(producer) = &operation.input else {
        return false;
    };
    if runtime.active.is_some() {
        return false;
    }
    if !matches!(
        worker
            .permit
            .with_current(|| worker.shared.begin_operation(&operation)),
        Ok(true)
    ) {
        return false;
    }
    let ticket = match producer.lock_model().reserve_next() {
        Ok(ticket) => ticket,
        Err(_) => return false,
    };
    shared.lock().active = Some((Arc::clone(&operation), ticket));
    runtime.active = Some(ActiveOperationGuard::new(
        Arc::clone(&operation),
        Arc::clone(&worker.shared),
    ));
    let entered = Arc::clone(&producer.entered);
    let release = Arc::clone(&producer.release);
    runtime.effect = Some(Box::pin(async move {
        entered.notify_one();
        release.notified().await;
    }));
    shared.changed.notify_waiters();
    true
}

fn finish_effect(shared: &Arc<PhysicalShared>, runtime: &mut Runtime) -> bool {
    let (operation, ticket) = {
        let ledger = shared.lock();
        let Some((operation, ticket)) = ledger.active.as_ref() else {
            return false;
        };
        (Arc::clone(operation), ticket.clone())
    };
    let ConnectionActorDriverTaskInput::PhysicalProducer(producer) = &operation.input else {
        return false;
    };
    let mut model = producer.lock_model();
    let prepared = model
        .finish_preparation(&ticket, Some(producer.stanza.clone()))
        .is_ok();
    if runtime.stopping {
        model.stop();
    }
    runtime.effect_done = true;
    drop(model);
    shared.changed.notify_waiters();
    prepared
}

fn finish_checkpoint(shared: &Arc<PhysicalShared>, outcome: CheckpointResult) -> bool {
    let (ack, current) = match outcome.result {
        Ok(ConnectionActorDriverCompletion::CurrentAtCompletion(Some(ack))) => (Some(ack), true),
        Ok(ConnectionActorDriverCompletion::CompletionUnknown { retained, .. }) => {
            (retained, false)
        }
        _ => (None, false),
    };
    let mut ledger = shared.lock();
    ledger.checkpoint_h = outcome.checkpoint_h;
    if let Some(PendingControl::Request { ack: stored, .. }) = ledger.control.as_mut() {
        *stored = ack;
    }
    drop(ledger);
    shared.changed.notify_waiters();
    current && ack.is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AckDecision {
    None,
    Deferred,
    Applied,
    Invalid,
}

#[derive(Clone, Copy)]
enum CurrentProgress {
    NoData,
    Data(Option<WriteProgress>),
}

fn decide_peer_ack(
    old: u32,
    attempted: u32,
    appended: u32,
    ack: u32,
    current: CurrentProgress,
) -> (AckDecision, u32) {
    let distance = ack.wrapping_sub(old);
    let regressed = ack != old && distance > 0x8000_0000;
    let mut frontier = attempted;
    if !regressed && distance > frontier.wrapping_sub(old) {
        match current {
            CurrentProgress::Data(Some(WriteProgress::Attempted | WriteProgress::Completed)) => {
                frontier = appended;
            }
            CurrentProgress::Data(
                None | Some(WriteProgress::Admitted | WriteProgress::Uncertain),
            ) => {
                return (AckDecision::Deferred, frontier);
            }
            CurrentProgress::Data(Some(WriteProgress::NotAttempted)) | CurrentProgress::NoData => {
                return (AckDecision::Invalid, frontier);
            }
        }
    }
    if !regressed && distance > frontier.wrapping_sub(old) {
        (AckDecision::Invalid, frontier)
    } else {
        (AckDecision::Applied, frontier)
    }
}

fn process_peer_ack(shared: &Arc<PhysicalShared>) -> AckDecision {
    let mut ledger = shared.lock();
    let ack = match ledger.control.as_ref() {
        Some(PendingControl::PeerAck { ack, .. }) => *ack,
        _ => return AckDecision::None,
    };
    let old = ledger.peer_acked;
    let distance = ack.h.wrapping_sub(old);
    let regressed = ack.h != old && distance > 0x8000_0000;
    let current = match ledger.current.as_ref() {
        Some(current) if !matches!(current.kind, CurrentKind::Acknowledgement(_)) => {
            CurrentProgress::Data(current.receipt.get().map(WebSocketWriteReceipt::progress))
        }
        _ => CurrentProgress::NoData,
    };
    let (decision, frontier) =
        decide_peer_ack(old, ledger.attempted, ledger.appended, ack.h, current);
    if decision != AckDecision::Applied {
        return decision;
    }
    ledger.attempted = frontier;
    if !regressed {
        while ledger.replay.front().is_some_and(|entry| {
            let offset = entry.sequence.wrapping_sub(old);
            offset != 0 && offset <= distance
        }) {
            if let Some(entry) = ledger.replay.pop_front() {
                ledger.replay_bytes -= entry.charged_bytes;
            }
        }
        ledger.peer_acked = ack.h;
    }
    ledger.control = None;
    drop(ledger);
    shared.changed.notify_waiters();
    AckDecision::Applied
}

fn observe_attempt(shared: &Arc<PhysicalShared>, progress: WriteProgress) -> bool {
    let mut ledger = shared.lock();
    let Some(current) = ledger.current.as_ref() else {
        return false;
    };
    if matches!(
        progress,
        WriteProgress::Attempted | WriteProgress::Completed
    ) {
        if let CurrentKind::Producer { operation, ticket } = &current.kind {
            let ConnectionActorDriverTaskInput::PhysicalProducer(producer) = &operation.input
            else {
                return false;
            };
            let mut model = producer.lock_model();
            if model.phase(ticket) == Ok(ContinuationPhase::Admitted)
                && model.attempted(ticket).is_err()
            {
                return false;
            }
        }
        if !matches!(current.kind, CurrentKind::Acknowledgement(_)) {
            ledger.attempted = ledger.appended;
        }
    }
    drop(ledger);
    shared.changed.notify_waiters();
    true
}

fn observe_terminal(shared: &Arc<PhysicalShared>, progress: WriteProgress) -> bool {
    let mut ledger = shared.lock();
    let Some(current) = ledger.current.as_ref() else {
        return false;
    };
    if let CurrentKind::Producer { operation, ticket } = &current.kind {
        let ConnectionActorDriverTaskInput::PhysicalProducer(producer) = &operation.input else {
            return false;
        };
        let mut model = producer.lock_model();
        match progress {
            WriteProgress::Completed => {
                if model.phase(ticket) == Ok(ContinuationPhase::Admitted)
                    && model.attempted(ticket).is_err()
                {
                    return false;
                }
                if model.mark_flushed(ticket).is_err() {
                    return false;
                }
            }
            WriteProgress::Uncertain => {
                let _ = model.mark_uncertain(ticket);
            }
            WriteProgress::NotAttempted => model.stop(),
            WriteProgress::Admitted | WriteProgress::Attempted => return false,
        }
    }
    if progress != WriteProgress::Completed {
        return false;
    }
    match &current.kind {
        CurrentKind::Producer { .. } | CurrentKind::Direct(_) => {
            ledger.attempted = ledger.appended;
            ledger.flushed = ledger.flushed.saturating_add(1);
        }
        CurrentKind::Acknowledgement(_) => {
            ledger.control = None;
        }
    }
    ledger.current = None;
    drop(ledger);
    shared.changed.notify_waiters();
    true
}

fn encode(stanza: &Stanza, limit: usize) -> Option<String> {
    let mut counter = CappedCount { count: 0, limit };
    stanza.to_element().write_to(&mut counter).ok()?;
    let mut bytes = Vec::with_capacity(counter.count);
    stanza.to_element().write_to(&mut bytes).ok()?;
    String::from_utf8(bytes).ok()
}

fn launch_write(
    parts: &mut PhysicalParts,
    shared: &Arc<PhysicalShared>,
    runtime: &mut Runtime,
    kind: CurrentKind,
    frame: WebSocketOutboundFrame,
    replay: Option<(ReplayPayload, usize)>,
) -> bool {
    let Some(mut output) = parts.output.take() else {
        return false;
    };
    let receipt = Arc::new(OnceLock::new());
    {
        let mut ledger = shared.lock();
        if ledger.current.is_some() {
            parts.output = Some(output);
            return false;
        }
        if let Some((payload, bytes)) = replay {
            let sequence = ledger.appended.wrapping_add(1);
            ledger.replay.push_back(ReplayEntry {
                sequence,
                payload,
                charged_bytes: bytes,
                receipt: Arc::clone(&receipt),
            });
            ledger.replay_bytes += bytes;
            ledger.appended = sequence;
        }
        ledger.current = Some(CurrentWrite {
            kind,
            receipt: Arc::clone(&receipt),
        });
    }
    runtime.admission = Some(Box::pin(async move {
        let result = output.submit(frame).await;
        let result = match result {
            Ok(actual) => {
                // This slot is new and has one writer. Publish the actual
                // receipt in the same poll that returns from `submit`.
                let _ = receipt.set(actual);
                Ok(())
            }
            Err(error) => Err(error),
        };
        (output, result)
    }));
    shared.changed.notify_waiters();
    true
}

fn start_control_write(
    parts: &mut PhysicalParts,
    shared: &Arc<PhysicalShared>,
    runtime: &mut Runtime,
) -> Option<bool> {
    let ack = match shared.lock().control.as_ref() {
        Some(PendingControl::Request { ack: Some(ack), .. }) => *ack,
        _ => return None,
    };
    let Some(output) = parts.output.as_ref() else {
        return Some(false);
    };
    let Ok(frame) = output.data_frame(Message::Text(ack.to_xml().into())) else {
        return Some(false);
    };
    Some(launch_write(
        parts,
        shared,
        runtime,
        CurrentKind::Acknowledgement(ack),
        frame,
        None,
    ))
}

fn start_direct_write(
    parts: &mut PhysicalParts,
    shared: &Arc<PhysicalShared>,
    runtime: &mut Runtime,
) -> Option<bool> {
    let id = shared.lock().pending_direct.clone()?;
    if id.connection() != &shared.connection {
        return Some(false);
    }
    let custody = Arc::clone(parts.direct.custody());
    let Some(outbound) = custody.snapshot(&id) else {
        return Some(false);
    };
    if outbound.kind != DeliveryKind::DirectFrame {
        return Some(false);
    }
    let Some(xml) = encode(&outbound.stanza, shared.limits.max_frame_bytes) else {
        return Some(false);
    };
    if !shared.replay_has_room(xml.len()) {
        return None;
    }
    let bytes = xml.len();
    let Some(output) = parts.output.as_ref() else {
        return Some(false);
    };
    let Ok(frame) = output.actor_data_frame(Message::Text(xml.into()), custody, id.clone()) else {
        return Some(false);
    };
    let started = launch_write(
        parts,
        shared,
        runtime,
        CurrentKind::Direct(id.clone()),
        frame,
        Some((ReplayPayload::Direct { id, outbound }, bytes)),
    );
    if started {
        shared.lock().pending_direct = None;
    }
    Some(started)
}

fn start_producer_write(
    parts: &mut PhysicalParts,
    shared: &Arc<PhysicalShared>,
    runtime: &mut Runtime,
) -> Option<bool> {
    if !runtime.effect_done || parts.first_handled.is_some() {
        return None;
    }
    let (operation, ticket) = {
        let ledger = shared.lock();
        let (operation, ticket) = ledger.active.as_ref()?;
        (Arc::clone(operation), ticket.clone())
    };
    let ConnectionActorDriverTaskInput::PhysicalProducer(producer) = &operation.input else {
        return Some(false);
    };
    if !shared.replay_has_room(producer.input_bytes) {
        return None;
    }
    let stanza = match producer.lock_model().stanza_snapshot(&ticket) {
        Ok(stanza) => stanza,
        Err(_) => return Some(false),
    };
    let Some(xml) = encode(&stanza, producer.max_frame_bytes) else {
        return Some(false);
    };
    if xml.len() != producer.input_bytes {
        return Some(false);
    }
    let bytes = xml.len();
    let Some(output) = parts.output.as_ref() else {
        return Some(false);
    };
    let Ok(frame) = output.data_frame(Message::Text(xml.into())) else {
        return Some(false);
    };
    Some(launch_write(
        parts,
        shared,
        runtime,
        CurrentKind::Producer {
            operation: Arc::clone(&operation),
            ticket,
        },
        frame,
        Some((ReplayPayload::Producer { stanza, operation }, bytes)),
    ))
}

fn advance_producer(
    worker: &DriverWorker,
    parts: &mut PhysicalParts,
    shared: &Arc<PhysicalShared>,
    runtime: &mut Runtime,
) -> bool {
    if !runtime.effect_done {
        return true;
    }
    if let Some(sequence) = parts.first_handled {
        let Some(mut core) = shared.try_core(&shared.connection) else {
            return runtime.checkpoint.is_some();
        };
        core.complete_fixture_handled(sequence);
        drop(core);
        parts.first_handled = None;
    }
    let (operation, ticket) = {
        let ledger = shared.lock();
        let Some((operation, ticket)) = ledger.active.as_ref() else {
            return true;
        };
        (Arc::clone(operation), ticket.clone())
    };
    let ConnectionActorDriverTaskInput::PhysicalProducer(producer) = &operation.input else {
        return false;
    };
    if producer.lock_model().phase(&ticket) != Ok(ContinuationPhase::Flushed) {
        return true;
    }
    if ticket.ordinal().saturating_add(1) >= producer.frame_count {
        let Some(mut active) = runtime.active.take() else {
            return false;
        };
        let mut result = Some(ConnectionActorDriverTaskResultValue::PhysicalProducer);
        match worker
            .permit
            .with_current(|| active.complete(result.take().expect("result retained")))
        {
            Ok(Some(reason)) => worker.shared.request_stop(reason),
            Ok(None) => {}
            Err(_) => active.complete_non_current(result.expect("result retained on revoke")),
        }
        shared.lock().active = None;
        runtime.effect_done = false;
        shared.changed.notify_waiters();
        return true;
    }
    let next = {
        let mut model = producer.lock_model();
        let Ok(next) = model.reserve_next() else {
            return false;
        };
        if model
            .finish_preparation(&next, Some(producer.stanza.clone()))
            .is_err()
        {
            return false;
        }
        next
    };
    shared.lock().active = Some((operation, next));
    shared.changed.notify_waiters();
    true
}

fn finish_admission(
    worker: &DriverWorker,
    parts: &mut PhysicalParts,
    shared: &Arc<PhysicalShared>,
    runtime: &mut Runtime,
    outcome: AdmissionResult,
) -> bool {
    let (output, result) = outcome;
    parts.output = Some(output);
    if result.is_err() {
        return false;
    }
    let receipt = {
        let ledger = shared.lock();
        let Some(current) = ledger.current.as_ref() else {
            return false;
        };
        let Some(receipt) = current.receipt.get().cloned() else {
            return false;
        };
        if let CurrentKind::Producer { operation, ticket } = &current.kind {
            let ConnectionActorDriverTaskInput::PhysicalProducer(producer) = &operation.input
            else {
                return false;
            };
            if producer.lock_model().admit(ticket).is_err() {
                return false;
            }
        }
        receipt
    };
    if !receipt.is_for(&worker.shared.connection_ref) {
        return false;
    }
    let attempt = receipt.clone();
    runtime.attempt = Some(Box::pin(async move { attempt.wait_for_attempt().await }));
    runtime.terminal = Some(Box::pin(async move { receipt.wait_for_terminal().await }));
    shared.changed.notify_waiters();
    true
}

enum Event {
    Stop,
    Enrollment(bool),
    Effect,
    Checkpoint(CheckpointResult),
    Admission(AdmissionResult),
    Attempt(WriteProgress),
    Terminal(WriteProgress),
    Inbound(Option<Result<RetainedWebSocketInput, InputError<axum::Error>>>),
    Direct(Option<ActorOutboundId>),
    Command(Option<super::DriverCommand>),
    Control(Option<DriverControl>),
}

pub(super) async fn run(mut worker: DriverWorker, mut parts: PhysicalParts) {
    let Some(shared) = worker.shared.physical.clone() else {
        worker.shared.request_stop(StopReason::Requested);
        return;
    };
    let mut runtime = Runtime::new();
    shared.lock().enrollment = Some(PhysicalEnrollment::Running);
    let ingress = Arc::clone(&shared.ingress);
    let stream = shared.stream.clone();
    runtime.enrollment = Some(Box::pin(async move {
        ingress.enroll_stream(&stream).await.is_ok()
    }));
    let mut direct_open = true;
    let mut control_open = true;
    let mut commands_open = true;
    loop {
        if worker.stop.is_cancelled() && !runtime.stopping {
            let reason = worker
                .shared
                .lock_state()
                .stop_reason
                .unwrap_or(StopReason::Requested);
            stop(&worker, &parts, &mut runtime, reason);
        }
        let enrolled = shared.lock().enrollment == Some(PhysicalEnrollment::Enrolled);
        if !runtime.stopping && enrolled {
            if !advance_producer(&worker, &mut parts, &shared, &mut runtime) {
                stop(&worker, &parts, &mut runtime, StopReason::Requested);
                continue;
            }
            if runtime.checkpoint.is_none() {
                runtime.checkpoint = begin_checkpoint(&worker, &parts, &shared);
            }
            if matches!(process_peer_ack(&shared), AckDecision::Invalid) {
                stop(&worker, &parts, &mut runtime, StopReason::Requested);
                continue;
            }
            let idle = runtime.admission.is_none()
                && runtime.terminal.is_none()
                && shared.lock().current.is_none();
            if idle {
                let started =
                    start_control_write(&mut parts, &shared, &mut runtime).or_else(|| {
                        if shared.control_pending() {
                            return None;
                        }
                        start_direct_write(&mut parts, &shared, &mut runtime).or_else(|| {
                            (!shared.direct_pending())
                                .then(|| start_producer_write(&mut parts, &shared, &mut runtime))
                                .flatten()
                        })
                    });
                if started == Some(false) {
                    stop(&worker, &parts, &mut runtime, StopReason::Requested);
                    continue;
                }
            }
        }
        if runtime.stopping && runtime.drained() {
            return;
        }
        let event = tokio::select! {
            biased;
            _ = worker.stop.cancelled(), if !runtime.stopping => Event::Stop,
            result = poll_slot(&mut runtime.enrollment), if runtime.enrollment.is_some() => Event::Enrollment(result),
            _ = poll_slot(&mut runtime.effect), if runtime.effect.is_some() => Event::Effect,
            result = poll_slot(&mut runtime.checkpoint), if runtime.checkpoint.is_some() => Event::Checkpoint(result),
            result = poll_slot(&mut runtime.admission), if runtime.admission.is_some() => Event::Admission(result),
            result = poll_slot(&mut runtime.attempt), if runtime.attempt.is_some() => Event::Attempt(result),
            result = poll_slot(&mut runtime.terminal), if runtime.terminal.is_some() => Event::Terminal(result),
            incoming = parts.input.recv_retained(), if !runtime.stopping && enrolled && !shared.control_pending() => Event::Inbound(incoming),
            direct = parts.direct.recv(), if !runtime.stopping && enrolled && direct_open && !shared.direct_pending() => Event::Direct(direct),
            command = worker.receiver.recv(), if !runtime.stopping && enrolled && commands_open && runtime.active.is_none() => Event::Command(command),
            control = worker.control_receiver.recv(), if !runtime.stopping && control_open => Event::Control(control),
        };
        match event {
            Event::Stop => stop(&worker, &parts, &mut runtime, StopReason::Requested),
            Event::Enrollment(enrolled) => {
                runtime.enrollment = None;
                shared.lock().enrollment = Some(if enrolled {
                    PhysicalEnrollment::Enrolled
                } else {
                    PhysicalEnrollment::Unknown
                });
                shared.changed.notify_waiters();
                if !enrolled {
                    stop(&worker, &parts, &mut runtime, StopReason::Requested);
                }
            }
            Event::Effect => {
                runtime.effect = None;
                if !finish_effect(&shared, &mut runtime) {
                    stop(&worker, &parts, &mut runtime, StopReason::Requested);
                }
            }
            Event::Checkpoint(outcome) => {
                runtime.checkpoint = None;
                if !finish_checkpoint(&shared, outcome) {
                    stop(&worker, &parts, &mut runtime, StopReason::Requested);
                }
            }
            Event::Admission(outcome) => {
                runtime.admission = None;
                if !finish_admission(&worker, &mut parts, &shared, &mut runtime, outcome) {
                    stop(&worker, &parts, &mut runtime, StopReason::Requested);
                }
            }
            Event::Attempt(progress) => {
                runtime.attempt = None;
                if !observe_attempt(&shared, progress) {
                    stop(&worker, &parts, &mut runtime, StopReason::Requested);
                }
            }
            Event::Terminal(progress) => {
                runtime.terminal = None;
                runtime.attempt = None;
                if !observe_terminal(&shared, progress) {
                    stop(&worker, &parts, &mut runtime, StopReason::Requested);
                }
            }
            Event::Inbound(incoming) => {
                if !retain_inbound(&shared, incoming) {
                    stop(&worker, &parts, &mut runtime, StopReason::Requested);
                }
            }
            Event::Direct(Some(id)) => {
                shared.lock().pending_direct = Some(id);
                shared.changed.notify_waiters();
            }
            Event::Direct(None) => direct_open = false,
            Event::Command(Some(command)) => {
                if !start_producer(&worker, &shared, &mut runtime, command.operation) {
                    stop(&worker, &parts, &mut runtime, StopReason::Requested);
                }
            }
            Event::Command(None) => commands_open = false,
            Event::Control(Some(DriverControl::Barrier(complete))) => {
                let _ = complete.send(());
            }
            Event::Control(None) => control_open = false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{decide_peer_ack, AckDecision, CurrentProgress};
    use crate::server::routes::websocket::supervised_io::WriteProgress;

    #[test]
    fn earlier_attempted_frame_can_be_acked_after_later_failure() {
        for later in [WriteProgress::NotAttempted, WriteProgress::Uncertain] {
            assert_eq!(
                decide_peer_ack(0, 1, 2, 1, CurrentProgress::Data(Some(later))),
                (AckDecision::Applied, 1)
            );
        }
        assert_eq!(
            decide_peer_ack(
                0,
                1,
                2,
                2,
                CurrentProgress::Data(Some(WriteProgress::NotAttempted))
            ),
            (AckDecision::Invalid, 1)
        );
        assert_eq!(
            decide_peer_ack(
                0,
                1,
                2,
                2,
                CurrentProgress::Data(Some(WriteProgress::Uncertain))
            ),
            (AckDecision::Deferred, 1)
        );
    }

    #[test]
    fn fast_ack_waits_for_attempt_and_regressed_ack_is_ignored() {
        assert_eq!(
            decide_peer_ack(
                0,
                0,
                1,
                1,
                CurrentProgress::Data(Some(WriteProgress::Admitted))
            ),
            (AckDecision::Deferred, 0)
        );
        assert_eq!(
            decide_peer_ack(
                0,
                0,
                1,
                1,
                CurrentProgress::Data(Some(WriteProgress::Attempted))
            ),
            (AckDecision::Applied, 1)
        );
        assert_eq!(
            decide_peer_ack(2, 3, 3, 1, CurrentProgress::NoData),
            (AckDecision::Applied, 3)
        );
        assert_eq!(
            decide_peer_ack(u32::MAX - 1, 0, 0, 0, CurrentProgress::NoData),
            (AckDecision::Applied, 0)
        );
    }
}
