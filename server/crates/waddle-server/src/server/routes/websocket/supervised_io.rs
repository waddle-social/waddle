//! Test-gated owner for bounded WebSocket I/O pumps.
//!
//! This module is an isolated transport boundary for #2046. The production
//! connection loop does not instantiate it yet. It carries the physical
//! connection reference through task retirement, bounds queued items by count
//! and bytes, and reports local write progress without making an XMPP or
//! durable-custody claim.

use futures::{Sink, SinkExt, Stream, StreamExt};
use std::collections::HashMap;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::ConnectionRef;

const WRITE_ADMITTED: u8 = 0;
const WRITE_ATTEMPTED: u8 = 1;
const WRITE_COMPLETED: u8 = 2;
const WRITE_UNCERTAIN: u8 = 3;
const WRITE_NOT_ATTEMPTED: u8 = 4;

/// Count and total byte bounds for one direction of a transport queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    max_count: usize,
    max_bytes: usize,
}

impl Limits {
    /// Create positive queue bounds supported by Tokio's semaphore.
    pub fn new(max_count: usize, max_bytes: usize) -> Result<Self, LimitsError> {
        if max_count == 0
            || max_bytes == 0
            || max_count > Semaphore::MAX_PERMITS
            || max_bytes > Semaphore::MAX_PERMITS
        {
            return Err(LimitsError);
        }
        Ok(Self {
            max_count,
            max_bytes,
        })
    }
}

/// Independent inbound and outbound queue budgets for one transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportLimits {
    inbound: Limits,
    outbound: Limits,
}

impl TransportLimits {
    /// Create a transport budget from already validated directional limits.
    pub fn new(inbound: Limits, outbound: Limits) -> Self {
        Self { inbound, outbound }
    }
}

/// WebSocket Close frame and exact peer-close rejection classifier.
pub struct ShutdownProtocol<M, IsSendAfterClose> {
    shutdown_frame: M,
    is_send_after_close: IsSendAfterClose,
}

impl<M, IsSendAfterClose> ShutdownProtocol<M, IsSendAfterClose> {
    /// Create a close protocol description. The classifier should recognize
    /// only the sink error that means a peer Close already changed write state.
    pub fn new(shutdown_frame: M, is_send_after_close: IsSendAfterClose) -> Self {
        Self {
            shutdown_frame,
            is_send_after_close,
        }
    }
}

/// Invalid count or byte bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("queue limits must be positive and fit Tokio's semaphore capacity")]
pub struct LimitsError;

/// First reason that stopped a transport's pumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Requested,
    InboundOverload,
    OutboundOverload,
    InboundQueueClosed,
    OutboundQueueClosed,
    PeerClosed,
    InboundIoFailure,
    OutboundIoFailure,
    PumpExited,
    ShutdownDeadline,
    /// Interrupts the pumps after local lease loss, but is not an authority
    /// fence for a write already crossing the sink effect boundary.
    LeaseLost,
}

/// Shared, first-wins cancellation signal with a typed reason.
#[derive(Clone)]
pub struct StopSignal {
    token: CancellationToken,
    reason: Arc<Mutex<Option<StopReason>>>,
}

impl StopSignal {
    fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            reason: Arc::new(Mutex::new(None)),
        }
    }

    /// Request pump cancellation. The first reason remains authoritative.
    pub fn cancel(&self, reason: StopReason) {
        let mut current = match self.reason.lock() {
            Ok(current) => current,
            Err(poisoned) => poisoned.into_inner(),
        };
        if current.is_none() {
            *current = Some(reason);
        }
        self.token.cancel();
    }

    /// Wait until cancellation is requested.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// First cancellation reason, when present.
    pub fn reason(&self) -> Option<StopReason> {
        match self.reason.lock() {
            Ok(current) => *current,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

/// Why queue admission failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    Overloaded,
    Closed,
    Stopped,
    Shutdown,
}

/// One admitted queue item. Its permits remain held until this value drops.
pub struct QueueItem<T> {
    /// Admitted value.
    pub value: T,
    _count_permit: OwnedSemaphorePermit,
    _byte_permit: OwnedSemaphorePermit,
}

impl<T> QueueItem<T> {
    /// Consume the queue item and release its capacity permits.
    pub fn into_inner(self) -> T {
        self.value
    }
}

/// Bounded queue sender that signals the transport before returning overload.
///
/// A stop can race with an admission between the final stopped check and the
/// channel send. Callers must still fence completion effects with the carried
/// `ConnectionRef`; this queue does not provide an atomic authority transition.
pub struct BoundedSender<T> {
    sender: mpsc::Sender<QueueItem<T>>,
    count: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    stop: StopSignal,
    overload_reason: StopReason,
    closed_reason: StopReason,
}

impl<T> BoundedSender<T> {
    fn new(
        sender: mpsc::Sender<QueueItem<T>>,
        limits: Limits,
        stop: StopSignal,
        overload_reason: StopReason,
        closed_reason: StopReason,
    ) -> Self {
        Self {
            sender,
            count: Arc::new(Semaphore::new(limits.max_count)),
            bytes: Arc::new(Semaphore::new(limits.max_bytes)),
            stop,
            overload_reason,
            closed_reason,
        }
    }

    /// Try to admit an item without waiting for queue capacity.
    pub fn try_send(&self, value: T, byte_len: usize) -> Result<(), AdmissionError> {
        if self.stop.is_cancelled() {
            return Err(AdmissionError::Stopped);
        }
        let result = (|| {
            let count_permit = self
                .count
                .clone()
                .try_acquire_owned()
                .map_err(|_| AdmissionError::Overloaded)?;
            let byte_len = u32::try_from(byte_len).map_err(|_| AdmissionError::Overloaded)?;
            let byte_permit = self
                .bytes
                .clone()
                .try_acquire_many_owned(byte_len)
                .map_err(|_| AdmissionError::Overloaded)?;
            if self.stop.is_cancelled() {
                return Err(AdmissionError::Stopped);
            }
            self.sender
                .try_send(QueueItem {
                    value,
                    _count_permit: count_permit,
                    _byte_permit: byte_permit,
                })
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => AdmissionError::Overloaded,
                    mpsc::error::TrySendError::Closed(_) => AdmissionError::Closed,
                })
        })();

        match result {
            Ok(()) => Ok(()),
            Err(AdmissionError::Overloaded) => {
                self.stop.cancel(self.overload_reason);
                Err(AdmissionError::Overloaded)
            }
            Err(AdmissionError::Closed) => {
                self.stop.cancel(self.closed_reason);
                Err(AdmissionError::Closed)
            }
            Err(AdmissionError::Stopped) => Err(AdmissionError::Stopped),
            Err(AdmissionError::Shutdown) => Err(AdmissionError::Shutdown),
        }
    }
}

/// Locally observed progress for an admitted write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteProgress {
    Admitted,
    /// The sink accepted `start_send`; a later flush can still be uncertain.
    Attempted,
    Completed,
    /// An accepted sink write did not reach a completed local flush.
    Uncertain,
    /// The sink rejected the write before `start_send` returned success.
    NotAttempted,
}

/// Receipt for one outbound item. Completion means the local sink flush
/// completed; it does not mean the peer processed or acknowledged the frame.
/// A later exact transport rejection does not downgrade an already published
/// `Attempted` milestone; it remains `Uncertain` because the receipt does not
/// distinguish a sink-buffered frame from one that reached the transport.
#[derive(Debug, Clone)]
pub struct WriteReceipt(Arc<AtomicU8>);

impl WriteReceipt {
    fn new() -> Self {
        Self(Arc::new(AtomicU8::new(WRITE_ADMITTED)))
    }

    /// Current local write milestone.
    pub fn progress(&self) -> WriteProgress {
        match self.0.load(Ordering::Acquire) {
            WRITE_ADMITTED => WriteProgress::Admitted,
            WRITE_ATTEMPTED => WriteProgress::Attempted,
            WRITE_COMPLETED => WriteProgress::Completed,
            WRITE_NOT_ATTEMPTED => WriteProgress::NotAttempted,
            _ => WriteProgress::Uncertain,
        }
    }
}

struct WriteRequest<T> {
    value: Option<T>,
    receipt: WriteReceipt,
}

impl<T> Drop for WriteRequest<T> {
    fn drop(&mut self) {
        if self.value.is_some() {
            let _ = self.receipt.0.compare_exchange(
                WRITE_ADMITTED,
                WRITE_NOT_ATTEMPTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

/// Bounded outbound queue whose sends return their local progress receipt.
pub struct WriteSender<T> {
    sender: BoundedSender<WriteRequest<T>>,
    admission_open: Arc<Mutex<bool>>,
}

impl<T> WriteSender<T> {
    /// Admit one outbound value under the queue's count and byte bounds.
    pub fn try_send(&self, value: T, byte_len: usize) -> Result<WriteReceipt, AdmissionError> {
        let admission_open = match self.admission_open.lock() {
            Ok(admission_open) => admission_open,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !*admission_open {
            return Err(AdmissionError::Shutdown);
        }
        let receipt = WriteReceipt::new();
        self.sender.try_send(
            WriteRequest {
                value: Some(value),
                receipt: receipt.clone(),
            },
            byte_len,
        )?;
        Ok(receipt)
    }
}

/// How a pump ended.
#[derive(Debug, PartialEq, Eq)]
pub enum InboundExit<E> {
    Stopped,
    PeerClosed,
    Overloaded,
    IoFailure(E),
}

/// How the outbound pump ended.
#[derive(Debug, PartialEq, Eq)]
pub enum OutboundExit<E> {
    Stopped,
    ShutdownClose,
    PeerClosedReplyFlushed,
    QueueClosed,
    IoFailure(E),
}

/// How a supervisor retires its two pump tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetirementMode {
    /// Send a local Close and wait for peer Close or EOF. Use
    /// `shutdown_with_deadline` when retirement must be bounded.
    Graceful,
    Forced,
}

/// Results from both joined pumps, tagged with the exact physical transport.
#[derive(Debug)]
pub struct Joined<ReadError, WriteError> {
    connection_ref: ConnectionRef,
    /// Reader task's independent completion result.
    pub inbound: Result<InboundExit<ReadError>, JoinError>,
    /// Writer task's independent completion result.
    pub outbound: Result<OutboundExit<WriteError>, JoinError>,
}

impl<ReadError, WriteError> Joined<ReadError, WriteError> {
    /// Connection reference carried by the retired tasks.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// Whether this late completion belongs to the supplied physical socket.
    pub fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        &self.connection_ref == connection_ref
    }
}

/// Owns the two split halves' pump tasks and their retirement signals.
pub struct IoSupervisor<ReadError, WriteError> {
    connection_ref: ConnectionRef,
    stop: StopSignal,
    force_stop: CancellationToken,
    shutdown: CancellationToken,
    admission_open: Arc<Mutex<bool>>,
    inbound: TaskOwner<InboundExit<ReadError>>,
    outbound: TaskOwner<OutboundExit<WriteError>>,
}

/// Lifecycle state published by the independent physical-connection custodian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodianState {
    /// Inventory admitted this exact connection, but its pump pair is not
    /// fully started yet.
    Starting,
    /// The custodian owns both pump handles and has not begun retirement.
    Running,
    /// Stop was requested or one pump ended; the custodian is joining both.
    Stopping,
    /// At least one started pump still lacks confirmed join proof because the
    /// retirement deadline elapsed or its owner was lost. This never claims
    /// the socket has terminated.
    CleanupPending,
    /// Both pump JoinHandles returned, including cancellation or panic results.
    Joined,
    /// Startup ended before any pump task started; no task remains to join.
    StartupAbandoned,
    /// Every pump that did start returned, but one member of the pair never
    /// started. This is deliberately distinct from two-pump `Joined`.
    PartialStartJoined,
}

/// Observable stages in the reserved pair-start sequence, exposed so tests
/// can hold or cancel startup at ownership boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedStartStage {
    BeforeInboundSpawn,
    BetweenPumpSpawns,
    AfterOutboundSpawn,
    AfterCommit,
}

/// Deterministic reaper-loss point used only by lifecycle tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedReaperExitPoint {
    BeforeCommit,
    BeforeReady,
}

/// Validated bounds and pump behavior for one reserved physical connection.
pub struct ReservedIoStartConfig<M, IsSendAfterClose, Size, IsClose> {
    limits: TransportLimits,
    shutdown_protocol: ShutdownProtocol<M, IsSendAfterClose>,
    frame_size: Size,
    is_close: IsClose,
    retirement_deadline: Duration,
    reaper_exit_point: Option<ReservedReaperExitPoint>,
}

impl<M, IsSendAfterClose, Size, IsClose> ReservedIoStartConfig<M, IsSendAfterClose, Size, IsClose>
where
    Size: Fn(&M) -> usize,
    IsClose: Fn(&M) -> bool,
{
    /// Build a startup configuration for a previously reserved connection.
    pub fn new(
        limits: TransportLimits,
        shutdown_protocol: ShutdownProtocol<M, IsSendAfterClose>,
        frame_size: Size,
        is_close: IsClose,
        retirement_deadline: Duration,
    ) -> Self {
        Self {
            limits,
            shutdown_protocol,
            frame_size,
            is_close,
            retirement_deadline,
            reaper_exit_point: None,
        }
    }

    /// Inject reaper loss at a startup boundary for lifecycle tests.
    pub fn with_test_reaper_exit_point(mut self, exit_point: ReservedReaperExitPoint) -> Self {
        self.reaper_exit_point = Some(exit_point);
        self
    }
}

/// Terminal outcome of the inbound pump, summarized without retaining its
/// generic I/O error value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundTerminal {
    NotStarted,
    Stopped,
    PeerClosed,
    Overloaded,
    IoFailure,
    Panicked,
    Aborted,
    JoinFailure,
}

/// Terminal outcome of the outbound pump, summarized without retaining its
/// generic I/O error value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundTerminal {
    NotStarted,
    Stopped,
    ShutdownClose,
    PeerClosedReplyFlushed,
    QueueClosed,
    IoFailure,
    Panicked,
    Aborted,
    JoinFailure,
}

/// Bounded, typed evidence retained after every started pump task has joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodianSummary {
    connection_ref: ConnectionRef,
    /// Reader pump's independently observed terminal outcome.
    pub inbound: InboundTerminal,
    /// Writer pump's independently observed terminal outcome.
    pub outbound: OutboundTerminal,
}

impl CustodianSummary {
    /// Physical transport these pump outcomes belong to.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// Whether these terminal outcomes belong to the supplied transport.
    pub fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        &self.connection_ref == connection_ref
    }
}

/// Control handle for a custodian that owns a physical connection's two pumps.
///
/// The custodian runs independently of any Kameo actor and remains responsible
/// for both JoinHandles when a caller drops this handle. Dropping the last
/// handle requests stop; a canceled waiter does not cancel the custodian.
pub struct IoCustodianHandle {
    client: Arc<CustodianClient>,
}

struct CustodianClient {
    connection_ref: ConnectionRef,
    stop: StopSignal,
    force_stop: CancellationToken,
    admission_open: Arc<Mutex<bool>>,
    state: watch::Receiver<CustodianState>,
    summary: Arc<Mutex<Option<CustodianSummary>>>,
    cleanup_pending_published: Arc<AtomicBool>,
    startup_completion_finished: Arc<AtomicBool>,
}

/// Observation-only view of custodian state that does not keep control alive.
#[derive(Clone)]
pub struct IoCustodianObserver {
    connection_ref: ConnectionRef,
    state: watch::Receiver<CustodianState>,
    summary: Arc<Mutex<Option<CustodianSummary>>>,
    cleanup_pending_published: Arc<AtomicBool>,
    startup_completion_finished: Arc<AtomicBool>,
}

/// Refusal to retain an observation in the bounded physical-connection index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CustodianInventoryError {
    /// An inventory must be able to retain at least one custodian.
    #[error("custodian inventory capacity must be positive")]
    ZeroCapacity,
    /// This exact physical connection already has a retained custodian entry.
    #[error("custodian inventory already contains this connection")]
    DuplicateConnection,
    /// Every inventory slot is occupied by a custodian not yet joined.
    #[error("custodian inventory is full")]
    Full,
    /// Reservation bookkeeping exhausted its unique token space.
    #[error("custodian reservation identifiers are exhausted")]
    ReservationIdExhausted,
    /// The inventory no longer contains this reservation's exact token.
    #[error("custodian reservation is no longer active")]
    ReservationLost,
}

/// Startup failure after reservation when the independent reaper disappears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReservedStartError {
    /// The inventory refused or lost the exact reservation.
    #[error(transparent)]
    Inventory(#[from] CustodianInventoryError),
    /// The reaper disappeared before it acknowledged ownership readiness.
    #[error("reserved pump reaper disappeared during startup")]
    ReaperUnavailable,
}

enum InventoryEntry {
    Reserved {
        reservation_id: u64,
    },
    Active {
        reservation_id: Option<u64>,
        observer: IoCustodianObserver,
    },
}

/// Bounded, observation-only index of physical-connection custodians.
///
/// The index does not keep a control handle alive. It lets an independent
/// owner reacquire state after an actor loses all of its local handles, and it
/// keeps unfinished cleanup discoverable until terminal entries with no live
/// pump tasks are explicitly pruned.
#[derive(Clone)]
pub struct IoCustodianInventory {
    capacity: usize,
    entries: Arc<Mutex<HashMap<ConnectionRef, InventoryEntry>>>,
}

static NEXT_CUSTODIAN_RESERVATION_ID: AtomicU64 = AtomicU64::new(1);

/// One noncloneable admission to start the exact physical connection's pump
/// pair. Dropping an unused reservation returns its inventory slot.
pub struct IoCustodianReservation {
    inventory: IoCustodianInventory,
    connection_ref: ConnectionRef,
    reservation_id: u64,
    active: bool,
}

impl IoCustodianInventory {
    /// Create an inventory with a fixed maximum number of physical sockets.
    pub fn new(capacity: usize) -> Result<Self, CustodianInventoryError> {
        if capacity == 0 {
            return Err(CustodianInventoryError::ZeroCapacity);
        }
        Ok(Self {
            capacity,
            entries: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Retain observation for a custodian unless its exact connection is
    /// already indexed or the bounded inventory is full.
    pub fn try_retain(&self, custodian: &IoCustodianHandle) -> Result<(), CustodianInventoryError> {
        let connection_ref = custodian.connection_ref().clone();
        let mut entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        if entries.contains_key(&connection_ref) {
            return Err(CustodianInventoryError::DuplicateConnection);
        }
        if entries.len() >= self.capacity {
            return Err(CustodianInventoryError::Full);
        }
        entries.insert(
            connection_ref,
            InventoryEntry::Active {
                reservation_id: None,
                observer: custodian.observer(),
            },
        );
        Ok(())
    }

    /// Reserve capacity for this exact physical connection before invoking a
    /// pump factory, starting either pump, or spawning its independent reaper.
    pub fn reserve(
        &self,
        connection_ref: ConnectionRef,
    ) -> Result<IoCustodianReservation, CustodianInventoryError> {
        let reservation_id = NEXT_CUSTODIAN_RESERVATION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| CustodianInventoryError::ReservationIdExhausted)?;
        let mut entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        if entries.contains_key(&connection_ref) {
            return Err(CustodianInventoryError::DuplicateConnection);
        }
        if entries.len() >= self.capacity {
            return Err(CustodianInventoryError::Full);
        }
        entries.insert(
            connection_ref.clone(),
            InventoryEntry::Reserved { reservation_id },
        );
        Ok(IoCustodianReservation {
            inventory: self.clone(),
            connection_ref,
            reservation_id,
            active: true,
        })
    }

    /// Reacquire observation by exact physical-connection identity.
    pub fn observe(&self, connection_ref: &ConnectionRef) -> Option<IoCustodianObserver> {
        let entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        match entries.get(connection_ref) {
            Some(InventoryEntry::Active { observer, .. }) => Some(observer.clone()),
            Some(InventoryEntry::Reserved { .. }) | None => None,
        }
    }

    /// Number of retained observations, including cleanup that is still
    /// pending and Joined entries not yet explicitly pruned.
    pub fn len(&self) -> usize {
        let entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        entries.len()
    }

    /// Whether the index currently retains no observations.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove entries only after every started pump task has returned.
    /// CleanupPending and all other unfinished states remain discoverable.
    pub fn prune_joined(&self) -> usize {
        let mut entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        let before = entries.len();
        entries.retain(|_, entry| match entry {
            InventoryEntry::Reserved { .. } => true,
            InventoryEntry::Active { observer, .. } => !matches!(
                observer.state(),
                CustodianState::Joined
                    | CustodianState::StartupAbandoned
                    | CustodianState::PartialStartJoined
            ),
        });
        before - entries.len()
    }

    fn release_reservation(&self, connection_ref: &ConnectionRef, reservation_id: u64) {
        let mut entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        if matches!(
            entries.get(connection_ref),
            Some(InventoryEntry::Reserved {
                reservation_id: current
            }) if *current == reservation_id
        ) {
            entries.remove(connection_ref);
        }
    }

    fn activate_reservation(
        &self,
        connection_ref: &ConnectionRef,
        reservation_id: u64,
        observer: IoCustodianObserver,
    ) -> Result<(), CustodianInventoryError> {
        let mut entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        match entries.get(connection_ref) {
            Some(InventoryEntry::Reserved {
                reservation_id: current,
            }) if *current == reservation_id => {}
            _ => return Err(CustodianInventoryError::ReservationLost),
        }
        entries.insert(
            connection_ref.clone(),
            InventoryEntry::Active {
                reservation_id: Some(reservation_id),
                observer,
            },
        );
        Ok(())
    }

    fn release_active_without_pumps(&self, connection_ref: &ConnectionRef, reservation_id: u64) {
        let mut entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        if matches!(
            entries.get(connection_ref),
            Some(InventoryEntry::Active {
                reservation_id: Some(current),
                ..
            }) if *current == reservation_id
        ) {
            entries.remove(connection_ref);
        }
    }
}

impl IoCustodianReservation {
    /// Physical identity whose inventory slot this reservation holds.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// Whether this reservation belongs to the supplied exact connection.
    pub fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        &self.connection_ref == connection_ref
    }
}

impl Drop for IoCustodianReservation {
    fn drop(&mut self) {
        if self.active {
            self.inventory
                .release_reservation(&self.connection_ref, self.reservation_id);
        }
    }
}

impl IoCustodianReservation {
    /// Build the pump halves only after this reservation has won inventory
    /// capacity, then start both tasks under an independent reaper.
    pub async fn start_with<
        R,
        W,
        M,
        ReadError,
        WriteError,
        Size,
        IsClose,
        IsSendAfterClose,
        Factory,
        FactoryFuture,
    >(
        self,
        config: ReservedIoStartConfig<M, IsSendAfterClose, Size, IsClose>,
        factory: Factory,
    ) -> Result<
        (
            IoCustodianHandle,
            WriteSender<M>,
            mpsc::Receiver<QueueItem<M>>,
        ),
        ReservedStartError,
    >
    where
        R: Stream<Item = Result<M, ReadError>> + Unpin + Send + 'static,
        W: Sink<M, Error = WriteError> + Unpin + Send + 'static,
        M: Send + 'static,
        ReadError: Send + 'static,
        WriteError: Send + 'static,
        Size: Fn(&M) -> usize + Send + 'static,
        IsClose: Fn(&M) -> bool + Send + 'static,
        IsSendAfterClose: Fn(&WriteError) -> bool + Send + Sync + 'static,
        Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
        FactoryFuture: Future<Output = (R, W)> + Send,
    {
        self.start_with_hook(config, factory, |_| std::future::ready(()))
            .await
    }

    /// Start with deterministic asynchronous barriers for lifecycle tests.
    /// Canceling this future before the first pump starts releases its unused
    /// reservation; after either spawn, the independent reaper owns every
    /// started handle and keeps the inventory slot until all have joined.
    pub async fn start_with_hook<
        R,
        W,
        M,
        ReadError,
        WriteError,
        Size,
        IsClose,
        IsSendAfterClose,
        Factory,
        FactoryFuture,
        Hook,
        HookFuture,
    >(
        self,
        config: ReservedIoStartConfig<M, IsSendAfterClose, Size, IsClose>,
        factory: Factory,
        hook: Hook,
    ) -> Result<
        (
            IoCustodianHandle,
            WriteSender<M>,
            mpsc::Receiver<QueueItem<M>>,
        ),
        ReservedStartError,
    >
    where
        R: Stream<Item = Result<M, ReadError>> + Unpin + Send + 'static,
        W: Sink<M, Error = WriteError> + Unpin + Send + 'static,
        M: Send + 'static,
        ReadError: Send + 'static,
        WriteError: Send + 'static,
        Size: Fn(&M) -> usize + Send + 'static,
        IsClose: Fn(&M) -> bool + Send + 'static,
        IsSendAfterClose: Fn(&WriteError) -> bool + Send + Sync + 'static,
        Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
        FactoryFuture: Future<Output = (R, W)> + Send,
        Hook: FnMut(ReservedStartStage) -> HookFuture + Send,
        HookFuture: Future<Output = ()> + Send,
    {
        start_reserved_supervised_io(self, config, factory, hook).await
    }
}

impl IoCustodianHandle {
    /// Exact physical connection reference captured by the pump owner.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.client.connection_ref
    }

    /// Whether this custodian belongs to the supplied physical transport.
    pub fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        &self.client.connection_ref == connection_ref
    }

    /// Current lifecycle state. `Joined` is published only after both pump
    /// JoinHandles have returned.
    pub fn state(&self) -> CustodianState {
        *self.client.state.borrow()
    }

    /// First reason that stopped the pumps, when one has been recorded.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.client.stop.reason()
    }

    /// Terminal outcomes become available after every started pump task joins.
    /// `state()` distinguishes a full pair from an abandoned partial start.
    pub fn terminal_summary(&self) -> Option<CustodianSummary> {
        let summary = match self.client.summary.lock() {
            Ok(summary) => summary,
            Err(poisoned) => poisoned.into_inner(),
        };
        summary.clone()
    }

    /// Request an out-of-band stop without sending through an application data
    /// mailbox. Lease loss uses this to interrupt the pumps; it does not fence
    /// a write that has passed `poll_ready` and can still call `start_send`.
    /// Any live cutover needs an authority gate at that effect boundary.
    pub fn request_stop(&self, reason: StopReason) {
        if custodian_state_is_terminal(self.state()) {
            return;
        }
        let mut admission_open = match self.client.admission_open.lock() {
            Ok(admission_open) => admission_open,
            Err(poisoned) => poisoned.into_inner(),
        };
        *admission_open = false;
        drop(admission_open);
        self.client.stop.cancel(reason);
        self.client.force_stop.cancel();
    }

    /// Create an observation-only handle that can outlive this controller.
    pub fn observer(&self) -> IoCustodianObserver {
        IoCustodianObserver {
            connection_ref: self.client.connection_ref.clone(),
            state: self.client.state.clone(),
            summary: self.client.summary.clone(),
            cleanup_pending_published: self.client.cleanup_pending_published.clone(),
            startup_completion_finished: self.client.startup_completion_finished.clone(),
        }
    }

    /// Wait until every started pump task has returned. Canceling this future
    /// only cancels the waiter; the independent custodian keeps joining tasks.
    pub async fn wait_until_joined(&self) -> CustodianState {
        self.observer().wait_until_joined().await
    }
}

impl Drop for IoCustodianHandle {
    fn drop(&mut self) {
        if Arc::strong_count(&self.client) == 1 && !custodian_state_is_terminal(self.state()) {
            self.request_stop(StopReason::Requested);
        }
    }
}

impl IoCustodianObserver {
    /// Exact physical connection reference captured by the pump owner.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// Whether this observation belongs to the supplied physical transport.
    pub fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        &self.connection_ref == connection_ref
    }

    /// Current lifecycle state. `Joined` is published only after both pump
    /// JoinHandles have returned.
    pub fn state(&self) -> CustodianState {
        *self.state.borrow()
    }

    /// Whether this custodian ever published `CleanupPending`.
    pub fn cleanup_pending_was_published(&self) -> bool {
        self.cleanup_pending_published.load(Ordering::Acquire)
    }

    /// Whether reserved startup has relinquished any unstarted socket halves.
    /// Ordinary, already-started custodians report true because there is no
    /// reserved-start handoff left to complete.
    pub fn startup_completion_is_finished(&self) -> bool {
        self.startup_completion_finished.load(Ordering::Acquire)
    }

    /// Terminal outcomes become available after all started tasks have joined.
    /// The summary retains typed outcomes, not error payloads.
    pub fn terminal_summary(&self) -> Option<CustodianSummary> {
        let summary = match self.summary.lock() {
            Ok(summary) => summary,
            Err(poisoned) => poisoned.into_inner(),
        };
        summary.clone()
    }

    /// Wait until every started pump task has returned. If the custodian task
    /// exits unexpectedly, the last state is returned without claiming a
    /// successful join.
    pub async fn wait_until_joined(&self) -> CustodianState {
        let mut state = self.state.clone();
        loop {
            let current = *state.borrow_and_update();
            if custodian_state_is_terminal(current) {
                return current;
            }
            if state.changed().await.is_err() {
                return *state.borrow();
            }
        }
    }
}

fn custodian_state_is_terminal(state: CustodianState) -> bool {
    matches!(
        state,
        CustodianState::Joined
            | CustodianState::StartupAbandoned
            | CustodianState::PartialStartJoined
    )
}

/// Move both real pump handles into an independent lifecycle custodian.
///
/// The retirement deadline starts only after a stop request or a pump exit.
/// If a pump remains unfinished at that deadline, the state becomes
/// `CleanupPending`, the remaining pump is aborted, and the custodian keeps
/// waiting for both JoinHandles before it can publish `Joined`.
pub fn spawn_io_custodian<ReadError, WriteError>(
    supervisor: IoSupervisor<ReadError, WriteError>,
    retirement_deadline: Duration,
) -> IoCustodianHandle
where
    ReadError: Send + 'static,
    WriteError: Send + 'static,
{
    let connection_ref = supervisor.connection_ref.clone();
    let stop = supervisor.stop.clone();
    let force_stop = supervisor.force_stop.clone();
    let admission_open = supervisor.admission_open.clone();
    let (state_tx, state_rx) = watch::channel(CustodianState::Running);
    let summary = Arc::new(Mutex::new(None));
    let custodian_summary = summary.clone();
    let cleanup_pending_published = Arc::new(AtomicBool::new(false));
    let custodian_cleanup_pending_published = cleanup_pending_published.clone();
    let startup_completion_finished = Arc::new(AtomicBool::new(true));

    tokio::spawn(run_io_custodian(
        supervisor,
        retirement_deadline,
        state_tx,
        custodian_summary,
        custodian_cleanup_pending_published,
        None,
    ));

    IoCustodianHandle {
        client: Arc::new(CustodianClient {
            connection_ref,
            stop,
            force_stop,
            admission_open,
            state: state_rx,
            summary,
            cleanup_pending_published,
            startup_completion_finished,
        }),
    }
}

async fn run_io_custodian<ReadError, WriteError>(
    mut supervisor: IoSupervisor<ReadError, WriteError>,
    retirement_deadline: Duration,
    state_tx: watch::Sender<CustodianState>,
    custodian_summary: Arc<Mutex<Option<CustodianSummary>>>,
    cleanup_pending_published: Arc<AtomicBool>,
    startup_finished: Option<watch::Receiver<bool>>,
) where
    ReadError: Send + 'static,
    WriteError: Send + 'static,
{
    let mut inbound = None;
    let mut outbound = None;

    tokio::select! {
        result = supervisor.inbound.join() => inbound = Some(result),
        result = supervisor.outbound.join() => outbound = Some(result),
        _ = supervisor.stop.cancelled() => {}
    }

    state_tx.send_replace(CustodianState::Stopping);
    supervisor.stop.cancel(StopReason::PumpExited);

    let deadline = tokio::time::sleep(retirement_deadline);
    tokio::pin!(deadline);
    while inbound.is_none() || outbound.is_none() {
        tokio::select! {
            result = supervisor.inbound.join(), if inbound.is_none() => inbound = Some(result),
            result = supervisor.outbound.join(), if outbound.is_none() => outbound = Some(result),
            _ = &mut deadline => break,
        }
    }

    if inbound.is_none() || outbound.is_none() {
        cleanup_pending_published.store(true, Ordering::Release);
        state_tx.send_replace(CustodianState::CleanupPending);
        supervisor.stop.cancel(StopReason::ShutdownDeadline);
        supervisor.force_stop.cancel();
    }
    if inbound.is_none() {
        supervisor.inbound.abort();
    }
    if outbound.is_none() {
        supervisor.outbound.abort();
    }
    let inbound = match inbound {
        Some(result) => result,
        None => supervisor.inbound.join().await,
    };
    let outbound = match outbound {
        Some(result) => result,
        None => supervisor.outbound.join().await,
    };
    if let Some(mut startup_finished) = startup_finished {
        wait_for_startup_completion(&mut startup_finished).await;
    }
    let joined = supervisor.joined(inbound, outbound);
    let summary = CustodianSummary {
        connection_ref: joined.connection_ref.clone(),
        inbound: summarize_inbound(&joined.inbound),
        outbound: summarize_outbound(&joined.outbound),
    };
    let mut retained_summary = match custodian_summary.lock() {
        Ok(retained_summary) => retained_summary,
        Err(poisoned) => poisoned.into_inner(),
    };
    *retained_summary = Some(summary);
    drop(retained_summary);
    state_tx.send_replace(CustodianState::Joined);
}

struct ReservedReaperSetup<ReadError, WriteError> {
    connection_ref: ConnectionRef,
    stop: StopSignal,
    force_stop: CancellationToken,
    shutdown: CancellationToken,
    admission_open: Arc<Mutex<bool>>,
    retirement_deadline: Duration,
    inbound_owner_rx: oneshot::Receiver<JoinHandle<InboundExit<ReadError>>>,
    outbound_owner_rx: oneshot::Receiver<JoinHandle<OutboundExit<WriteError>>>,
    commit_rx: oneshot::Receiver<()>,
    startup_finished: watch::Receiver<bool>,
    ready_tx: oneshot::Sender<()>,
    inbound_start_tx: oneshot::Sender<()>,
    outbound_start_tx: oneshot::Sender<()>,
    state_tx: watch::Sender<CustodianState>,
    summary: Arc<Mutex<Option<CustodianSummary>>>,
    inventory: IoCustodianInventory,
    reservation_id: u64,
    exit_point: Option<ReservedReaperExitPoint>,
    cleanup_pending_published: Arc<AtomicBool>,
}

/// Completes startup only after the prepared reader and writer futures have
/// dropped or moved into their independently owned pump tasks. This is the
/// final `PreparedIo` field so its signal follows any unstarted halves' Drop.
struct StartupCompletionSignal {
    sender: Option<watch::Sender<bool>>,
    finished: Arc<AtomicBool>,
}

impl StartupCompletionSignal {
    fn new(completed: watch::Sender<bool>, finished: Arc<AtomicBool>) -> Self {
        Self {
            sender: Some(completed),
            finished,
        }
    }
}

impl Drop for StartupCompletionSignal {
    fn drop(&mut self) {
        if let Some(completed) = self.sender.take() {
            // A waiter can publish terminal state as soon as the watch changes.
            self.finished.store(true, Ordering::Release);
            completed.send_replace(true);
        }
    }
}

async fn wait_for_startup_completion(startup_finished: &mut watch::Receiver<bool>) {
    loop {
        if *startup_finished.borrow_and_update() {
            return;
        }
        if startup_finished.changed().await.is_err() {
            return;
        }
    }
}

struct ReaperMonitorSetup {
    reaper: JoinHandle<()>,
    startup_finished: watch::Receiver<bool>,
    connection_ref: ConnectionRef,
    state: watch::Receiver<CustodianState>,
    state_tx: watch::Sender<CustodianState>,
    summary: Arc<Mutex<Option<CustodianSummary>>>,
    inventory: IoCustodianInventory,
    reservation_id: u64,
    started_pump_count: Arc<AtomicU8>,
    cleanup_pending_published: Arc<AtomicBool>,
}

/// Reconcile an unexpectedly exited reaper after the starter can no longer
/// spawn pumps. A zero-pump start is abandoned; otherwise cleanup stays pending
/// because this monitor has no join proof for every started handle.
async fn monitor_reserved_reaper(setup: ReaperMonitorSetup) {
    let ReaperMonitorSetup {
        reaper,
        startup_finished,
        connection_ref,
        state,
        state_tx,
        summary,
        inventory,
        reservation_id,
        started_pump_count,
        cleanup_pending_published,
    } = setup;
    let _ = reaper.await;
    let mut startup_finished = startup_finished;
    wait_for_startup_completion(&mut startup_finished).await;

    let current_state = *state.borrow();
    if custodian_state_is_terminal(current_state) || current_state == CustodianState::CleanupPending
    {
        return;
    }

    if started_pump_count.load(Ordering::Acquire) == 0 {
        let mut retained_summary = match summary.lock() {
            Ok(retained_summary) => retained_summary,
            Err(poisoned) => poisoned.into_inner(),
        };
        *retained_summary = Some(CustodianSummary {
            connection_ref: connection_ref.clone(),
            inbound: InboundTerminal::NotStarted,
            outbound: OutboundTerminal::NotStarted,
        });
        drop(retained_summary);
        // Terminal state means this exact no-pump reservation is already gone.
        inventory.release_active_without_pumps(&connection_ref, reservation_id);
        state_tx.send_replace(CustodianState::StartupAbandoned);
    } else {
        cleanup_pending_published.store(true, Ordering::Release);
        state_tx.send_replace(CustodianState::CleanupPending);
    }
}

async fn run_reserved_start<ReadError, WriteError>(
    setup: ReservedReaperSetup<ReadError, WriteError>,
) where
    ReadError: Send + 'static,
    WriteError: Send + 'static,
{
    let ReservedReaperSetup {
        connection_ref,
        stop,
        force_stop,
        shutdown,
        admission_open,
        retirement_deadline,
        inbound_owner_rx,
        outbound_owner_rx,
        commit_rx,
        mut startup_finished,
        ready_tx,
        inbound_start_tx,
        outbound_start_tx,
        state_tx,
        summary,
        inventory,
        reservation_id,
        exit_point,
        cleanup_pending_published,
    } = setup;
    if exit_point == Some(ReservedReaperExitPoint::BeforeCommit) {
        return;
    }
    let (inbound_handle, outbound_handle, committed) =
        tokio::join!(inbound_owner_rx, outbound_owner_rx, commit_rx);
    let mut inbound = inbound_handle.ok().map(TaskOwner::new);
    let mut outbound = outbound_handle.ok().map(TaskOwner::new);

    if committed.is_ok() {
        match (inbound.take(), outbound.take()) {
            (Some(inbound_owner), Some(outbound_owner)) => {
                if exit_point == Some(ReservedReaperExitPoint::BeforeReady) {
                    return;
                }
                let _ = inbound_start_tx.send(());
                let _ = outbound_start_tx.send(());
                state_tx.send_replace(CustodianState::Running);
                let _ = ready_tx.send(());
                return run_io_custodian(
                    IoSupervisor {
                        connection_ref,
                        stop,
                        force_stop,
                        shutdown,
                        admission_open,
                        inbound: inbound_owner,
                        outbound: outbound_owner,
                    },
                    retirement_deadline,
                    state_tx,
                    summary,
                    cleanup_pending_published,
                    Some(startup_finished),
                )
                .await;
            }
            (inbound_owner, outbound_owner) => {
                inbound = inbound_owner;
                outbound = outbound_owner;
            }
        }
    }

    state_tx.send_replace(CustodianState::Stopping);
    stop.cancel(StopReason::Requested);
    force_stop.cancel();
    if let Some(inbound) = inbound.as_ref() {
        inbound.abort();
    }
    if let Some(outbound) = outbound.as_ref() {
        outbound.abort();
    }
    let mut inbound_result = None;
    let mut outbound_result = None;
    let deadline = tokio::time::sleep(retirement_deadline);
    tokio::pin!(deadline);
    while (inbound.is_some() && inbound_result.is_none())
        || (outbound.is_some() && outbound_result.is_none())
    {
        tokio::select! {
            result = join_owned(&mut inbound),
                if inbound.is_some() && inbound_result.is_none() => inbound_result = Some(result),
            result = join_owned(&mut outbound),
                if outbound.is_some() && outbound_result.is_none() => outbound_result = Some(result),
            _ = &mut deadline => break,
        }
    }
    let inbound_pending = inbound.is_some() && inbound_result.is_none();
    let outbound_pending = outbound.is_some() && outbound_result.is_none();
    if inbound_pending || outbound_pending {
        cleanup_pending_published.store(true, Ordering::Release);
        state_tx.send_replace(CustodianState::CleanupPending);
        stop.cancel(StopReason::ShutdownDeadline);
        force_stop.cancel();
    }
    if inbound_result.is_none() {
        if let Some(owner) = inbound.as_mut() {
            inbound_result = Some(owner.join().await);
        }
    }
    if outbound_result.is_none() {
        if let Some(owner) = outbound.as_mut() {
            outbound_result = Some(owner.join().await);
        }
    }
    wait_for_startup_completion(&mut startup_finished).await;
    let inbound_terminal = inbound_result
        .as_ref()
        .map(summarize_inbound)
        .unwrap_or(InboundTerminal::NotStarted);
    let outbound_terminal = outbound_result
        .as_ref()
        .map(summarize_outbound)
        .unwrap_or(OutboundTerminal::NotStarted);
    let terminal_summary = CustodianSummary {
        connection_ref: connection_ref.clone(),
        inbound: inbound_terminal,
        outbound: outbound_terminal,
    };
    let mut retained_summary = match summary.lock() {
        Ok(retained_summary) => retained_summary,
        Err(poisoned) => poisoned.into_inner(),
    };
    *retained_summary = Some(terminal_summary);
    drop(retained_summary);

    match (inbound_result.is_some(), outbound_result.is_some()) {
        (true, true) => {
            state_tx.send_replace(CustodianState::Joined);
        }
        (false, false) => {
            inventory.release_active_without_pumps(&connection_ref, reservation_id);
            state_tx.send_replace(CustodianState::StartupAbandoned);
        }
        _ => {
            state_tx.send_replace(CustodianState::PartialStartJoined);
        }
    };
}

fn summarize_inbound<E>(result: &Result<InboundExit<E>, JoinError>) -> InboundTerminal {
    match result {
        Ok(InboundExit::Stopped) => InboundTerminal::Stopped,
        Ok(InboundExit::PeerClosed) => InboundTerminal::PeerClosed,
        Ok(InboundExit::Overloaded) => InboundTerminal::Overloaded,
        Ok(InboundExit::IoFailure(_)) => InboundTerminal::IoFailure,
        Err(error) if error.is_panic() => InboundTerminal::Panicked,
        Err(error) if error.is_cancelled() => InboundTerminal::Aborted,
        Err(_) => InboundTerminal::JoinFailure,
    }
}

fn summarize_outbound<E>(result: &Result<OutboundExit<E>, JoinError>) -> OutboundTerminal {
    match result {
        Ok(OutboundExit::Stopped) => OutboundTerminal::Stopped,
        Ok(OutboundExit::ShutdownClose) => OutboundTerminal::ShutdownClose,
        Ok(OutboundExit::PeerClosedReplyFlushed) => OutboundTerminal::PeerClosedReplyFlushed,
        Ok(OutboundExit::QueueClosed) => OutboundTerminal::QueueClosed,
        Ok(OutboundExit::IoFailure(_)) => OutboundTerminal::IoFailure,
        Err(error) if error.is_panic() => OutboundTerminal::Panicked,
        Err(error) if error.is_cancelled() => OutboundTerminal::Aborted,
        Err(_) => OutboundTerminal::JoinFailure,
    }
}

impl<ReadError, WriteError> IoSupervisor<ReadError, WriteError> {
    /// Exact physical connection reference captured at pump creation.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// First stop reason, when either pump requested cancellation.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.stop.reason()
    }

    /// Whether the shared transport stop signal is set.
    pub fn is_stopped(&self) -> bool {
        self.stop.is_cancelled()
    }

    /// Wait for the shared transport stop signal.
    pub async fn stopped(&self) {
        self.stop.cancelled().await;
    }

    /// Begin graceful shutdown. This seals outbound admission, wakes the writer,
    /// and leaves the reader active for the peer Close handshake. Inbound
    /// application frames are discarded after this point. Admitted but
    /// abandoned requests report `NotAttempted`.
    pub fn begin_graceful_shutdown(&self) {
        let mut admission_open = match self.admission_open.lock() {
            Ok(admission_open) => admission_open,
            Err(poisoned) => poisoned.into_inner(),
        };
        *admission_open = false;
        drop(admission_open);
        self.shutdown.cancel();
    }

    /// Whether the inbound pump task has finished.
    pub fn inbound_finished(&self) -> bool {
        self.inbound.is_finished()
    }

    /// Whether the outbound pump task has finished.
    pub fn outbound_finished(&self) -> bool {
        self.outbound.is_finished()
    }

    /// Abort the inbound task. Available only because this whole module is
    /// gated behind `test-support`.
    pub fn abort_inbound_for_test(&self) {
        self.inbound.abort();
    }

    /// Abort the outbound task. Available only because this whole module is
    /// gated behind `test-support`.
    pub fn abort_outbound_for_test(&self) {
        self.outbound.abort();
    }

    /// Stop both pumps and join them, preserving each task's independent result.
    pub async fn retire(mut self, mode: RetirementMode) -> Joined<ReadError, WriteError> {
        match mode {
            RetirementMode::Graceful => self.begin_graceful_shutdown(),
            RetirementMode::Forced => {
                self.force_stop.cancel();
                self.stop.cancel(StopReason::Requested);
            }
        }
        let (inbound, outbound) = tokio::join!(self.inbound.join(), self.outbound.join());
        self.joined(inbound, outbound)
    }

    /// Ask the writer to send its close frame, then force-abort and join any
    /// pump that exceeds the supplied deadline.
    pub async fn shutdown_with_deadline(
        mut self,
        deadline: Duration,
    ) -> Joined<ReadError, WriteError> {
        self.begin_graceful_shutdown();
        let deadline = tokio::time::sleep(deadline);
        tokio::pin!(deadline);
        let mut inbound = None;
        let mut outbound = None;

        while inbound.is_none() || outbound.is_none() {
            tokio::select! {
                result = self.inbound.join(), if inbound.is_none() => inbound = Some(result),
                result = self.outbound.join(), if outbound.is_none() => outbound = Some(result),
                _ = &mut deadline => break,
            }
        }

        if inbound.is_none() || outbound.is_none() {
            self.force_stop.cancel();
            self.stop.cancel(StopReason::ShutdownDeadline);
        }
        let inbound = match inbound {
            Some(result) => result,
            None => self.inbound.abort_and_join().await,
        };
        let outbound = match outbound {
            Some(result) => result,
            None => self.outbound.abort_and_join().await,
        };
        self.joined(inbound, outbound)
    }

    /// Observe one pump's termination, stop its peer, and join both within the
    /// supplied bound. Peer-close reply flushing is allowed to complete until
    /// the deadline, after which the remaining task is aborted and joined.
    pub async fn retire_after_failure(
        mut self,
        deadline: Duration,
    ) -> Joined<ReadError, WriteError> {
        let deadline = tokio::time::sleep(deadline);
        tokio::pin!(deadline);
        let mut inbound = None;
        let mut outbound = None;

        while inbound.is_none() || outbound.is_none() {
            tokio::select! {
                result = self.inbound.join(), if inbound.is_none() => {
                    inbound = Some(result);
                    self.stop.cancel(StopReason::PumpExited);
                }
                result = self.outbound.join(), if outbound.is_none() => {
                    outbound = Some(result);
                    self.stop.cancel(StopReason::PumpExited);
                }
                _ = &mut deadline => break,
            }
        }

        if inbound.is_none() || outbound.is_none() {
            self.force_stop.cancel();
            self.stop.cancel(StopReason::ShutdownDeadline);
        }
        let inbound = match inbound {
            Some(result) => result,
            None => self.inbound.abort_and_join().await,
        };
        let outbound = match outbound {
            Some(result) => result,
            None => self.outbound.abort_and_join().await,
        };
        self.joined(inbound, outbound)
    }

    fn joined(
        &self,
        inbound: Result<InboundExit<ReadError>, JoinError>,
        outbound: Result<OutboundExit<WriteError>, JoinError>,
    ) -> Joined<ReadError, WriteError> {
        Joined {
            connection_ref: self.connection_ref.clone(),
            inbound,
            outbound,
        }
    }
}

struct TaskOwner<T> {
    handle: Option<JoinHandle<T>>,
}

impl<T> TaskOwner<T> {
    fn new(handle: JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .map(JoinHandle::is_finished)
            .unwrap_or(true)
    }

    fn abort(&self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }

    async fn join(&mut self) -> Result<T, JoinError> {
        let result = match self.handle.as_mut() {
            Some(handle) => handle.await,
            // The select loops disable a task's branch after it returns once.
            // If a join future loses a select, this handle remains stored so
            // the next join or abort path still owns it.
            None => return std::future::pending().await,
        };
        self.handle = None;
        result
    }

    async fn abort_and_join(&mut self) -> Result<T, JoinError> {
        self.abort();
        self.join().await
    }
}

async fn join_owned<T>(owner: &mut Option<TaskOwner<T>>) -> Result<T, JoinError> {
    match owner.as_mut() {
        Some(owner) => owner.join().await,
        None => std::future::pending().await,
    }
}

impl<T> Drop for TaskOwner<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

struct PumpHandoff<T: Send + 'static> {
    handle: Option<JoinHandle<T>>,
    owner: Option<oneshot::Sender<JoinHandle<T>>>,
}

impl<T: Send + 'static> PumpHandoff<T> {
    fn new(handle: JoinHandle<T>, owner: oneshot::Sender<JoinHandle<T>>) -> Self {
        Self {
            handle: Some(handle),
            owner: Some(owner),
        }
    }

    fn transfer(mut self) {
        self.send_to_owner();
    }

    fn send_to_owner(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let Some(owner) = self.owner.take() else {
            abort_and_reap(handle);
            return;
        };
        if let Err(error) = owner.send(handle) {
            abort_and_reap(error);
        }
    }
}

impl<T: Send + 'static> Drop for PumpHandoff<T> {
    fn drop(&mut self) {
        self.send_to_owner();
    }
}

fn abort_and_reap<T: Send + 'static>(handle: JoinHandle<T>) {
    // If the independent reaper has already exited, retain the inventory slot
    // in its last nonterminal state. This fallback joins the task best-effort
    // but cannot publish custodian join proof; external cleanup may be needed.
    handle.abort();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = handle.await;
        });
    }
}

type PumpFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

struct PreparedIo<M, ReadError, WriteError> {
    connection_ref: ConnectionRef,
    stop: StopSignal,
    force_stop: CancellationToken,
    shutdown: CancellationToken,
    admission_open: Arc<Mutex<bool>>,
    inbound: PumpFuture<InboundExit<ReadError>>,
    outbound: PumpFuture<OutboundExit<WriteError>>,
    outbound_sender: WriteSender<M>,
    inbound_receiver: mpsc::Receiver<QueueItem<M>>,
    startup_completion: Option<StartupCompletionSignal>,
}

fn prepare_supervised_io<R, W, M, ReadError, WriteError, Size, IsClose, IsSendAfterClose>(
    connection_ref: ConnectionRef,
    reader: R,
    writer: W,
    limits: TransportLimits,
    shutdown_protocol: ShutdownProtocol<M, IsSendAfterClose>,
    frame_size: Size,
    is_close: IsClose,
) -> PreparedIo<M, ReadError, WriteError>
where
    R: Stream<Item = Result<M, ReadError>> + Unpin + Send + 'static,
    W: Sink<M, Error = WriteError> + Unpin + Send + 'static,
    M: Send + 'static,
    ReadError: Send + 'static,
    WriteError: Send + 'static,
    Size: Fn(&M) -> usize + Send + 'static,
    IsClose: Fn(&M) -> bool + Send + 'static,
    IsSendAfterClose: Fn(&WriteError) -> bool + Send + Sync + 'static,
{
    let ShutdownProtocol {
        shutdown_frame,
        is_send_after_close,
    } = shutdown_protocol;
    let stop = StopSignal::new();
    let force_stop = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let peer_close = CancellationToken::new();
    let admission_open = Arc::new(Mutex::new(true));
    let (inbound, inbound_receiver) = bounded_channel(
        limits.inbound,
        stop.clone(),
        StopReason::InboundOverload,
        StopReason::InboundQueueClosed,
    );
    let (outbound_queue, outbound_receiver) = bounded_channel(
        limits.outbound,
        stop.clone(),
        StopReason::OutboundOverload,
        StopReason::OutboundQueueClosed,
    );

    let inbound = Box::pin(run_reader(
        reader,
        inbound,
        stop.clone(),
        shutdown.clone(),
        peer_close.clone(),
        frame_size,
        is_close,
    ));
    let outbound = Box::pin(run_writer(
        writer,
        outbound_receiver,
        WriterSignals {
            stop: stop.clone(),
            force_stop: force_stop.clone(),
            shutdown: shutdown.clone(),
            peer_close,
        },
        Some(shutdown_frame),
        is_send_after_close,
    ));

    PreparedIo {
        connection_ref,
        stop,
        force_stop,
        shutdown,
        admission_open: admission_open.clone(),
        inbound,
        outbound,
        outbound_sender: WriteSender {
            sender: outbound_queue,
            admission_open,
        },
        inbound_receiver,
        startup_completion: None,
    }
}

async fn start_reserved_supervised_io<
    R,
    W,
    M,
    ReadError,
    WriteError,
    Size,
    IsClose,
    IsSendAfterClose,
    Factory,
    FactoryFuture,
    Hook,
    HookFuture,
>(
    mut reservation: IoCustodianReservation,
    config: ReservedIoStartConfig<M, IsSendAfterClose, Size, IsClose>,
    factory: Factory,
    mut hook: Hook,
) -> Result<
    (
        IoCustodianHandle,
        WriteSender<M>,
        mpsc::Receiver<QueueItem<M>>,
    ),
    ReservedStartError,
>
where
    R: Stream<Item = Result<M, ReadError>> + Unpin + Send + 'static,
    W: Sink<M, Error = WriteError> + Unpin + Send + 'static,
    M: Send + 'static,
    ReadError: Send + 'static,
    WriteError: Send + 'static,
    Size: Fn(&M) -> usize + Send + 'static,
    IsClose: Fn(&M) -> bool + Send + 'static,
    IsSendAfterClose: Fn(&WriteError) -> bool + Send + Sync + 'static,
    Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
    FactoryFuture: Future<Output = (R, W)> + Send,
    Hook: FnMut(ReservedStartStage) -> HookFuture + Send,
    HookFuture: Future<Output = ()> + Send,
{
    let ReservedIoStartConfig {
        limits,
        shutdown_protocol,
        frame_size,
        is_close,
        retirement_deadline,
        reaper_exit_point,
    } = config;
    let connection_ref = reservation.connection_ref.clone();
    let (reader, writer) = factory(&connection_ref).await;
    let mut prepared = prepare_supervised_io(
        connection_ref.clone(),
        reader,
        writer,
        limits,
        shutdown_protocol,
        frame_size,
        is_close,
    );

    let (state_tx, state_rx) = watch::channel(CustodianState::Starting);
    let summary = Arc::new(Mutex::new(None));
    let cleanup_pending_published = Arc::new(AtomicBool::new(false));
    let startup_completion_finished = Arc::new(AtomicBool::new(false));
    let handle_summary = summary.clone();
    let observer = IoCustodianObserver {
        connection_ref: connection_ref.clone(),
        state: state_rx.clone(),
        summary: summary.clone(),
        cleanup_pending_published: cleanup_pending_published.clone(),
        startup_completion_finished: startup_completion_finished.clone(),
    };
    let inventory = reservation.inventory.clone();
    let reservation_id = reservation.reservation_id;

    let (inbound_owner_tx, inbound_owner_rx) = oneshot::channel();
    let (outbound_owner_tx, outbound_owner_rx) = oneshot::channel();
    let (commit_tx, commit_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (inbound_start_tx, inbound_start_rx) = oneshot::channel();
    let (outbound_start_tx, outbound_start_rx) = oneshot::channel();
    let (startup_finished_tx, startup_finished_rx) = watch::channel(false);
    prepared.startup_completion = Some(StartupCompletionSignal::new(
        startup_finished_tx,
        startup_completion_finished.clone(),
    ));
    let started_pump_count = Arc::new(AtomicU8::new(0));

    // The reservation remains armed until the reaper exists and has the
    // channels needed to recover every later startup cancellation or panic.
    let failure_state_tx = state_tx.clone();
    let failure_cleanup_pending_published = cleanup_pending_published.clone();
    let monitor_state_tx = state_tx.clone();
    let monitor_state = state_rx.clone();
    let monitor_summary = summary.clone();
    let monitor_inventory = inventory.clone();
    let monitor_started_pump_count = started_pump_count.clone();
    let monitor_cleanup_pending_published = cleanup_pending_published.clone();
    let reaper = tokio::spawn(run_reserved_start(ReservedReaperSetup {
        connection_ref: connection_ref.clone(),
        stop: prepared.stop.clone(),
        force_stop: prepared.force_stop.clone(),
        shutdown: prepared.shutdown.clone(),
        admission_open: prepared.admission_open.clone(),
        retirement_deadline,
        inbound_owner_rx,
        outbound_owner_rx,
        commit_rx,
        startup_finished: startup_finished_rx.clone(),
        ready_tx,
        inbound_start_tx,
        outbound_start_tx,
        state_tx,
        summary,
        inventory,
        reservation_id,
        exit_point: reaper_exit_point,
        cleanup_pending_published: cleanup_pending_published.clone(),
    }));
    tokio::spawn(monitor_reserved_reaper(ReaperMonitorSetup {
        reaper,
        startup_finished: startup_finished_rx,
        connection_ref: connection_ref.clone(),
        state: monitor_state,
        state_tx: monitor_state_tx,
        summary: monitor_summary,
        inventory: monitor_inventory,
        reservation_id,
        started_pump_count: monitor_started_pump_count,
        cleanup_pending_published: monitor_cleanup_pending_published,
    }));

    reservation
        .inventory
        .activate_reservation(&connection_ref, reservation_id, observer)?;
    reservation.active = false;

    let handle = IoCustodianHandle {
        client: Arc::new(CustodianClient {
            connection_ref,
            stop: prepared.stop,
            force_stop: prepared.force_stop,
            admission_open: prepared.admission_open,
            state: state_rx,
            summary: handle_summary,
            cleanup_pending_published,
            startup_completion_finished,
        }),
    };

    hook(ReservedStartStage::BeforeInboundSpawn).await;
    let inbound_task = tokio::spawn(async move {
        if inbound_start_rx.await.is_err() {
            return InboundExit::Stopped;
        }
        prepared.inbound.await
    });
    started_pump_count.fetch_add(1, Ordering::Release);
    PumpHandoff::new(inbound_task, inbound_owner_tx).transfer();

    hook(ReservedStartStage::BetweenPumpSpawns).await;
    let outbound_task = tokio::spawn(async move {
        if outbound_start_rx.await.is_err() {
            return OutboundExit::Stopped;
        }
        prepared.outbound.await
    });
    started_pump_count.fetch_add(1, Ordering::Release);
    PumpHandoff::new(outbound_task, outbound_owner_tx).transfer();

    hook(ReservedStartStage::AfterOutboundSpawn).await;
    if commit_tx.send(()).is_err() {
        failure_cleanup_pending_published.store(true, Ordering::Release);
        failure_state_tx.send_replace(CustodianState::CleanupPending);
        return Err(ReservedStartError::ReaperUnavailable);
    }
    if ready_rx.await.is_err() {
        failure_cleanup_pending_published.store(true, Ordering::Release);
        failure_state_tx.send_replace(CustodianState::CleanupPending);
        return Err(ReservedStartError::ReaperUnavailable);
    }
    hook(ReservedStartStage::AfterCommit).await;

    Ok((handle, prepared.outbound_sender, prepared.inbound_receiver))
}

/// Split socket halves into supervised read and write pumps.
pub fn spawn_supervised_io<R, W, M, ReadError, WriteError, Size, IsClose, IsSendAfterClose>(
    connection_ref: ConnectionRef,
    reader: R,
    writer: W,
    limits: TransportLimits,
    shutdown_protocol: ShutdownProtocol<M, IsSendAfterClose>,
    frame_size: Size,
    is_close: IsClose,
) -> (
    IoSupervisor<ReadError, WriteError>,
    WriteSender<M>,
    mpsc::Receiver<QueueItem<M>>,
)
where
    R: Stream<Item = Result<M, ReadError>> + Unpin + Send + 'static,
    W: Sink<M, Error = WriteError> + Unpin + Send + 'static,
    M: Send + 'static,
    ReadError: Send + 'static,
    WriteError: Send + 'static,
    Size: Fn(&M) -> usize + Send + 'static,
    IsClose: Fn(&M) -> bool + Send + 'static,
    IsSendAfterClose: Fn(&WriteError) -> bool + Send + Sync + 'static,
{
    let prepared = prepare_supervised_io(
        connection_ref,
        reader,
        writer,
        limits,
        shutdown_protocol,
        frame_size,
        is_close,
    );
    let inbound_task = tokio::spawn(prepared.inbound);
    let outbound_task = tokio::spawn(prepared.outbound);

    (
        IoSupervisor {
            connection_ref: prepared.connection_ref,
            stop: prepared.stop,
            force_stop: prepared.force_stop,
            shutdown: prepared.shutdown,
            admission_open: prepared.admission_open,
            inbound: TaskOwner::new(inbound_task),
            outbound: TaskOwner::new(outbound_task),
        },
        prepared.outbound_sender,
        prepared.inbound_receiver,
    )
}

fn bounded_channel<T>(
    limits: Limits,
    stop: StopSignal,
    overload_reason: StopReason,
    closed_reason: StopReason,
) -> (BoundedSender<T>, mpsc::Receiver<QueueItem<T>>) {
    let (sender, receiver) = mpsc::channel(limits.max_count);
    (
        BoundedSender::new(sender, limits, stop, overload_reason, closed_reason),
        receiver,
    )
}

async fn run_reader<R, M, E, Size, IsClose>(
    mut reader: R,
    inbound: BoundedSender<M>,
    stop: StopSignal,
    shutdown: CancellationToken,
    peer_close: CancellationToken,
    frame_size: Size,
    is_close: IsClose,
) -> InboundExit<E>
where
    R: Stream<Item = Result<M, E>> + Unpin,
    Size: Fn(&M) -> usize,
    IsClose: Fn(&M) -> bool,
{
    let mut closing = false;
    loop {
        let frame = tokio::select! {
            biased;
            _ = stop.cancelled() => return InboundExit::Stopped,
            _ = shutdown.cancelled(), if !closing => {
                closing = true;
                continue;
            }
            frame = reader.next() => frame,
        };
        match frame {
            Some(Ok(frame)) if is_close(&frame) => {
                peer_close.cancel();
                stop.cancel(StopReason::PeerClosed);
                return InboundExit::PeerClosed;
            }
            Some(Ok(_frame)) if closing => {}
            Some(Ok(frame)) => {
                let byte_len = frame_size(&frame);
                match inbound.try_send(frame, byte_len) {
                    Ok(()) => {}
                    Err(AdmissionError::Overloaded) => return InboundExit::Overloaded,
                    Err(
                        AdmissionError::Closed | AdmissionError::Stopped | AdmissionError::Shutdown,
                    ) => {
                        return InboundExit::Stopped;
                    }
                }
            }
            Some(Err(error)) => {
                stop.cancel(StopReason::InboundIoFailure);
                return InboundExit::IoFailure(error);
            }
            None => {
                stop.cancel(StopReason::PeerClosed);
                return InboundExit::PeerClosed;
            }
        }
    }
}

struct WriterSignals {
    stop: StopSignal,
    force_stop: CancellationToken,
    shutdown: CancellationToken,
    peer_close: CancellationToken,
}

async fn run_writer<W, M, E, IsSendAfterClose>(
    mut writer: W,
    mut outbound: mpsc::Receiver<QueueItem<WriteRequest<M>>>,
    signals: WriterSignals,
    mut shutdown_frame: Option<M>,
    is_send_after_close: IsSendAfterClose,
) -> OutboundExit<E>
where
    W: Sink<M, Error = E> + Unpin,
    IsSendAfterClose: Fn(&E) -> bool,
{
    let WriterSignals {
        stop,
        force_stop,
        shutdown,
        peer_close,
    } = signals;
    loop {
        let work = tokio::select! {
            biased;
            _ = force_stop.cancelled() => return OutboundExit::Stopped,
            _ = peer_close.cancelled() => {
                return flush_peer_close_reply(
                    &mut writer,
                    &force_stop,
                    &is_send_after_close,
                )
                .await;
            }
            _ = stop.cancelled() => return OutboundExit::Stopped,
            _ = shutdown.cancelled() => {
                let Some(frame) = shutdown_frame.take() else {
                    return OutboundExit::Stopped;
                };
                return send_shutdown_frame(
                    &mut writer,
                    frame,
                    &stop,
                    &force_stop,
                    &peer_close,
                    &is_send_after_close,
                )
                .await;
            }
            work = outbound.recv() => match work {
                Some(work) => work,
                None => {
                    stop.cancel(StopReason::OutboundQueueClosed);
                    return OutboundExit::QueueClosed;
                }
            }
        };

        match send_queued(
            &mut writer,
            work,
            &stop,
            &force_stop,
            &shutdown,
            &peer_close,
            &is_send_after_close,
        )
        .await
        {
            SendOutcome::Sent => {}
            SendOutcome::Stopped => return OutboundExit::Stopped,
            SendOutcome::PeerClosed => {
                return flush_peer_close_reply(&mut writer, &force_stop, &is_send_after_close)
                    .await;
            }
            SendOutcome::Shutdown => {
                let Some(frame) = shutdown_frame.take() else {
                    return OutboundExit::Stopped;
                };
                return send_shutdown_frame(
                    &mut writer,
                    frame,
                    &stop,
                    &force_stop,
                    &peer_close,
                    &is_send_after_close,
                )
                .await;
            }
            SendOutcome::IoFailure(error) => {
                stop.cancel(StopReason::OutboundIoFailure);
                return OutboundExit::IoFailure(error);
            }
        }
    }
}

enum SendOutcome<E> {
    Sent,
    Stopped,
    PeerClosed,
    Shutdown,
    IoFailure(E),
}

async fn send_queued<W, M, E>(
    writer: &mut W,
    work: QueueItem<WriteRequest<M>>,
    stop: &StopSignal,
    force_stop: &CancellationToken,
    shutdown: &CancellationToken,
    peer_close: &CancellationToken,
    is_send_after_close: &impl Fn(&E) -> bool,
) -> SendOutcome<E>
where
    W: Sink<M, Error = E> + Unpin,
{
    let mut request = work.value;
    let mut progress = InFlightWrite::new(request.receipt.clone());
    let ready = tokio::select! {
        biased;
        _ = force_stop.cancelled() => return SendOutcome::Stopped,
        _ = peer_close.cancelled() => return SendOutcome::PeerClosed,
        _ = stop.cancelled() => return SendOutcome::Stopped,
        _ = shutdown.cancelled() => return SendOutcome::Shutdown,
        ready = poll_fn(|context| Pin::new(&mut *writer).poll_ready(context)) => ready,
    };
    if let Err(error) = ready {
        if is_send_after_close(&error) {
            return wait_for_peer_close_or_force(force_stop, peer_close).await;
        }
        return SendOutcome::IoFailure(error);
    }

    let value = request
        .value
        .take()
        .expect("write request retains its value until first attempt");
    progress.begin_attempt();
    if let Err(error) = Pin::new(&mut *writer).start_send(value) {
        if is_send_after_close(&error) {
            progress.not_attempted();
            return wait_for_peer_close_or_force(force_stop, peer_close).await;
        }
        progress.uncertain();
        return SendOutcome::IoFailure(error);
    }
    progress.attempted();

    let flushed = tokio::select! {
        biased;
        _ = force_stop.cancelled() => return SendOutcome::Stopped,
        _ = peer_close.cancelled() => return SendOutcome::PeerClosed,
        _ = stop.cancelled() => return SendOutcome::Stopped,
        _ = shutdown.cancelled() => return SendOutcome::Shutdown,
        flushed = poll_fn(|context| Pin::new(&mut *writer).poll_flush(context)) => flushed,
    };
    match flushed {
        Ok(()) => {
            progress.completed();
            SendOutcome::Sent
        }
        Err(error) if is_send_after_close(&error) => {
            wait_for_peer_close_or_force(force_stop, peer_close).await
        }
        Err(error) => SendOutcome::IoFailure(error),
    }
}

async fn wait_for_peer_close_or_force<E>(
    force_stop: &CancellationToken,
    peer_close: &CancellationToken,
) -> SendOutcome<E> {
    tokio::select! {
        biased;
        _ = force_stop.cancelled() => SendOutcome::Stopped,
        _ = peer_close.cancelled() => SendOutcome::PeerClosed,
    }
}

async fn flush_peer_close_reply<W, M, E>(
    writer: &mut W,
    force_stop: &CancellationToken,
    is_send_after_close: &impl Fn(&E) -> bool,
) -> OutboundExit<E>
where
    W: Sink<M, Error = E> + Unpin,
{
    let mut result = tokio::select! {
        biased;
        _ = force_stop.cancelled() => return OutboundExit::Stopped,
        result = writer.flush() => result,
    };

    // SplitSink consumes its buffered frame before forwarding it. If Tungstenite
    // rejects it because the peer already closed, retry once to flush the
    // automatic reply that the reader queued. Preserve any retry error.
    if result.as_ref().is_err_and(is_send_after_close) {
        result = tokio::select! {
            biased;
            _ = force_stop.cancelled() => return OutboundExit::Stopped,
            result = writer.flush() => result,
        };
    }

    match result {
        Ok(()) => OutboundExit::PeerClosedReplyFlushed,
        Err(error) => OutboundExit::IoFailure(error),
    }
}

async fn send_shutdown_frame<W, M, E, IsSendAfterClose>(
    writer: &mut W,
    shutdown_frame: M,
    stop: &StopSignal,
    force_stop: &CancellationToken,
    peer_close: &CancellationToken,
    is_send_after_close: &IsSendAfterClose,
) -> OutboundExit<E>
where
    W: Sink<M, Error = E> + Unpin,
    IsSendAfterClose: Fn(&E) -> bool,
{
    let result = tokio::select! {
        biased;
        _ = force_stop.cancelled() => return OutboundExit::Stopped,
        _ = peer_close.cancelled() => {
            return flush_peer_close_reply(writer, force_stop, is_send_after_close).await;
        }
        _ = stop.cancelled() => return OutboundExit::Stopped,
        result = writer.send(shutdown_frame) => result,
    };
    match result {
        Ok(()) => OutboundExit::ShutdownClose,
        Err(error) if is_send_after_close(&error) => {
            tokio::select! {
                biased;
                _ = force_stop.cancelled() => OutboundExit::Stopped,
                _ = peer_close.cancelled() => {
                    flush_peer_close_reply(writer, force_stop, is_send_after_close).await
                }
            }
        }
        Err(error) => {
            stop.cancel(StopReason::OutboundIoFailure);
            OutboundExit::IoFailure(error)
        }
    }
}

struct InFlightWrite {
    receipt: WriteReceipt,
    attempted: bool,
    completed: bool,
}

impl InFlightWrite {
    fn new(receipt: WriteReceipt) -> Self {
        Self {
            receipt,
            attempted: false,
            completed: false,
        }
    }

    fn begin_attempt(&mut self) {
        // Keep the externally visible receipt at Admitted until `start_send`
        // returns, but classify a panic during that synchronous call as
        // uncertain when this guard drops.
        self.attempted = true;
    }

    fn attempted(&mut self) {
        self.attempted = true;
        self.receipt.0.store(WRITE_ATTEMPTED, Ordering::Release);
    }

    fn uncertain(&mut self) {
        self.attempted = true;
        self.receipt.0.store(WRITE_UNCERTAIN, Ordering::Release);
    }

    fn completed(&mut self) {
        self.completed = true;
        self.receipt.0.store(WRITE_COMPLETED, Ordering::Release);
    }

    fn not_attempted(&mut self) {
        self.attempted = false;
        self.receipt.0.store(WRITE_NOT_ATTEMPTED, Ordering::Release);
    }
}

impl Drop for InFlightWrite {
    fn drop(&mut self) {
        if self.attempted && !self.completed {
            self.receipt.0.store(WRITE_UNCERTAIN, Ordering::Release);
        }
    }
}
