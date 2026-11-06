//! Bounded WebSocket I/O pumps and their independently observed owner.
//!
//! This module is an isolated transport boundary for #2046. The production
//! connection loop does not instantiate it yet. It carries the physical
//! connection reference through task retirement, bounds queued items by count
//! and bytes, and reports local write progress without making an XMPP or
//! durable-custody claim.

mod relinquishment;

pub use relinquishment::SplitIoRelinquishment;

use futures::{Sink, SinkExt, Stream, StreamExt};
use relinquishment::track_halves;
use std::collections::HashMap;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
#[cfg(feature = "test-support")]
use std::sync::MutexGuard;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::ConnectionRef;

const WRITE_ADMITTED: u8 = 0;
const WRITE_ATTEMPTED: u8 = 1;
const WRITE_COMPLETED: u8 = 2;
const WRITE_UNCERTAIN: u8 = 3;
const WRITE_NOT_ATTEMPTED: u8 = 4;

pub(crate) enum FixedTerminalText {
    SystemShutdown,
    Conflict,
    FramingClose,
}

/// Reuse the active connection path's exact terminal serialization.
pub(crate) fn fixed_terminal_text(frame: FixedTerminalText) -> axum::extract::ws::Message {
    let xml = match frame {
        FixedTerminalText::SystemShutdown => {
            super::transport_xml::build_system_shutdown_stream_error()
        }
        FixedTerminalText::Conflict => super::transport_xml::build_conflict_stream_error(),
        FixedTerminalText::FramingClose => super::transport_xml::websocket_stream_close_xml(),
    };
    axum::extract::ws::Message::Text(xml.into())
}

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

    /// Maximum number of simultaneously admitted queue items.
    pub fn max_count(self) -> usize {
        self.max_count
    }

    /// Maximum admitted payload bytes.
    pub fn max_bytes(self) -> usize {
        self.max_bytes
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

    /// Inbound item and byte bounds.
    pub fn inbound(self) -> Limits {
        self.inbound
    }

    /// Outbound item and byte bounds.
    pub fn outbound(self) -> Limits {
        self.outbound
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
    /// Interrupts the pumps after local lease loss. The supervised WebSocket
    /// data and live-detach paths also fence their synchronous sink enqueue.
    LeaseLost,
}

/// Shared, first-wins cancellation signal with a typed reason.
#[derive(Clone)]
pub struct StopSignal {
    token: CancellationToken,
    state: Arc<Mutex<StopSignalState>>,
}

struct StopSignalState {
    reason: Option<StopReason>,
    listener: Option<Arc<dyn Fn(StopReason) + Send + Sync>>,
    retirement_started: bool,
    retirement_reason: Option<StopReason>,
    retirement_listener: Option<Arc<dyn Fn(StopReason) + Send + Sync>>,
}

impl StopSignal {
    fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            state: Arc::new(Mutex::new(StopSignalState {
                reason: None,
                listener: None,
                retirement_started: false,
                retirement_reason: None,
                retirement_listener: None,
            })),
        }
    }

    /// Request pump cancellation. The first reason remains authoritative.
    pub fn cancel(&self, reason: StopReason) {
        self.cancel_with_peer_close(reason, None);
    }

    fn cancel_with_peer_close(&self, reason: StopReason, peer_close: Option<&CancellationToken>) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.reason.is_none() {
            Self::begin_retirement_locked(&mut state, reason);
            if let Some(listener) = &state.listener {
                // The listener is a synchronous lifecycle authority. It runs
                // before cancellation becomes observable so protocol
                // completion and every stop source share one linearization
                // point. Listeners must not call back into this StopSignal.
                listener(reason);
            }
            state.reason = Some(reason);
        }
        if let Some(peer_close) = peer_close {
            // Publish the first stop reason before the peer-close signal wakes
            // the writer and reaper, but wake that writer before the stop token
            // so it can flush the automatic WebSocket close reply.
            peer_close.cancel();
        }
        drop(state);
        self.token.cancel();
    }

    fn begin_retirement(&self, reason: StopReason) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        Self::begin_retirement_locked(&mut state, reason);
    }

    fn begin_retirement_locked(state: &mut StopSignalState, reason: StopReason) {
        if state.retirement_started {
            return;
        }
        state.retirement_started = true;
        state.retirement_reason = Some(reason);
        if let Some(listener) = &state.retirement_listener {
            // This callback is the synchronous owner-side retirement fence.
            // It runs before the pump stop or graceful-shutdown signal becomes
            // observable and must not call back into this StopSignal.
            listener(reason);
        }
    }

    /// Run the synchronous sink enqueue only if no stop has won the state
    /// lock. A concurrent stop waits until the enqueue returns. Callers may
    /// check transport authority inside the closure, but must not await,
    /// cancel or re-enter this signal, or acquire actor/effect-owner locks.
    pub(crate) fn start_send_if_running<R>(&self, start_send: impl FnOnce() -> R) -> Option<R> {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.reason.is_some() {
            return None;
        }
        let result = start_send();
        drop(state);
        Some(result)
    }

    #[cfg(feature = "test-support")]
    fn set_listener(&self, listener: Arc<dyn Fn(StopReason) + Send + Sync>) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.listener = Some(Arc::clone(&listener));
        if let Some(reason) = state.reason {
            listener(reason);
        }
    }

    fn set_retirement_listener(&self, listener: Arc<dyn Fn(StopReason) + Send + Sync>) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.retirement_listener = Some(Arc::clone(&listener));
        if state.retirement_started {
            listener(state.retirement_reason.unwrap_or(StopReason::Requested));
        }
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
        match self.state.lock() {
            Ok(state) => state.reason,
            Err(poisoned) => poisoned.into_inner().reason,
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

    /// Transform a retained item without releasing its count or byte permits.
    #[cfg(feature = "test-support")]
    pub(crate) fn map<U>(self, transform: impl FnOnce(T) -> U) -> QueueItem<U> {
        QueueItem {
            value: transform(self.value),
            _count_permit: self._count_permit,
            _byte_permit: self._byte_permit,
        }
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
    max_bytes: usize,
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
            max_bytes: limits.max_bytes,
            stop,
            overload_reason,
            closed_reason,
        }
    }

    async fn acquire_capacity(
        &self,
        byte_len: usize,
    ) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), AdmissionError> {
        if byte_len > self.max_bytes || u32::try_from(byte_len).is_err() {
            return Err(AdmissionError::Overloaded);
        }
        let count = tokio::select! {
            biased;
            _ = self.stop.cancelled() => return Err(AdmissionError::Stopped),
            _ = self.sender.closed() => return Err(AdmissionError::Closed),
            permit = self.count.clone().acquire_owned() => permit.map_err(|_| AdmissionError::Closed)?,
        };
        let bytes = tokio::select! {
            biased;
            _ = self.stop.cancelled() => return Err(AdmissionError::Stopped),
            _ = self.sender.closed() => return Err(AdmissionError::Closed),
            permit = self.bytes.clone().acquire_many_owned(byte_len as u32) => permit.map_err(|_| AdmissionError::Closed)?,
        };
        Ok((count, bytes))
    }

    fn send_with_capacity(
        &self,
        value: T,
        (count_permit, byte_permit): (OwnedSemaphorePermit, OwnedSemaphorePermit),
    ) -> Result<(), AdmissionError> {
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
    }

    async fn send_wait(&self, value: T, byte_len: usize) -> Result<(), AdmissionError> {
        let capacity = self.acquire_capacity(byte_len).await?;
        self.send_with_capacity(value, capacity)
    }

    /// Try to admit an item without waiting for queue capacity.
    pub fn try_send(&self, value: T, byte_len: usize) -> Result<(), AdmissionError> {
        let result = self.try_send_without_notification(value, byte_len);
        self.notify_failed_admission(result)
    }

    // Queue operations under an authority gate must not synchronously invoke
    // the stop listener, which can need that same gate. Return the result for
    // notification after the caller has dropped its guards.
    fn try_send_without_notification(
        &self,
        value: T,
        byte_len: usize,
    ) -> Result<(), AdmissionError> {
        if self.stop.is_cancelled() {
            return Err(AdmissionError::Stopped);
        }
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
    }

    fn notify_failed_admission<R>(
        &self,
        result: Result<R, AdmissionError>,
    ) -> Result<R, AdmissionError> {
        match result {
            Ok(value) => Ok(value),
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

impl<T> Clone for BoundedSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            count: Arc::clone(&self.count),
            bytes: Arc::clone(&self.bytes),
            max_bytes: self.max_bytes,
            stop: self.stop.clone(),
            overload_reason: self.overload_reason,
            closed_reason: self.closed_reason,
        }
    }
}

/// Locally observed progress for an admitted write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteProgress {
    Admitted,
    /// The sink accepted `start_send`; it may only have buffered the frame.
    Attempted,
    /// The local sink flush completed; the peer may not have handled the frame.
    Completed,
    /// `start_send` began, but a successful local flush was not observed. A
    /// failing or panicking call may have produced effects before returning.
    Uncertain,
    /// The frame was abandoned before `start_send`.
    NotAttempted,
}

/// Receipt for one outbound item. Completion means the local sink flush
/// completed; it does not mean the peer processed or acknowledged the frame.
/// A later exact transport rejection does not downgrade an already published
/// `Attempted` milestone; it remains `Uncertain` because the receipt does not
/// distinguish a sink-buffered frame from one that reached the transport.
/// The attempted milestone is not evidence of an OS write or a peer ACK.
#[derive(Debug, Clone)]
pub struct WriteReceipt(Arc<ReceiptState>);

#[derive(Debug)]
struct ReceiptState {
    progress: AtomicU8,
    changed: Notify,
}

impl WriteReceipt {
    fn new() -> Self {
        Self(Arc::new(ReceiptState {
            progress: AtomicU8::new(WRITE_ADMITTED),
            changed: Notify::new(),
        }))
    }

    /// Current local write milestone.
    pub fn progress(&self) -> WriteProgress {
        match self.0.progress.load(Ordering::Acquire) {
            WRITE_ADMITTED => WriteProgress::Admitted,
            WRITE_ATTEMPTED => WriteProgress::Attempted,
            WRITE_COMPLETED => WriteProgress::Completed,
            WRITE_NOT_ATTEMPTED => WriteProgress::NotAttempted,
            _ => WriteProgress::Uncertain,
        }
    }

    fn set(&self, state: u8) {
        self.0.progress.store(state, Ordering::Release);
        self.0.changed.notify_waiters();
    }

    fn not_attempted_if_admitted(&self) {
        if self
            .0
            .progress
            .compare_exchange(
                WRITE_ADMITTED,
                WRITE_NOT_ATTEMPTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.0.changed.notify_waiters();
        }
    }

    /// Wait until the sink has been attempted or the frame has already reached
    /// a terminal outcome. This observes the local sink boundary, not peer delivery.
    pub async fn wait_for_attempt(&self) -> WriteProgress {
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let progress = self.progress();
            if progress != WriteProgress::Admitted {
                return progress;
            }
            notified.await;
        }
    }

    /// Wait for this frame's final local outcome. A completed flush is not a peer acknowledgement.
    pub async fn wait_for_terminal(&self) -> WriteProgress {
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let progress = self.progress();
            if matches!(
                progress,
                WriteProgress::Completed | WriteProgress::Uncertain | WriteProgress::NotAttempted
            ) {
                return progress;
            }
            notified.await;
        }
    }
}

struct WriteRequest<T> {
    value: Option<T>,
    receipt: WriteReceipt,
    authority: Option<Arc<dyn WriteAuthority>>,
}

/// A per-request fence for the actual synchronous sink enqueue. Implementors
/// may invoke `attempt` only while their authority holds.
/// The generic supervised-I/O test harness does not install this fence; the
/// WebSocket adapter supplies one for every submitted frame.
pub(crate) trait WriteAuthority: Send + Sync {
    fn revoked(&self) -> futures::future::BoxFuture<'_, ()>;
    fn attempt(
        &self,
        connection_ref: &ConnectionRef,
        stop: &StopSignal,
        attempt: Box<dyn FnOnce() + '_>,
    ) -> bool;
    fn completed(&self);
    #[cfg(feature = "test-support")]
    fn after_attempt(&self) -> futures::future::BoxFuture<'_, ()>;
}

impl<T> Drop for WriteRequest<T> {
    fn drop(&mut self) {
        if self.value.is_some() {
            self.receipt.not_attempted_if_admitted();
        }
    }
}

/// Bounded outbound queue whose sends return their local progress receipt.
pub struct WriteSender<T> {
    sender: BoundedSender<WriteRequest<T>>,
    admission_open: Arc<Mutex<bool>>,
    shutdown: CancellationToken,
}

/// Held only across the synchronous queue admission, never notification.
#[cfg(feature = "test-support")]
pub(crate) struct WriteAdmissionGuard<'a, T> {
    sender: &'a WriteSender<T>,
    _open: MutexGuard<'a, bool>,
}

#[cfg(feature = "test-support")]
impl<T> WriteAdmissionGuard<'_, T> {
    pub(crate) fn try_send_without_notification(
        &self,
        value: T,
        byte_len: usize,
    ) -> Result<WriteReceipt, AdmissionError> {
        let receipt = WriteReceipt::new();
        self.sender.sender.try_send_without_notification(
            WriteRequest {
                value: Some(value),
                receipt: receipt.clone(),
                authority: None,
            },
            byte_len,
        )?;
        Ok(receipt)
    }
}

impl<T> WriteSender<T> {
    /// Wait for capacity without treating ordinary saturation as a transport failure.
    #[cfg(feature = "test-support")]
    pub async fn send_wait(
        &self,
        value: T,
        byte_len: usize,
    ) -> Result<WriteReceipt, AdmissionError> {
        self.send_wait_with_authority(value, byte_len, None).await
    }

    pub(crate) async fn send_wait_fenced(
        &self,
        value: T,
        byte_len: usize,
        authority: Arc<dyn WriteAuthority>,
    ) -> Result<WriteReceipt, AdmissionError> {
        self.send_wait_with_authority(value, byte_len, Some(authority))
            .await
    }

    async fn send_wait_with_authority(
        &self,
        value: T,
        byte_len: usize,
        authority: Option<Arc<dyn WriteAuthority>>,
    ) -> Result<WriteReceipt, AdmissionError> {
        let capacity = tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => return Err(AdmissionError::Shutdown),
            capacity = self.sender.acquire_capacity(byte_len) => capacity?,
        };
        let admission_open = match self.admission_open.lock() {
            Ok(admission_open) => admission_open,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !*admission_open {
            return Err(AdmissionError::Shutdown);
        }
        let receipt = WriteReceipt::new();
        self.sender.send_with_capacity(
            WriteRequest {
                value: Some(value),
                receipt: receipt.clone(),
                authority,
            },
            capacity,
        )?;
        Ok(receipt)
    }

    /// Admit one outbound value under the queue's count and byte bounds.
    #[cfg(feature = "test-support")]
    pub fn try_send(&self, value: T, byte_len: usize) -> Result<WriteReceipt, AdmissionError> {
        let result = {
            let admission = self.lock_admission()?;
            admission.try_send_without_notification(value, byte_len)
        };
        self.notify_failed_admission(result)
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn lock_admission(&self) -> Result<WriteAdmissionGuard<'_, T>, AdmissionError> {
        let admission_open = match self.admission_open.lock() {
            Ok(admission_open) => admission_open,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !*admission_open {
            return Err(AdmissionError::Shutdown);
        }
        Ok(WriteAdmissionGuard {
            sender: self,
            _open: admission_open,
        })
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn notify_failed_admission<R>(
        &self,
        result: Result<R, AdmissionError>,
    ) -> Result<R, AdmissionError> {
        self.sender.notify_failed_admission(result)
    }
}

impl<T> Clone for WriteSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            admission_open: Arc::clone(&self.admission_open),
            shutdown: self.shutdown.clone(),
        }
    }
}

/// An ordered terminal input held separately from the bounded data queue.
pub(crate) enum OrderedInboundTerminal<M, E> {
    Close(M),
    IoFailure(E),
    Oversized { size: usize, max_bytes: usize },
}

/// How a pump ended.
#[derive(Debug, PartialEq, Eq)]
pub enum InboundExit<E> {
    Stopped,
    PeerClosed,
    Overloaded,
    IoFailure(E),
    /// The exact I/O error was transferred to the ordered inbound stream.
    IoFailureReported,
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
#[cfg(feature = "test-support")]
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
    #[cfg(feature = "test-support")]
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// Whether this late completion belongs to the supplied physical socket.
    #[cfg(feature = "test-support")]
    pub fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        &self.connection_ref == connection_ref
    }
}

/// Owns the two split halves' pump tasks and their retirement signals.
pub struct IoSupervisor<ReadError, WriteError> {
    connection_ref: ConnectionRef,
    #[cfg(feature = "test-support")]
    relinquishment: watch::Receiver<bool>,
    stop: StopSignal,
    force_stop: CancellationToken,
    shutdown: CancellationToken,
    #[cfg(feature = "test-support")]
    skip_shutdown_frame: Arc<AtomicBool>,
    #[cfg(feature = "test-support")]
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
    /// Reserved actor fixtures also require their auxiliary cleanup owner to
    /// finish before this state is published.
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

    /// Inbound item and byte bounds configured for the reader pump.
    #[cfg(feature = "test-support")]
    pub fn inbound_limits(&self) -> Limits {
        self.limits.inbound()
    }

    /// Inject reaper loss at a startup boundary for lifecycle tests.
    #[cfg(feature = "test-support")]
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
#[derive(Clone)]
pub struct IoCustodianHandle {
    client: Arc<CustodianClient>,
}

/// Stops the exact custodian if its startup caller disappears before handing
/// control back. The watcher is joined by the startup owner on normal return.
pub(crate) struct CustodianCancellationWatch {
    client: Arc<CustodianClient>,
    task: Option<JoinHandle<()>>,
    settled: bool,
}

impl CustodianCancellationWatch {
    pub(crate) async fn finish(mut self) {
        self.settled = true;
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for CustodianCancellationWatch {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        IoCustodianHandle {
            client: Arc::clone(&self.client),
        }
        .request_stop(StopReason::Requested);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct CustodianClient {
    connection_ref: ConnectionRef,
    relinquishment: watch::Receiver<bool>,
    stop: StopSignal,
    force_stop: CancellationToken,
    shutdown: CancellationToken,
    skip_shutdown_frame: Arc<AtomicBool>,
    admission_open: Arc<Mutex<bool>>,
    state: watch::Receiver<CustodianState>,
    summary: Arc<Mutex<Option<CustodianSummary>>>,
    cleanup_pending_published: Arc<AtomicBool>,
    startup_completion_finished: Arc<AtomicBool>,
    auxiliary_required: bool,
    auxiliary_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    auxiliary_finished: Arc<AtomicBool>,
    auxiliary_registered: Arc<AtomicBool>,
    auxiliary_join_started: Arc<AtomicBool>,
    auxiliary_task_registered: Arc<Notify>,
    #[cfg(feature = "test-support")]
    physical_stop: Arc<PhysicalStopHandoff>,
}

// A dark physical driver may request retirement without acquiring the fence
// held by the writer's synchronous start_send. The reaper owns the receiving
// side and keeps this request through worker cancellation and retirement.
#[cfg(feature = "test-support")]
#[derive(Default)]
struct PhysicalStopHandoff {
    state: Mutex<PhysicalStopState>,
    changed: Notify,
}

#[cfg(feature = "test-support")]
#[derive(Default)]
struct PhysicalStopState {
    enabled: bool,
    retiring: bool,
    closed: bool,
    pending: Option<StopReason>,
}

#[cfg(feature = "test-support")]
impl PhysicalStopHandoff {
    fn lock(&self) -> std::sync::MutexGuard<'_, PhysicalStopState> {
        self.state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }

    fn enable(&self) -> bool {
        let mut state = self.lock();
        if state.enabled || state.retiring || state.closed {
            return false;
        }
        state.enabled = true;
        true
    }

    fn request(&self, reason: StopReason) -> bool {
        let mut state = self.lock();
        if !state.enabled || state.closed {
            return false;
        }
        if state.pending.is_none() {
            state.pending = Some(reason);
        }
        drop(state);
        self.changed.notify_one();
        true
    }

    fn begin_retirement(&self) -> bool {
        let mut state = self.lock();
        state.retiring = true;
        state.enabled
    }

    fn take_request(&self) -> Option<StopReason> {
        self.lock().pending.take()
    }

    fn close_if_empty(&self) -> bool {
        let mut state = self.lock();
        if state.pending.is_some() {
            return false;
        }
        state.closed = true;
        true
    }
}

// The sender must not report a live receiver if the reaper is lost.
#[cfg(feature = "test-support")]
struct PhysicalStopReceiver(Arc<PhysicalStopHandoff>);

#[cfg(feature = "test-support")]
impl Drop for PhysicalStopReceiver {
    fn drop(&mut self) {
        self.0.lock().closed = true;
    }
}

/// Observation-only view of custodian state that does not keep control alive.
#[derive(Clone)]
pub struct IoCustodianObserver {
    connection_ref: ConnectionRef,
    relinquishment: watch::Receiver<bool>,
    stop: StopSignal,
    state: watch::Receiver<CustodianState>,
    summary: Arc<Mutex<Option<CustodianSummary>>>,
    cleanup_pending_published: Arc<AtomicBool>,
    startup_completion_finished: Arc<AtomicBool>,
    auxiliary_required: bool,
    auxiliary_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    auxiliary_finished: Arc<AtomicBool>,
    auxiliary_registered: Arc<AtomicBool>,
    auxiliary_join_started: Arc<AtomicBool>,
    auxiliary_task_registered: Arc<Notify>,
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
    /// This custodian already has an auxiliary cleanup task registered.
    #[error("custodian auxiliary cleanup task is already registered")]
    AuxiliaryTaskAlreadyRegistered,
    /// This custodian has no retained auxiliary cleanup hold.
    #[error("custodian does not require an auxiliary cleanup task")]
    AuxiliaryTaskNotRequired,
    /// This reservation already has an auxiliary cleanup hold.
    #[error("custodian reservation already has an auxiliary cleanup hold")]
    AuxiliaryHoldAlreadyRetained,
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
    /// Cancellation arrived before any pump or reaper was activated.
    #[error("reserved startup was canceled before either pump started")]
    CancelledBeforePumps,
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
/// keeps unfinished cleanup discoverable. When admission reaches capacity, it
/// can reclaim one fully joined entry to make room for a new connection.
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
    auxiliary_cleanup: Option<watch::Receiver<bool>>,
    auxiliary_required: bool,
    auxiliary_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    auxiliary_finished: Arc<AtomicBool>,
    auxiliary_registered: Arc<AtomicBool>,
    auxiliary_join_started: Arc<AtomicBool>,
    auxiliary_task_registered: Arc<Notify>,
    active: bool,
}

/// Completes an exact connection's auxiliary cleanup hold after non-pump
/// owners, such as a test-only actor and effect manager, have been joined.
/// Dropping this guard without completing it deliberately keeps the indexed
/// connection in nonterminal cleanup.
pub struct AuxiliaryCleanupGuard {
    joined: watch::Sender<bool>,
}

impl AuxiliaryCleanupGuard {
    /// Publish auxiliary cleanup only after every task and retained payload
    /// owned by the caller has been joined or dropped.
    pub fn complete(self) {
        self.joined.send_replace(true);
    }
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
        if entries.len() >= self.capacity && !reclaim_one_joined(&mut entries) {
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
        if entries.len() >= self.capacity && !reclaim_one_joined(&mut entries) {
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
            auxiliary_cleanup: None,
            auxiliary_required: false,
            auxiliary_task: Arc::new(Mutex::new(None)),
            auxiliary_finished: Arc::new(AtomicBool::new(true)),
            auxiliary_registered: Arc::new(AtomicBool::new(false)),
            auxiliary_join_started: Arc::new(AtomicBool::new(false)),
            auxiliary_task_registered: Arc::new(Notify::new()),
            active: true,
        })
    }

    /// Test-only snapshot of reserved identities, including legacy capacity holds.
    #[cfg(feature = "test-support")]
    pub fn reserved_connection_refs(&self) -> Vec<ConnectionRef> {
        let entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        entries
            .iter()
            .filter(|(_, entry)| matches!(entry, InventoryEntry::Reserved { .. }))
            .map(|(connection_ref, _)| connection_ref.clone())
            .collect()
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

    /// Register the independent task that proves an actor-side cleanup hold
    /// for this exact physical connection. The task handle remains indexed
    /// until it has finished and the pump custodian is terminal.
    pub fn register_auxiliary_task(
        &self,
        connection_ref: &ConnectionRef,
        task: JoinHandle<()>,
    ) -> Result<(), CustodianInventoryError> {
        let entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(InventoryEntry::Active { observer, .. }) = entries.get(connection_ref) else {
            task.abort();
            return Err(CustodianInventoryError::ReservationLost);
        };
        if !observer.auxiliary_required {
            task.abort();
            return Err(CustodianInventoryError::AuxiliaryTaskNotRequired);
        }
        if observer.auxiliary_registered.load(Ordering::Acquire) {
            task.abort();
            return Err(CustodianInventoryError::AuxiliaryTaskAlreadyRegistered);
        }
        let mut retained = match observer.auxiliary_task.lock() {
            Ok(retained) => retained,
            Err(poisoned) => poisoned.into_inner(),
        };
        if retained.is_some() {
            task.abort();
            return Err(CustodianInventoryError::AuxiliaryTaskAlreadyRegistered);
        }
        observer.auxiliary_registered.store(true, Ordering::Release);
        *retained = Some(task);
        observer.auxiliary_task_registered.notify_one();
        Ok(())
    }

    /// Number of retained observations, including pending cleanup and joined
    /// entries that have not yet been reclaimed or explicitly pruned.
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
            InventoryEntry::Active { observer, .. } => {
                let terminal = matches!(
                    observer.state(),
                    CustodianState::Joined
                        | CustodianState::StartupAbandoned
                        | CustodianState::PartialStartJoined
                );
                let auxiliary_finished = observer.auxiliary_finished.load(Ordering::Acquire);
                !(terminal && auxiliary_finished)
            }
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

/// Reclaim at most one terminal entry while holding the inventory lock. The
/// scan is bounded by capacity, and the same lock protects the subsequent
/// insertion, so no saved identity can remove a later reservation.
fn reclaim_one_joined(entries: &mut HashMap<ConnectionRef, InventoryEntry>) -> bool {
    let candidate = entries.iter().find_map(|(connection_ref, entry)| {
        let InventoryEntry::Active { observer, .. } = entry else {
            return None;
        };
        let joined = matches!(
            observer.state(),
            CustodianState::Joined | CustodianState::PartialStartJoined
        );
        (joined && observer.auxiliary_finished.load(Ordering::Acquire))
            .then(|| connection_ref.clone())
    });
    candidate.is_some_and(|connection_ref| entries.remove(&connection_ref).is_some())
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

    /// Keep this exact inventory entry nonterminal until auxiliary connection
    /// owners have supplied their own cleanup proof.
    pub fn retain_until_auxiliary_cleanup(
        &mut self,
    ) -> Result<AuxiliaryCleanupGuard, CustodianInventoryError> {
        if self.auxiliary_required {
            return Err(CustodianInventoryError::AuxiliaryHoldAlreadyRetained);
        }
        let (joined, observer) = watch::channel(false);
        self.auxiliary_cleanup = Some(observer);
        self.auxiliary_required = true;
        self.auxiliary_finished = Arc::new(AtomicBool::new(false));
        self.auxiliary_registered = Arc::new(AtomicBool::new(false));
        self.auxiliary_join_started = Arc::new(AtomicBool::new(false));
        self.auxiliary_task_registered = Arc::new(Notify::new());
        Ok(AuxiliaryCleanupGuard { joined })
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
    #[cfg(feature = "test-support")]
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
    #[cfg(feature = "test-support")]
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
        start_reserved_supervised_io(self, config, factory, hook, None, None).await
    }

    /// Same as `start_with_hook`, while a caller-owned cancellation signal
    /// stops this exact connection if the outer starter is abandoned.
    #[cfg(feature = "test-support")]
    pub async fn start_with_hook_and_cancellation<
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
        cancellation: CancellationToken,
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
        start_reserved_supervised_io(self, config, factory, hook, Some(cancellation), None).await
    }
}

impl IoCustodianHandle {
    /// Give the owner its bounded terminal-output window after revocation,
    /// then stop without retaining a controlling handle during that window.
    pub(crate) fn watch_transport_cancellation(
        &self,
        cancellation: CancellationToken,
        retirement_deadline: Duration,
    ) -> JoinHandle<()> {
        let client = Arc::downgrade(&self.client);
        let observer = self.observer();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    tokio::select! {
                        _ = observer.wait_until_joined() => return,
                        _ = tokio::time::sleep(retirement_deadline) => {}
                    }
                    if let Some(client) = client.upgrade() {
                        IoCustodianHandle { client }.request_stop(StopReason::Requested);
                    }
                }
                _ = observer.wait_until_joined() => {}
            }
        })
    }

    /// Exact physical connection reference captured by the pump owner.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.client.connection_ref
    }

    /// Whether this custodian belongs to the supplied physical transport.
    pub fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        &self.client.connection_ref == connection_ref
    }

    /// Clone the exact physical connection's first-wins stop signal for
    /// independently owned cleanup tasks.
    pub(crate) fn stop_signal(&self) -> StopSignal {
        self.client.stop.clone()
    }

    pub(crate) fn watch_cancellation(
        &self,
        cancellation: CancellationToken,
    ) -> CustodianCancellationWatch {
        let client = Arc::clone(&self.client);
        let watcher_client = Arc::clone(&client);
        let task = tokio::spawn(async move {
            cancellation.cancelled().await;
            IoCustodianHandle {
                client: watcher_client,
            }
            .request_stop(StopReason::Requested);
        });
        CustodianCancellationWatch {
            client,
            task: Some(task),
            settled: false,
        }
    }

    /// Current lifecycle state. `Joined` includes any required auxiliary
    /// cleanup owner in addition to both pump JoinHandles.
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

    /// Stop admitting data, send a WebSocket Close, and wait for the peer or
    /// the custodian's retirement deadline. The independent reaper joins both pumps.
    pub fn request_graceful_shutdown(&self) {
        if custodian_state_is_terminal(self.state()) {
            return;
        }
        let mut admission_open = match self.client.admission_open.lock() {
            Ok(admission_open) => admission_open,
            Err(poisoned) => poisoned.into_inner(),
        };
        *admission_open = false;
        drop(admission_open);
        self.client.stop.begin_retirement(StopReason::Requested);
        self.client.shutdown.cancel();
    }

    pub(crate) fn request_graceful_after_local_close(&self) {
        self.client
            .skip_shutdown_frame
            .store(true, Ordering::Release);
        self.request_graceful_shutdown();
    }

    /// Request an out-of-band stop without sending through an application data
    /// mailbox. The supervised WebSocket data and live-detach paths fence
    /// `start_send` against this stop; generic pump users need their own
    /// authority gate at that effect boundary.
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

    /// Enable the parked dark driver's handoff only while the committed
    /// reaper is still running. Its lifecycle lock arbitrates concurrent exit.
    #[cfg(feature = "test-support")]
    pub(crate) fn enable_physical_stop_handoff(&self) -> bool {
        self.state() == CustodianState::Running
            && self
                .client
                .startup_completion_finished
                .load(Ordering::Acquire)
            && !self.client.auxiliary_required
            && self.client.physical_stop.enable()
    }

    /// Queue a request to the reaper without waiting for the sink stop fence.
    /// Acceptance is not evidence that this reason won the physical fence.
    #[cfg(feature = "test-support")]
    pub(crate) fn request_physical_stop(&self, reason: StopReason) -> bool {
        self.client.physical_stop.request(reason)
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn set_stop_listener(&self, listener: Arc<dyn Fn(StopReason) + Send + Sync>) {
        self.client.stop.set_listener(listener);
    }

    /// Install the app-owned retirement fence. It runs once, synchronously,
    /// before either pump-stop cancellation or graceful shutdown is visible.
    pub(crate) fn set_retirement_listener(&self, listener: Arc<dyn Fn(StopReason) + Send + Sync>) {
        self.client.stop.set_retirement_listener(listener);
    }

    /// Create an observation-only handle that can outlive this controller.
    pub fn observer(&self) -> IoCustodianObserver {
        IoCustodianObserver {
            connection_ref: self.client.connection_ref.clone(),
            relinquishment: self.client.relinquishment.clone(),
            stop: self.client.stop.clone(),
            state: self.client.state.clone(),
            summary: self.client.summary.clone(),
            cleanup_pending_published: self.client.cleanup_pending_published.clone(),
            startup_completion_finished: self.client.startup_completion_finished.clone(),
            auxiliary_required: self.client.auxiliary_required,
            auxiliary_task: self.client.auxiliary_task.clone(),
            auxiliary_finished: self.client.auxiliary_finished.clone(),
            auxiliary_registered: self.client.auxiliary_registered.clone(),
            auxiliary_join_started: self.client.auxiliary_join_started.clone(),
            auxiliary_task_registered: self.client.auxiliary_task_registered.clone(),
        }
    }

    /// Wait until every started pump and required auxiliary task has returned.
    /// Canceling this future only cancels the waiter; independent owners keep
    /// joining their tasks.
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

    /// Observe both factory-supplied pump halves for this exact reference.
    pub fn local_io_relinquishment(&self) -> SplitIoRelinquishment {
        SplitIoRelinquishment::new(
            self.connection_ref.clone(),
            self.relinquishment.clone(),
            self.state.clone(),
        )
    }

    /// First stop reason observed by this exact physical connection.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.stop.reason()
    }

    /// Current lifecycle state. `Joined` includes required auxiliary cleanup.
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

    /// Whether a cleanup owner has been registered for this exact connection.
    #[cfg(feature = "test-support")]
    pub fn auxiliary_registered(&self) -> bool {
        self.auxiliary_registered.load(Ordering::Acquire)
    }

    /// Whether the reaper has taken and begun awaiting the registered cleanup
    /// task's JoinHandle. Exposed for deterministic dark lifecycle tests.
    pub fn auxiliary_join_started(&self) -> bool {
        self.auxiliary_join_started.load(Ordering::Acquire)
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

    /// Wait until every started pump and required auxiliary task has returned.
    /// If the custodian exits unexpectedly, the last state is returned without
    /// claiming a successful join.
    pub async fn wait_until_joined(&self) -> CustodianState {
        let mut state = self.state.clone();
        loop {
            let current = *state.borrow_and_update();
            if custodian_state_is_terminal(current) {
                self.wait_for_auxiliary_task().await;
                return current;
            }
            if state.changed().await.is_err() {
                let current = *state.borrow();
                if custodian_state_is_terminal(current) {
                    self.wait_for_auxiliary_task().await;
                }
                return current;
            }
        }
    }

    async fn wait_for_auxiliary_task(&self) {
        if !self.auxiliary_required {
            return;
        }
        while !self.auxiliary_finished.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
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
#[cfg(feature = "test-support")]
pub fn spawn_io_custodian<ReadError, WriteError>(
    supervisor: IoSupervisor<ReadError, WriteError>,
    retirement_deadline: Duration,
) -> IoCustodianHandle
where
    ReadError: Send + 'static,
    WriteError: Send + 'static,
{
    let connection_ref = supervisor.connection_ref.clone();
    let relinquishment = supervisor.relinquishment.clone();
    let stop = supervisor.stop.clone();
    let force_stop = supervisor.force_stop.clone();
    let shutdown = supervisor.shutdown.clone();
    let skip_shutdown_frame = supervisor.skip_shutdown_frame.clone();
    let admission_open = supervisor.admission_open.clone();
    let (state_tx, state_rx) = watch::channel(CustodianState::Running);
    let summary = Arc::new(Mutex::new(None));
    let custodian_summary = summary.clone();
    let cleanup_pending_published = Arc::new(AtomicBool::new(false));
    let custodian_cleanup_pending_published = cleanup_pending_published.clone();
    let startup_completion_finished = Arc::new(AtomicBool::new(true));
    let auxiliary_task = Arc::new(Mutex::new(None));
    let auxiliary_finished = Arc::new(AtomicBool::new(true));
    let auxiliary_join_started = Arc::new(AtomicBool::new(false));
    let auxiliary_task_registered = Arc::new(Notify::new());
    let physical_stop = Arc::new(PhysicalStopHandoff::default());

    tokio::spawn(run_io_custodian(
        supervisor,
        IoCustodianRunContext {
            retirement_deadline,
            state_tx,
            custodian_summary,
            cleanup_pending_published: custodian_cleanup_pending_published,
            startup_finished: None,
            auxiliary_cleanup: None,
            auxiliary_required: false,
            auxiliary_task: auxiliary_task.clone(),
            auxiliary_finished: auxiliary_finished.clone(),
            auxiliary_join_started: auxiliary_join_started.clone(),
            auxiliary_task_registered: auxiliary_task_registered.clone(),
            physical_stop: PhysicalStopReceiver(physical_stop.clone()),
        },
    ));

    IoCustodianHandle {
        client: Arc::new(CustodianClient {
            connection_ref,
            relinquishment,
            stop,
            force_stop,
            shutdown,
            skip_shutdown_frame,
            admission_open,
            state: state_rx,
            summary,
            cleanup_pending_published,
            startup_completion_finished,
            auxiliary_required: false,
            auxiliary_task,
            auxiliary_finished,
            auxiliary_registered: Arc::new(AtomicBool::new(false)),
            auxiliary_join_started,
            auxiliary_task_registered,
            physical_stop,
        }),
    }
}

struct IoCustodianRunContext {
    retirement_deadline: Duration,
    state_tx: watch::Sender<CustodianState>,
    custodian_summary: Arc<Mutex<Option<CustodianSummary>>>,
    cleanup_pending_published: Arc<AtomicBool>,
    startup_finished: Option<watch::Receiver<bool>>,
    auxiliary_cleanup: Option<watch::Receiver<bool>>,
    auxiliary_required: bool,
    auxiliary_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    auxiliary_finished: Arc<AtomicBool>,
    auxiliary_join_started: Arc<AtomicBool>,
    auxiliary_task_registered: Arc<Notify>,
    #[cfg(feature = "test-support")]
    physical_stop: PhysicalStopReceiver,
}

#[derive(Clone, Copy)]
enum InitialRetirement {
    Stop,
    Graceful,
    PumpExited,
    #[cfg(feature = "test-support")]
    PhysicalRequest,
}

// Only the attached dark physical fixture enables this path. A sink may be
// inside start_send while holding StopSignal's synchronous fence; joining a
// blocking stop child leaves the reaper free to enforce its own deadline.
#[cfg(feature = "test-support")]
async fn retire_physical_pumps<ReadError, WriteError>(
    supervisor: &mut IoSupervisor<ReadError, WriteError>,
    handoff: &PhysicalStopHandoff,
    state_tx: &watch::Sender<CustodianState>,
    cleanup_pending_published: &AtomicBool,
    retirement_deadline: Duration,
    initial: InitialRetirement,
    mut inbound: Option<Result<InboundExit<ReadError>, JoinError>>,
    mut outbound: Option<Result<OutboundExit<WriteError>, JoinError>>,
) -> (
    Result<InboundExit<ReadError>, JoinError>,
    Result<OutboundExit<WriteError>, JoinError>,
)
where
    ReadError: Send + 'static,
    WriteError: Send + 'static,
{
    let mut stop_done = matches!(initial, InitialRetirement::Stop);
    let mut stop_reason =
        matches!(initial, InitialRetirement::PumpExited).then_some(StopReason::PumpExited);
    let mut stop_child: Option<JoinHandle<()>> = None;
    let mut child_failed = false;
    let mut force_requested = false;
    let mut deadline_elapsed = false;
    let deadline = tokio::time::sleep(retirement_deadline);
    tokio::pin!(deadline);

    loop {
        if let Some(reason) = handoff.take_request() {
            force_requested = true;
            stop_reason.get_or_insert(reason);
        }
        if !stop_done && stop_child.is_none() && !child_failed {
            if let Some(reason) = stop_reason.take() {
                let stop = supervisor.stop.clone();
                let admission_open = Arc::clone(&supervisor.admission_open);
                stop_child = Some(tokio::task::spawn_blocking(move || {
                    let mut open = admission_open
                        .lock()
                        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
                    *open = false;
                    drop(open);
                    stop.cancel(reason);
                }));
            }
        }
        if stop_done && force_requested {
            supervisor.force_stop.cancel();
        }
        if inbound.is_some()
            && outbound.is_some()
            && stop_child.is_none()
            && !child_failed
            && handoff.close_if_empty()
        {
            return (
                inbound.expect("inbound joined"),
                outbound.expect("outbound joined"),
            );
        }

        tokio::select! {
            _ = handoff.changed.notified() => {},
            _ = supervisor.stop.cancelled(), if !stop_done => stop_done = true,
            result = async { stop_child.as_mut().expect("stop child").await }, if stop_child.is_some() => {
                stop_child = None;
                if result.is_ok() {
                    stop_done = true;
                } else {
                    child_failed = true;
                    cleanup_pending_published.store(true, Ordering::Release);
                    state_tx.send_replace(CustodianState::CleanupPending);
                }
            },
            result = supervisor.inbound.join(), if inbound.is_none() => inbound = Some(result),
            result = supervisor.outbound.join(), if outbound.is_none() => outbound = Some(result),
            _ = &mut deadline, if !deadline_elapsed => {
                deadline_elapsed = true;
                cleanup_pending_published.store(true, Ordering::Release);
                state_tx.send_replace(CustodianState::CleanupPending);
                force_requested = true;
                stop_reason.get_or_insert(StopReason::ShutdownDeadline);
                if inbound.is_none() {
                    supervisor.inbound.abort();
                }
                if outbound.is_none() {
                    supervisor.outbound.abort();
                }
            },
        }
    }
}

async fn run_io_custodian<ReadError, WriteError>(
    mut supervisor: IoSupervisor<ReadError, WriteError>,
    context: IoCustodianRunContext,
) where
    ReadError: Send + 'static,
    WriteError: Send + 'static,
{
    let IoCustodianRunContext {
        retirement_deadline,
        state_tx,
        custodian_summary,
        cleanup_pending_published,
        startup_finished,
        auxiliary_cleanup,
        auxiliary_required,
        auxiliary_task,
        auxiliary_finished,
        auxiliary_join_started,
        auxiliary_task_registered,
        #[cfg(feature = "test-support")]
        physical_stop,
    } = context;
    let mut inbound = None;
    let mut outbound = None;

    let initial = {
        #[cfg(feature = "test-support")]
        let physical_request = physical_stop.0.changed.notified();
        #[cfg(not(feature = "test-support"))]
        let physical_request = std::future::pending::<()>();
        tokio::pin!(physical_request);
        tokio::select! {
            biased;
            _ = supervisor.stop.cancelled() => InitialRetirement::Stop,
            _ = supervisor.shutdown.cancelled() => InitialRetirement::Graceful,
            result = supervisor.inbound.join() => { inbound = Some(result); InitialRetirement::PumpExited },
            result = supervisor.outbound.join() => { outbound = Some(result); InitialRetirement::PumpExited },
            _ = &mut physical_request => {
                #[cfg(feature = "test-support")]
                { InitialRetirement::PhysicalRequest }
                #[cfg(not(feature = "test-support"))]
                { unreachable!("disabled physical handoff") }
            },
        }
    };

    #[cfg(feature = "test-support")]
    let physical_enabled = physical_stop.0.begin_retirement();
    state_tx.send_replace(CustodianState::Stopping);
    #[cfg(feature = "test-support")]
    let physical_joined = if physical_enabled {
        Some(
            retire_physical_pumps(
                &mut supervisor,
                &physical_stop.0,
                &state_tx,
                &cleanup_pending_published,
                retirement_deadline,
                initial,
                inbound.take(),
                outbound.take(),
            )
            .await,
        )
    } else {
        None
    };
    #[cfg(not(feature = "test-support"))]
    let physical_joined: Option<(
        Result<InboundExit<ReadError>, JoinError>,
        Result<OutboundExit<WriteError>, JoinError>,
    )> = None;
    let (inbound, outbound) = if let Some(joined) = physical_joined {
        joined
    } else {
        if !matches!(initial, InitialRetirement::Graceful) {
            supervisor.stop.cancel(StopReason::PumpExited);
        }

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
        (inbound, outbound)
    };
    if let Some(mut startup_finished) = startup_finished {
        wait_for_startup_completion(&mut startup_finished).await;
    }
    wait_for_auxiliary_cleanup(
        AuxiliaryCleanupWait {
            completion: auxiliary_cleanup,
            required: auxiliary_required,
            task: auxiliary_task,
            finished: auxiliary_finished,
            join_started: auxiliary_join_started,
            task_registered: auxiliary_task_registered,
            retirement_deadline,
        },
        &state_tx,
        &cleanup_pending_published,
    )
    .await;
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
    #[cfg(feature = "test-support")]
    relinquishment: watch::Receiver<bool>,
    stop: StopSignal,
    force_stop: CancellationToken,
    shutdown: CancellationToken,
    skip_shutdown_frame: Arc<AtomicBool>,
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
    auxiliary_cleanup: Option<watch::Receiver<bool>>,
    auxiliary_required: bool,
    auxiliary_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    auxiliary_finished: Arc<AtomicBool>,
    auxiliary_join_started: Arc<AtomicBool>,
    auxiliary_task_registered: Arc<Notify>,
    #[cfg(feature = "test-support")]
    physical_stop: PhysicalStopReceiver,
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

struct AuxiliaryCleanupWait {
    completion: Option<watch::Receiver<bool>>,
    required: bool,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
    finished: Arc<AtomicBool>,
    join_started: Arc<AtomicBool>,
    task_registered: Arc<Notify>,
    retirement_deadline: Duration,
}

async fn wait_for_auxiliary_cleanup(
    wait: AuxiliaryCleanupWait,
    state_tx: &watch::Sender<CustodianState>,
    cleanup_pending_published: &AtomicBool,
) {
    let AuxiliaryCleanupWait {
        mut completion,
        required,
        task: auxiliary_task,
        finished: auxiliary_finished,
        join_started: auxiliary_join_started,
        task_registered: auxiliary_task_registered,
        retirement_deadline,
    } = wait;
    if !required {
        return;
    }
    let Some(mut joined) = completion.take() else {
        cleanup_pending_published.store(true, Ordering::Release);
        state_tx.send_replace(CustodianState::CleanupPending);
        return std::future::pending::<()>().await;
    };

    let deadline = tokio::time::sleep(retirement_deadline);
    tokio::pin!(deadline);
    let mut cleanup_pending = false;
    loop {
        if *joined.borrow_and_update() {
            break;
        }
        tokio::select! {
            changed = joined.changed() => {
                if changed.is_err() {
                    // A dropped completion sender is not join proof. Keep the
                    // exact connection discoverable in CleanupPending.
                    cleanup_pending_published.store(true, Ordering::Release);
                    state_tx.send_replace(CustodianState::CleanupPending);
                    std::future::pending::<()>().await;
                }
            }
            _ = &mut deadline, if !cleanup_pending => {
                cleanup_pending = true;
                cleanup_pending_published.store(true, Ordering::Release);
                state_tx.send_replace(CustodianState::CleanupPending);
            }
        }
    }

    // The completion signal is emitted at the end of cleanup, but only the
    // JoinHandle result proves the cleanup task itself has returned. Keep the
    // slot in CleanupPending until that join has completed.
    let mut task = loop {
        let registered = auxiliary_task_registered.notified();
        if let Some(task) = take_auxiliary_task(&auxiliary_task) {
            break task;
        }
        if !cleanup_pending {
            tokio::select! {
                _ = &mut deadline => {
                    cleanup_pending = true;
                    cleanup_pending_published.store(true, Ordering::Release);
                    state_tx.send_replace(CustodianState::CleanupPending);
                }
                _ = registered => {}
            }
        } else {
            registered.await;
        }
    };
    auxiliary_join_started.store(true, Ordering::Release);

    if !cleanup_pending {
        tokio::select! {
            result = &mut task => {
                let _ = result;
            }
            _ = &mut deadline => {
                cleanup_pending_published.store(true, Ordering::Release);
                state_tx.send_replace(CustodianState::CleanupPending);
                let _ = task.await;
            }
        }
    } else {
        let _ = task.await;
    }
    auxiliary_finished.store(true, Ordering::Release);
}

fn take_auxiliary_task(auxiliary_task: &Mutex<Option<JoinHandle<()>>>) -> Option<JoinHandle<()>> {
    let mut retained = match auxiliary_task.lock() {
        Ok(retained) => retained,
        Err(poisoned) => poisoned.into_inner(),
    };
    retained.take()
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
    auxiliary_cleanup: Option<watch::Receiver<bool>>,
    auxiliary_required: bool,
    auxiliary_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    auxiliary_finished: Arc<AtomicBool>,
    auxiliary_join_started: Arc<AtomicBool>,
    auxiliary_task_registered: Arc<Notify>,
    retirement_deadline: Duration,
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
        auxiliary_cleanup,
        auxiliary_required,
        auxiliary_task,
        auxiliary_finished,
        auxiliary_join_started,
        auxiliary_task_registered,
        retirement_deadline,
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
        wait_for_auxiliary_cleanup(
            AuxiliaryCleanupWait {
                completion: auxiliary_cleanup,
                required: auxiliary_required,
                task: auxiliary_task,
                finished: auxiliary_finished,
                join_started: auxiliary_join_started,
                task_registered: auxiliary_task_registered,
                retirement_deadline,
            },
            &state_tx,
            &cleanup_pending_published,
        )
        .await;
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
        #[cfg(feature = "test-support")]
        relinquishment,
        stop,
        force_stop,
        shutdown,
        skip_shutdown_frame,
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
        auxiliary_cleanup,
        auxiliary_required,
        auxiliary_task,
        auxiliary_finished,
        auxiliary_join_started,
        auxiliary_task_registered,
        #[cfg(feature = "test-support")]
        physical_stop,
    } = setup;
    #[cfg(not(feature = "test-support"))]
    let _ = (&skip_shutdown_frame, &admission_open);
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
                        #[cfg(feature = "test-support")]
                        relinquishment,
                        stop,
                        force_stop,
                        shutdown,
                        #[cfg(feature = "test-support")]
                        skip_shutdown_frame,
                        #[cfg(feature = "test-support")]
                        admission_open,
                        inbound: inbound_owner,
                        outbound: outbound_owner,
                    },
                    IoCustodianRunContext {
                        retirement_deadline,
                        state_tx,
                        custodian_summary: summary,
                        cleanup_pending_published,
                        startup_finished: Some(startup_finished),
                        auxiliary_cleanup,
                        auxiliary_required,
                        auxiliary_task,
                        auxiliary_finished,
                        auxiliary_join_started,
                        auxiliary_task_registered,
                        #[cfg(feature = "test-support")]
                        physical_stop,
                    },
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
    wait_for_auxiliary_cleanup(
        AuxiliaryCleanupWait {
            completion: auxiliary_cleanup,
            required: auxiliary_required,
            task: auxiliary_task,
            finished: auxiliary_finished,
            join_started: auxiliary_join_started,
            task_registered: auxiliary_task_registered,
            retirement_deadline,
        },
        &state_tx,
        &cleanup_pending_published,
    )
    .await;
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
        Ok(InboundExit::IoFailure(_) | InboundExit::IoFailureReported) => {
            InboundTerminal::IoFailure
        }
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
    #[cfg(feature = "test-support")]
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// First stop reason, when either pump requested cancellation.
    #[cfg(feature = "test-support")]
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.stop.reason()
    }

    /// Whether the shared transport stop signal is set.
    #[cfg(feature = "test-support")]
    pub fn is_stopped(&self) -> bool {
        self.stop.is_cancelled()
    }

    /// Wait for the shared transport stop signal.
    #[cfg(feature = "test-support")]
    pub async fn stopped(&self) {
        self.stop.cancelled().await;
    }

    /// Begin graceful shutdown. This seals outbound admission, wakes the writer,
    /// and leaves the reader active for the peer Close handshake. Inbound
    /// application frames are discarded after this point. Admitted but
    /// abandoned requests report `NotAttempted`.
    #[cfg(feature = "test-support")]
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
    #[cfg(feature = "test-support")]
    pub fn inbound_finished(&self) -> bool {
        self.inbound.is_finished()
    }

    /// Whether the outbound pump task has finished.
    #[cfg(feature = "test-support")]
    pub fn outbound_finished(&self) -> bool {
        self.outbound.is_finished()
    }

    /// Abort the inbound task. Available only because this whole module is
    /// gated behind `test-support`.
    #[cfg(feature = "test-support")]
    pub fn abort_inbound_for_test(&self) {
        self.inbound.abort();
    }

    /// Abort the outbound task. Available only because this whole module is
    /// gated behind `test-support`.
    #[cfg(feature = "test-support")]
    pub fn abort_outbound_for_test(&self) {
        self.outbound.abort();
    }

    /// Stop both pumps and join them, preserving each task's independent result.
    #[cfg(feature = "test-support")]
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
    #[cfg(feature = "test-support")]
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
    #[cfg(feature = "test-support")]
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

    #[cfg(feature = "test-support")]
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

    #[cfg(feature = "test-support")]
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
    #[cfg(feature = "test-support")]
    connection_ref: ConnectionRef,
    relinquishment: watch::Receiver<bool>,
    stop: StopSignal,
    force_stop: CancellationToken,
    shutdown: CancellationToken,
    skip_shutdown_frame: Arc<AtomicBool>,
    admission_open: Arc<Mutex<bool>>,
    inbound: PumpFuture<InboundExit<ReadError>>,
    outbound: PumpFuture<OutboundExit<WriteError>>,
    outbound_sender: WriteSender<M>,
    inbound_receiver: mpsc::Receiver<QueueItem<M>>,
    startup_completion: Option<StartupCompletionSignal>,
}

struct ReaderPolicy<M, E, Size, IsClose> {
    frame_size: Size,
    is_close: IsClose,
    terminal: Option<oneshot::Sender<OrderedInboundTerminal<M, E>>>,
}

fn prepare_supervised_io<R, W, M, ReadError, WriteError, Size, IsClose, IsSendAfterClose>(
    _connection_ref: ConnectionRef,
    reader: R,
    writer: W,
    limits: TransportLimits,
    shutdown_protocol: ShutdownProtocol<M, IsSendAfterClose>,
    reader_policy: ReaderPolicy<M, ReadError, Size, IsClose>,
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
    let (reader, writer, relinquishment) = track_halves(reader, writer);
    let ShutdownProtocol {
        shutdown_frame,
        is_send_after_close,
    } = shutdown_protocol;
    let stop = StopSignal::new();
    let force_stop = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let skip_shutdown_frame = Arc::new(AtomicBool::new(false));
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
        reader_policy,
    ));
    let outbound = Box::pin(run_writer(
        _connection_ref.clone(),
        writer,
        outbound_receiver,
        WriterSignals {
            stop: stop.clone(),
            force_stop: force_stop.clone(),
            shutdown: shutdown.clone(),
            peer_close,
            skip_shutdown_frame: skip_shutdown_frame.clone(),
        },
        Some(shutdown_frame),
        is_send_after_close,
    ));

    PreparedIo {
        #[cfg(feature = "test-support")]
        connection_ref: _connection_ref,
        relinquishment,
        stop,
        force_stop,
        shutdown: shutdown.clone(),
        skip_shutdown_frame,
        admission_open: admission_open.clone(),
        inbound,
        outbound,
        outbound_sender: WriteSender {
            sender: outbound_queue,
            admission_open,
            shutdown: shutdown.clone(),
        },
        inbound_receiver,
        startup_completion: None,
    }
}

pub(crate) async fn start_reserved_supervised_io<
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
    cancellation: Option<CancellationToken>,
    terminal: Option<oneshot::Sender<OrderedInboundTerminal<M, ReadError>>>,
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
    if cancellation
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(ReservedStartError::CancelledBeforePumps);
    }
    let factory_future = factory(&connection_ref);
    let (reader, writer) = if let Some(cancellation) = cancellation.as_ref() {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                return Err(ReservedStartError::CancelledBeforePumps);
            }
            halves = factory_future => halves,
        }
    } else {
        factory_future.await
    };
    let mut prepared = prepare_supervised_io(
        connection_ref.clone(),
        reader,
        writer,
        limits,
        shutdown_protocol,
        ReaderPolicy {
            frame_size,
            is_close,
            terminal,
        },
    );

    let (state_tx, state_rx) = watch::channel(CustodianState::Starting);
    let summary = Arc::new(Mutex::new(None));
    let cleanup_pending_published = Arc::new(AtomicBool::new(false));
    let startup_completion_finished = Arc::new(AtomicBool::new(false));
    let handle_summary = summary.clone();
    let observer = IoCustodianObserver {
        connection_ref: connection_ref.clone(),
        relinquishment: prepared.relinquishment.clone(),
        stop: prepared.stop.clone(),
        state: state_rx.clone(),
        summary: summary.clone(),
        cleanup_pending_published: cleanup_pending_published.clone(),
        startup_completion_finished: startup_completion_finished.clone(),
        auxiliary_required: reservation.auxiliary_required,
        auxiliary_task: reservation.auxiliary_task.clone(),
        auxiliary_finished: reservation.auxiliary_finished.clone(),
        auxiliary_registered: reservation.auxiliary_registered.clone(),
        auxiliary_join_started: reservation.auxiliary_join_started.clone(),
        auxiliary_task_registered: reservation.auxiliary_task_registered.clone(),
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
    #[cfg(feature = "test-support")]
    let physical_stop = Arc::new(PhysicalStopHandoff::default());

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
        #[cfg(feature = "test-support")]
        relinquishment: prepared.relinquishment.clone(),
        stop: prepared.stop.clone(),
        force_stop: prepared.force_stop.clone(),
        shutdown: prepared.shutdown.clone(),
        skip_shutdown_frame: prepared.skip_shutdown_frame.clone(),
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
        auxiliary_cleanup: reservation.auxiliary_cleanup.clone(),
        auxiliary_required: reservation.auxiliary_required,
        auxiliary_task: reservation.auxiliary_task.clone(),
        auxiliary_finished: reservation.auxiliary_finished.clone(),
        auxiliary_join_started: reservation.auxiliary_join_started.clone(),
        auxiliary_task_registered: reservation.auxiliary_task_registered.clone(),
        #[cfg(feature = "test-support")]
        physical_stop: PhysicalStopReceiver(physical_stop.clone()),
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
        auxiliary_cleanup: reservation.auxiliary_cleanup.clone(),
        auxiliary_required: reservation.auxiliary_required,
        auxiliary_task: reservation.auxiliary_task.clone(),
        auxiliary_finished: reservation.auxiliary_finished.clone(),
        auxiliary_join_started: reservation.auxiliary_join_started.clone(),
        auxiliary_task_registered: reservation.auxiliary_task_registered.clone(),
        retirement_deadline,
    }));

    reservation
        .inventory
        .activate_reservation(&connection_ref, reservation_id, observer)?;
    reservation.active = false;

    let handle = IoCustodianHandle {
        client: Arc::new(CustodianClient {
            connection_ref,
            relinquishment: prepared.relinquishment,
            stop: prepared.stop,
            force_stop: prepared.force_stop,
            shutdown: prepared.shutdown,
            skip_shutdown_frame: prepared.skip_shutdown_frame,
            admission_open: prepared.admission_open,
            state: state_rx,
            summary: handle_summary,
            cleanup_pending_published,
            startup_completion_finished,
            auxiliary_required: reservation.auxiliary_required,
            auxiliary_task: reservation.auxiliary_task.clone(),
            auxiliary_finished: reservation.auxiliary_finished.clone(),
            auxiliary_registered: reservation.auxiliary_registered.clone(),
            auxiliary_join_started: reservation.auxiliary_join_started.clone(),
            auxiliary_task_registered: reservation.auxiliary_task_registered.clone(),
            #[cfg(feature = "test-support")]
            physical_stop,
        }),
    };
    let cancellation_watch =
        cancellation.map(|cancellation| handle.watch_cancellation(cancellation));

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
        handle.request_stop(StopReason::Requested);
        failure_cleanup_pending_published.store(true, Ordering::Release);
        failure_state_tx.send_replace(CustodianState::CleanupPending);
        if let Some(cancellation_watch) = cancellation_watch {
            cancellation_watch.finish().await;
        }
        return Err(ReservedStartError::ReaperUnavailable);
    }
    if ready_rx.await.is_err() {
        handle.request_stop(StopReason::Requested);
        failure_cleanup_pending_published.store(true, Ordering::Release);
        failure_state_tx.send_replace(CustodianState::CleanupPending);
        if let Some(cancellation_watch) = cancellation_watch {
            cancellation_watch.finish().await;
        }
        return Err(ReservedStartError::ReaperUnavailable);
    }
    hook(ReservedStartStage::AfterCommit).await;
    if let Some(cancellation_watch) = cancellation_watch {
        cancellation_watch.finish().await;
    }

    Ok((handle, prepared.outbound_sender, prepared.inbound_receiver))
}

/// Split socket halves into supervised read and write pumps.
#[cfg(feature = "test-support")]
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
        ReaderPolicy {
            frame_size,
            is_close,
            terminal: None,
        },
    );
    let inbound_task = tokio::spawn(prepared.inbound);
    let outbound_task = tokio::spawn(prepared.outbound);

    (
        IoSupervisor {
            connection_ref: prepared.connection_ref,
            relinquishment: prepared.relinquishment,
            stop: prepared.stop,
            force_stop: prepared.force_stop,
            shutdown: prepared.shutdown,
            skip_shutdown_frame: prepared.skip_shutdown_frame,
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
    policy: ReaderPolicy<M, E, Size, IsClose>,
) -> InboundExit<E>
where
    R: Stream<Item = Result<M, E>> + Unpin,
    Size: Fn(&M) -> usize,
    IsClose: Fn(&M) -> bool,
{
    let ReaderPolicy {
        frame_size,
        is_close,
        mut terminal,
    } = policy;
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
                let size = frame_size(&frame);
                let oversized = terminal.is_some()
                    && (size > inbound.max_bytes || u32::try_from(size).is_err());
                if let Some(terminal) = terminal.take() {
                    let event = if oversized {
                        OrderedInboundTerminal::Oversized {
                            size,
                            max_bytes: inbound.max_bytes,
                        }
                    } else {
                        OrderedInboundTerminal::Close(frame)
                    };
                    let _ = terminal.send(event);
                }
                stop.cancel_with_peer_close(
                    if oversized {
                        StopReason::InboundOverload
                    } else {
                        StopReason::PeerClosed
                    },
                    Some(&peer_close),
                );
                return if oversized {
                    InboundExit::Overloaded
                } else {
                    InboundExit::PeerClosed
                };
            }
            Some(Ok(_frame)) if closing => {}
            Some(Ok(frame)) => {
                let byte_len = frame_size(&frame);
                if terminal.is_some()
                    && (byte_len > inbound.max_bytes || u32::try_from(byte_len).is_err())
                {
                    if let Some(terminal) = terminal.take() {
                        let _ = terminal.send(OrderedInboundTerminal::Oversized {
                            size: byte_len,
                            max_bytes: inbound.max_bytes,
                        });
                    }
                    stop.cancel(StopReason::InboundOverload);
                    return InboundExit::Overloaded;
                }
                let admitted = if terminal.is_some() {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => { closing = true; continue; }
                        result = inbound.send_wait(frame, byte_len) => result,
                    }
                } else {
                    inbound.try_send(frame, byte_len)
                };
                match admitted {
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
                if let Some(terminal) = terminal.take() {
                    let _ = terminal.send(OrderedInboundTerminal::IoFailure(error));
                    return InboundExit::IoFailureReported;
                }
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
    skip_shutdown_frame: Arc<AtomicBool>,
}

async fn run_writer<W, M, E, IsSendAfterClose>(
    connection_ref: ConnectionRef,
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
        skip_shutdown_frame,
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
                if skip_shutdown_frame.load(Ordering::Acquire) {
                    return OutboundExit::ShutdownClose;
                }
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
            &QueuedWriteContext {
                connection_ref: &connection_ref,
                stop: &stop,
                force_stop: &force_stop,
                shutdown: &shutdown,
                peer_close: &peer_close,
            },
            &is_send_after_close,
        )
        .await
        {
            SendOutcome::Sent | SendOutcome::AuthorityRevoked => {}
            SendOutcome::Stopped => return OutboundExit::Stopped,
            SendOutcome::PeerClosed => {
                return flush_peer_close_reply(&mut writer, &force_stop, &is_send_after_close)
                    .await;
            }
            SendOutcome::Shutdown => {
                if skip_shutdown_frame.load(Ordering::Acquire) {
                    return OutboundExit::ShutdownClose;
                }
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
    AuthorityRevoked,
    Stopped,
    PeerClosed,
    Shutdown,
    IoFailure(E),
}

struct QueuedWriteContext<'a> {
    connection_ref: &'a ConnectionRef,
    stop: &'a StopSignal,
    force_stop: &'a CancellationToken,
    shutdown: &'a CancellationToken,
    peer_close: &'a CancellationToken,
}

async fn send_queued<W, M, E>(
    writer: &mut W,
    work: QueueItem<WriteRequest<M>>,
    context: &QueuedWriteContext<'_>,
    is_send_after_close: &impl Fn(&E) -> bool,
) -> SendOutcome<E>
where
    W: Sink<M, Error = E> + Unpin,
{
    let QueuedWriteContext {
        connection_ref,
        stop,
        force_stop,
        shutdown,
        peer_close,
    } = *context;
    let mut request = work.value;
    let mut progress = InFlightWrite::new(request.receipt.clone());
    let ready = tokio::select! {
        biased;
        _ = force_stop.cancelled() => return SendOutcome::Stopped,
        _ = peer_close.cancelled() => return SendOutcome::PeerClosed,
        _ = stop.cancelled() => return SendOutcome::Stopped,
        _ = shutdown.cancelled() => return SendOutcome::Shutdown,
        _ = authority_revoked(request.authority.as_deref()) => return SendOutcome::AuthorityRevoked,
        ready = poll_fn(|context| Pin::new(&mut *writer).poll_ready(context)) => ready,
    };
    if let Err(error) = ready {
        if is_send_after_close(&error) {
            return wait_for_peer_close_or_force(force_stop, peer_close).await;
        }
        return SendOutcome::IoFailure(error);
    }

    let authority = request.authority.clone();
    let mut start_result = None;
    let mut attempt = || {
        let value = request
            .value
            .take()
            .expect("write request retains its value until first attempt");
        progress.begin_attempt();
        start_result = Some(Pin::new(&mut *writer).start_send(value));
    };
    let admitted = match authority {
        Some(ref authority) => authority.attempt(connection_ref, stop, Box::new(attempt)),
        None => {
            attempt();
            true
        }
    };
    if !admitted {
        return SendOutcome::AuthorityRevoked;
    }
    let Some(result) = start_result else {
        return SendOutcome::AuthorityRevoked;
    };
    if let Err(error) = result {
        progress.uncertain();
        if is_send_after_close(&error) {
            return wait_for_peer_close_or_force(force_stop, peer_close).await;
        }
        return SendOutcome::IoFailure(error);
    }
    progress.attempted();
    #[cfg(feature = "test-support")]
    if let Some(authority) = authority.as_deref() {
        authority.after_attempt().await;
    }

    let flushed = tokio::select! {
        biased;
        _ = force_stop.cancelled() => return SendOutcome::Stopped,
        _ = peer_close.cancelled() => return SendOutcome::PeerClosed,
        _ = stop.cancelled() => return SendOutcome::Stopped,
        _ = shutdown.cancelled() => return SendOutcome::Shutdown,
        _ = authority_revoked(request.authority.as_deref()) => return SendOutcome::AuthorityRevoked,
        flushed = poll_fn(|context| Pin::new(&mut *writer).poll_flush(context)) => flushed,
    };
    match flushed {
        Ok(()) => {
            progress.completed();
            if let Some(authority) = authority.as_deref() {
                authority.completed();
            }
            SendOutcome::Sent
        }
        Err(error) if is_send_after_close(&error) => {
            wait_for_peer_close_or_force(force_stop, peer_close).await
        }
        Err(error) => SendOutcome::IoFailure(error),
    }
}

async fn authority_revoked(authority: Option<&dyn WriteAuthority>) {
    match authority {
        Some(authority) => authority.revoked().await,
        None => std::future::pending().await,
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
        self.receipt.set(WRITE_ATTEMPTED);
    }

    fn uncertain(&mut self) {
        self.attempted = true;
        self.receipt.set(WRITE_UNCERTAIN);
    }

    fn completed(&mut self) {
        self.completed = true;
        self.receipt.set(WRITE_COMPLETED);
    }
}

impl Drop for InFlightWrite {
    fn drop(&mut self) {
        if self.attempted && !self.completed {
            self.receipt.set(WRITE_UNCERTAIN);
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
mod physical_stop_tests {
    use super::*;
    use waddle_xmpp::ownership::NodeLeaseIdentity;

    fn reaper(
        stop: StopSignal,
        inbound: JoinHandle<InboundExit<()>>,
        outbound: JoinHandle<OutboundExit<()>>,
    ) -> IoCustodianHandle {
        let (_, relinquishment) = watch::channel(false);
        spawn_io_custodian(
            IoSupervisor {
                connection_ref: ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
                relinquishment,
                stop,
                force_stop: CancellationToken::new(),
                shutdown: CancellationToken::new(),
                skip_shutdown_frame: Arc::new(AtomicBool::new(false)),
                admission_open: Arc::new(Mutex::new(true)),
                inbound: TaskOwner::new(inbound),
                outbound: TaskOwner::new(outbound),
            },
            Duration::from_millis(50),
        )
    }

    async fn wait_until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reaper state deadline");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn physical_stop_handoff_is_serviced_after_competing_pump_exit() {
        let stop = StopSignal::new();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let held_stop = stop.clone();
        let held = std::thread::spawn(move || {
            held_stop.start_send_if_running(|| {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
            });
        });
        entered_rx.recv().expect("stop fence held");
        let (exit_tx, exit_rx) = oneshot::channel();
        let inbound = tokio::spawn(async move {
            let _ = exit_rx.await;
            InboundExit::Stopped
        });
        let outbound = tokio::spawn(std::future::pending::<OutboundExit<()>>());
        let owner = reaper(stop, inbound, outbound);
        assert!(owner.enable_physical_stop_handoff());
        let observer = owner.observer();
        exit_tx.send(()).expect("inbound exits");
        wait_until(|| observer.state() == CustodianState::Stopping).await;
        assert!(owner.request_physical_stop(StopReason::LeaseLost));
        wait_until(|| owner.client.physical_stop.lock().pending.is_none()).await;
        wait_until(|| observer.state() == CustodianState::CleanupPending).await;
        assert!(observer.terminal_summary().is_none());
        release_tx.send(()).expect("release fence");
        held.join().expect("sink thread joined");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), observer.wait_until_joined())
                .await
                .expect("joined reaper"),
            CustodianState::Joined
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn failed_physical_stop_child_never_reports_joined() {
        let stop = StopSignal::new();
        let panic_once = Arc::new(AtomicBool::new(true));
        let callback_panic = Arc::clone(&panic_once);
        stop.set_listener(Arc::new(move |_| {
            if callback_panic.swap(false, Ordering::AcqRel) {
                panic!("test stop-child failure");
            }
        }));
        let inbound = tokio::spawn(std::future::pending::<InboundExit<()>>());
        let outbound = tokio::spawn(std::future::pending::<OutboundExit<()>>());
        let owner = reaper(stop, inbound, outbound);
        assert!(owner.enable_physical_stop_handoff());
        let observer = owner.observer();
        assert!(owner.request_physical_stop(StopReason::Requested));
        wait_until(|| observer.state() == CustodianState::CleanupPending).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(150), observer.wait_until_joined())
                .await
                .is_err()
        );
        assert!(observer.terminal_summary().is_none());
    }
}
