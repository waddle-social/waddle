//! Test-gated owner for bounded WebSocket I/O pumps.
//!
//! This module is an isolated transport boundary for #2046. The production
//! connection loop does not instantiate it yet. It carries the physical
//! connection reference through task retirement, bounds queued items by count
//! and bytes, and reports local write progress without making an XMPP or
//! durable-custody claim.

use futures::{Sink, SinkExt, Stream, StreamExt};
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
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
    Attempted,
    Completed,
    Uncertain,
    NotAttempted,
}

/// Receipt for one outbound item. Completion means the local sink flush
/// completed; it does not mean the peer processed or acknowledged the frame.
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
    Shutdown,
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

    /// Begin graceful shutdown. This seals outbound admission before waking the
    /// writer. Admitted but abandoned requests report `NotAttempted`.
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

impl<T> Drop for TaskOwner<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// Split socket halves into supervised read and write pumps.
pub fn spawn_supervised_io<R, W, M, ReadError, WriteError, Size, IsClose>(
    connection_ref: ConnectionRef,
    reader: R,
    writer: W,
    limits: TransportLimits,
    shutdown_frame: M,
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
{
    let stop = StopSignal::new();
    let force_stop = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let peer_close = CancellationToken::new();
    let admission_open = Arc::new(Mutex::new(true));
    let (inbound, inbound_rx) = bounded_channel(
        limits.inbound,
        stop.clone(),
        StopReason::InboundOverload,
        StopReason::InboundQueueClosed,
    );
    let (outbound_queue, outbound_rx) = bounded_channel(
        limits.outbound,
        stop.clone(),
        StopReason::OutboundOverload,
        StopReason::OutboundQueueClosed,
    );

    let reader_stop = stop.clone();
    let reader_shutdown = shutdown.clone();
    let reader_peer_close = peer_close.clone();
    let inbound_task = tokio::spawn(async move {
        run_reader(
            reader,
            inbound,
            reader_stop,
            reader_shutdown,
            reader_peer_close,
            frame_size,
            is_close,
        )
        .await
    });

    let writer_stop = stop.clone();
    let writer_force_stop = force_stop.clone();
    let writer_shutdown = shutdown.clone();
    let writer_peer_close = peer_close;
    let outbound_task = tokio::spawn(async move {
        run_writer(
            writer,
            outbound_rx,
            writer_stop,
            writer_force_stop,
            writer_shutdown,
            writer_peer_close,
            Some(shutdown_frame),
        )
        .await
    });

    (
        IoSupervisor {
            connection_ref,
            stop,
            force_stop,
            shutdown,
            admission_open: admission_open.clone(),
            inbound: TaskOwner::new(inbound_task),
            outbound: TaskOwner::new(outbound_task),
        },
        WriteSender {
            sender: outbound_queue,
            admission_open,
        },
        inbound_rx,
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
    loop {
        let frame = tokio::select! {
            biased;
            _ = stop.cancelled() => return InboundExit::Stopped,
            _ = shutdown.cancelled() => return InboundExit::Shutdown,
            frame = reader.next() => frame,
        };
        match frame {
            Some(Ok(frame)) if is_close(&frame) => {
                peer_close.cancel();
                stop.cancel(StopReason::PeerClosed);
                return InboundExit::PeerClosed;
            }
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

async fn run_writer<W, M, E>(
    mut writer: W,
    mut outbound: mpsc::Receiver<QueueItem<WriteRequest<M>>>,
    stop: StopSignal,
    force_stop: CancellationToken,
    shutdown: CancellationToken,
    peer_close: CancellationToken,
    mut shutdown_frame: Option<M>,
) -> OutboundExit<E>
where
    W: Sink<M, Error = E> + Unpin,
{
    loop {
        let work = tokio::select! {
            biased;
            _ = force_stop.cancelled() => return OutboundExit::Stopped,
            _ = peer_close.cancelled() => {
                return flush_peer_close_reply(&mut writer, &force_stop).await;
            }
            _ = stop.cancelled() => return OutboundExit::Stopped,
            _ = shutdown.cancelled() => {
                let Some(frame) = shutdown_frame.take() else {
                    return OutboundExit::Stopped;
                };
                return send_shutdown_frame(&mut writer, frame, &stop).await;
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
        )
        .await
        {
            SendOutcome::Sent => {}
            SendOutcome::Stopped => return OutboundExit::Stopped,
            SendOutcome::PeerClosed => {
                return flush_peer_close_reply(&mut writer, &force_stop).await;
            }
            SendOutcome::Shutdown => {
                let Some(frame) = shutdown_frame.take() else {
                    return OutboundExit::Stopped;
                };
                return send_shutdown_frame(&mut writer, frame, &stop).await;
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
        return SendOutcome::IoFailure(error);
    }

    progress.attempted();
    let value = request
        .value
        .take()
        .expect("write request retains its value until first attempt");
    if let Err(error) = Pin::new(&mut *writer).start_send(value) {
        return SendOutcome::IoFailure(error);
    }

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
        Err(error) => SendOutcome::IoFailure(error),
    }
}

async fn flush_peer_close_reply<W, M, E>(
    writer: &mut W,
    force_stop: &CancellationToken,
) -> OutboundExit<E>
where
    W: Sink<M, Error = E> + Unpin,
{
    let result = tokio::select! {
        biased;
        _ = force_stop.cancelled() => return OutboundExit::Stopped,
        result = writer.flush() => result,
    };
    match result {
        Ok(()) => OutboundExit::PeerClosedReplyFlushed,
        Err(error) => OutboundExit::IoFailure(error),
    }
}

async fn send_shutdown_frame<W, M, E>(
    writer: &mut W,
    shutdown_frame: M,
    stop: &StopSignal,
) -> OutboundExit<E>
where
    W: Sink<M, Error = E> + Unpin,
{
    let result = tokio::select! {
        biased;
        _ = stop.cancelled() => return OutboundExit::Stopped,
        result = writer.send(shutdown_frame) => result,
    };
    match result {
        Ok(()) => OutboundExit::ShutdownClose,
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

    fn attempted(&mut self) {
        self.attempted = true;
        self.receipt.0.store(WRITE_ATTEMPTED, Ordering::Release);
    }

    fn completed(&mut self) {
        self.completed = true;
        self.receipt.0.store(WRITE_COMPLETED, Ordering::Release);
    }
}

impl Drop for InFlightWrite {
    fn drop(&mut self) {
        if self.attempted && !self.completed {
            self.receipt.0.store(WRITE_UNCERTAIN, Ordering::Release);
        }
    }
}
