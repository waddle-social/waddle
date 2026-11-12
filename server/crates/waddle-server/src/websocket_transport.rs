//! Bounded raw WebSocket transport for a single connection owner.
//!
//! This boundary does not parse incoming XMPP. Raw frames must be parsed before
//! entering protocol actor messages. Outgoing data carries this transport's
//! exact identity and lifecycle authority to the physical sink enqueue.
//! The production upgrade path takes an `AppState`-bound admission and starts
//! these pumps from it for the single-task connection loop, which reads on
//! demand through [`WebSocketInput`] and writes through
//! [`WebSocketMessageSink`]. The pump custodian holds the connection's
//! inventory slot until both pumps have joined and the owner has finished.
//! Revocation after splitting but before supervised startup gets a bounded
//! Close attempt on the split writer. If startup is dropped during that window,
//! an independent bounded cleanup task retains its capacity reservation.
//!
//! `SinkExt::send` waits for the physical flush, and the single-task loop
//! relies on that. An owner that must keep handling XEP-0198 acknowledgements
//! behind a slow writer instead keeps a `submit` future and its receipts
//! selectable alongside inbound frames and independent control.
//!
//! Queue limits cover admitted and in-flight items. The reader can additionally
//! hold one frame while waiting for capacity; the output adapter can hold one
//! frame while it awaits admission. Each admitted frame must fit its directional
//! byte limit. The underlying WebSocket can allocate an oversized frame before
//! this boundary observes and rejects it; these limits do not bound that storage.
//! A received frame leaves the queue budget when yielded. The protocol owner
//! must separately bound any frames or parsed operations that it retains. A
//! terminal I/O error is one retained value; its internal allocation is not
//! measured by the WebSocket payload-byte budget.
//! One terminal slot retains a peer Close or read error behind prior inputs;
//! an oversized Close is reported as `FrameTooLarge` rather than retained.

use crate::clustering::{NodeAdmissionError, NodeAdmissionPermit, NodeLifecycle};
use crate::server::routes::websocket::supervised_io::{
    fixed_terminal_text, start_reserved_supervised_io, FixedTerminalText, OrderedInboundTerminal,
    QueueItem, ReservedIoStartConfig, ShutdownProtocol, StopSignal, WriteAuthority, WriteSender,
};
use axum::extract::ws::{Message, WebSocket};
use futures::future::{poll_fn, BoxFuture};
use futures::{Sink, SinkExt, Stream, StreamExt};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::{
    ConnectionRef, NodeIdentityStamp, NodeLeaseIdentity, SharedNodeIdentity,
};
use waddle_xmpp::registry::{ActorOutboundCustody, ActorOutboundId};

pub use crate::server::routes::websocket::supervised_io::{
    AdmissionError, CustodianInventoryError, CustodianState, CustodianSummary, InboundTerminal,
    IoCustodianHandle, IoCustodianInventory, IoCustodianObserver, IoCustodianReservation, Limits,
    LimitsError, OutboundTerminal, ReservedStartError, ReservedStartStage, StopReason,
    TransportLimits, WriteProgress, WriteReceipt,
};

/// A terminal read error. The original transport error is preserved.
#[derive(Debug, thiserror::Error)]
pub enum InputError<E> {
    #[error("WebSocket read failed: {0}")]
    Transport(E),
    #[error("WebSocket frame has {size} bytes, exceeding the {max_bytes}-byte inbound limit")]
    FrameTooLarge { size: usize, max_bytes: usize },
}

/// A write error. Transport errors retain their original concrete value.
#[derive(Debug, thiserror::Error)]
pub enum OutputError<E> {
    #[error("WebSocket admission failed: {0:?}")]
    Admission(AdmissionError),
    #[error("WebSocket write failed: {0}")]
    Transport(Arc<E>),
    #[error("WebSocket write ended with local status {0:?}")]
    Interrupted(WriteProgress),
    #[error("WebSocket output is closing")]
    Closing,
    #[error("WebSocket retirement did not finish cleanly")]
    RetirementFailed,
    #[error("frame belongs to a different WebSocket transport")]
    WrongConnection,
    #[error("frame is not valid in the current transport phase")]
    InvalidFrame,
}

/// Input preserves all raw WebSocket frames and their original read error.
pub struct WebSocketInput<E> {
    connection_ref: ConnectionRef,
    frames: mpsc::Receiver<QueueItem<Message>>,
    terminal: oneshot::Receiver<OrderedInboundTerminal<Message, E>>,
    finished: bool,
    demand: Option<Arc<ReadDemand>>,
    demand_outstanding: bool,
    /// A frame `settle` took off the queue for the next `poll_next`.
    settled: Option<QueueItem<Message>>,
}

/// One-frame read demand from the input to its reader pump.
///
/// A reader that runs ahead of its owner reads a peer Close before the owner
/// has answered the frames in front of it, and the socket then refuses those
/// answers. With demand the pump reads one frame each time the owner asks for
/// one, which is the order a single task reading its own socket observes.
#[derive(Default)]
struct ReadDemand {
    granted: std::sync::atomic::AtomicBool,
    reader: futures::task::AtomicWaker,
    /// The reader polled the socket for the current grant and found nothing.
    idle: std::sync::atomic::AtomicBool,
    owner: futures::task::AtomicWaker,
}

impl ReadDemand {
    fn grant(&self) {
        self.idle.store(false, std::sync::atomic::Ordering::Release);
        self.granted
            .store(true, std::sync::atomic::Ordering::Release);
        self.reader.wake();
    }
}

/// A reader half that yields at most one frame per grant. Without a demand
/// handle it reads freely.
struct DemandGated<R> {
    inner: R,
    demand: Option<Arc<ReadDemand>>,
    holding: bool,
}

impl<R> DemandGated<R> {
    fn new(inner: R, demand: Option<Arc<ReadDemand>>) -> Self {
        Self {
            inner,
            demand,
            holding: false,
        }
    }
}

impl<R> Stream for DemandGated<R>
where
    R: Stream + Unpin,
{
    type Item = R::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(demand) = &this.demand {
            if !this.holding {
                demand.reader.register(cx.waker());
                if !demand
                    .granted
                    .swap(false, std::sync::atomic::Ordering::AcqRel)
                {
                    return Poll::Pending;
                }
                this.holding = true;
            }
        }
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(item) => {
                this.holding = false;
                Poll::Ready(item)
            }
            Poll::Pending => {
                if let Some(demand) = &this.demand {
                    demand
                        .idle
                        .store(true, std::sync::atomic::Ordering::Release);
                    demand.owner.wake();
                }
                Poll::Pending
            }
        }
    }
}

impl<E> WebSocketInput<E> {
    /// Exact physical connection whose reader owns these frames.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// Wait until a frame the socket already holds is ready for `poll_next`,
    /// or the reader has found the socket empty or has ended.
    ///
    /// A demand-driven reader reads in its own task, so a frame the kernel has
    /// already delivered is not ready on the first poll the way it is for an
    /// owner reading its own socket. An owner whose `select!` prefers input
    /// over its timers settles first to keep that preference. This asks for at
    /// most the one frame the next `poll_next` would ask for, and returns at
    /// once on an input that does not read on demand.
    pub(crate) async fn settle(&mut self) {
        let Some(demand) = self.demand.clone() else {
            return;
        };
        if self.finished || self.settled.is_some() {
            return;
        }
        poll_fn(|cx| match self.frames.poll_recv(cx) {
            Poll::Ready(Some(frame)) => {
                self.demand_outstanding = false;
                self.settled = Some(frame);
                Poll::Ready(())
            }
            Poll::Ready(None) => Poll::Ready(()),
            Poll::Pending => {
                if !self.demand_outstanding {
                    self.demand_outstanding = true;
                    demand.grant();
                }
                demand.owner.register(cx.waker());
                if demand.idle.load(std::sync::atomic::Ordering::Acquire) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }
        })
        .await;
    }
}

/// One raw transport item retained under its original pump budget. Terminal
/// values are separate because the reader holds them in its ordered slot.
#[cfg(feature = "test-support")]
#[expect(dead_code, reason = "the actor-owned endpoint does not use this yet")]
pub(crate) enum RetainedWebSocketInput {
    Frame(QueueItem<Message>),
    Close(Message),
}

#[cfg(feature = "test-support")]
impl<E> WebSocketInput<E> {
    #[expect(dead_code, reason = "the actor-owned endpoint does not use this yet")]
    pub(crate) async fn recv_retained(
        &mut self,
    ) -> Option<Result<RetainedWebSocketInput, InputError<E>>> {
        poll_fn(|cx| {
            if self.finished {
                return Poll::Ready(None);
            }
            match self.frames.poll_recv(cx) {
                Poll::Ready(Some(frame)) => {
                    return Poll::Ready(Some(Ok(RetainedWebSocketInput::Frame(frame))));
                }
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {}
            }
            match Pin::new(&mut self.terminal).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(result) => {
                    self.finished = true;
                    Poll::Ready(match result {
                        Ok(OrderedInboundTerminal::Close(frame)) => {
                            Some(Ok(RetainedWebSocketInput::Close(frame)))
                        }
                        Ok(OrderedInboundTerminal::IoFailure(error)) => {
                            Some(Err(InputError::Transport(error)))
                        }
                        Ok(OrderedInboundTerminal::Oversized { size, max_bytes }) => {
                            Some(Err(InputError::FrameTooLarge { size, max_bytes }))
                        }
                        Err(_) => None,
                    })
                }
            }
        })
        .await
    }
}

impl<E> Stream for WebSocketInput<E> {
    type Item = Result<Message, InputError<E>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        if let Some(frame) = this.settled.take() {
            return Poll::Ready(Some(Ok(frame.into_inner())));
        }
        match this.frames.poll_recv(cx) {
            Poll::Ready(Some(frame)) => {
                this.demand_outstanding = false;
                return Poll::Ready(Some(Ok(frame.into_inner())));
            }
            Poll::Pending => {
                // The terminal slot resolves only after the frame queue has
                // closed, so one grant covers either the next frame or the
                // terminal value.
                if let Some(demand) = &this.demand {
                    if !this.demand_outstanding {
                        this.demand_outstanding = true;
                        demand.grant();
                    }
                }
                return Poll::Pending;
            }
            Poll::Ready(None) => {}
        }
        match Pin::new(&mut this.terminal).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                this.finished = true;
                Poll::Ready(match result {
                    Ok(OrderedInboundTerminal::Close(frame)) => Some(Ok(frame)),
                    Ok(OrderedInboundTerminal::IoFailure(error)) => {
                        Some(Err(InputError::Transport(error)))
                    }
                    Ok(OrderedInboundTerminal::Oversized { size, max_bytes }) => {
                        Some(Err(InputError::FrameTooLarge { size, max_bytes }))
                    }
                    Err(_) => None,
                })
            }
        }
    }
}

struct ErrorCapture<W, E> {
    inner: W,
    error: Arc<Mutex<Option<Arc<E>>>>,
}

impl<W, E> ErrorCapture<W, E> {
    fn record(&self, error: E) -> Arc<E> {
        let error = Arc::new(error);
        let mut saved = match self.error.lock() {
            Ok(saved) => saved,
            Err(poisoned) => poisoned.into_inner(),
        };
        if saved.is_none() {
            *saved = Some(Arc::clone(&error));
        }
        error
    }
}

impl<W, E> Sink<Message> for ErrorCapture<W, E>
where
    W: Sink<Message, Error = E> + Unpin,
{
    type Error = Arc<E>;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_ready(cx) {
            Poll::Ready(Err(error)) => Poll::Ready(Err(this.record(error))),
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn start_send(self: Pin<&mut Self>, frame: Message) -> Result<(), Self::Error> {
        let this = self.get_mut();
        Pin::new(&mut this.inner)
            .start_send(frame)
            .map_err(|error| this.record(error))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_flush(cx) {
            Poll::Ready(Err(error)) => Poll::Ready(Err(this.record(error))),
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_close(cx) {
            Poll::Ready(Err(error)) => Poll::Ready(Err(this.record(error))),
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransportPhase {
    Open,
    Shutdown,
    LiveDetach,
    CloseOnly,
}

#[derive(Clone, Copy)]
enum FrameMode {
    Data,
    Shutdown,
    LiveDetach,
    TransportClose,
}

/// Failure to prepare or start an exact socket admission.
#[derive(Debug, thiserror::Error)]
pub enum SocketAdmissionError {
    #[error(transparent)]
    Lifecycle(#[from] NodeAdmissionError),
    #[error(transparent)]
    Inventory(#[from] CustodianInventoryError),
    #[error("node identity source revision is no longer current")]
    IdentityRevoked,
    #[error("clustering is enabled but the node identity source is missing")]
    MissingClusterIdentity,
    #[error("reservation belongs to a different physical connection")]
    WrongReservation,
    #[error("socket startup was cancelled")]
    Cancelled,
    #[error(transparent)]
    Start(#[from] ReservedStartError),
}

enum IdentitySource {
    Clustered {
        source: SharedNodeIdentity,
        stamp: NodeIdentityStamp,
    },
    Standalone,
}

impl IdentitySource {
    fn with_current<R>(&self, effect: impl FnOnce() -> R) -> Option<R> {
        match self {
            Self::Clustered { source, stamp } => source.with_current_stamp(stamp, |_| effect()),
            Self::Standalone => Some(effect()),
        }
    }

    async fn revoked(&self) {
        match self {
            Self::Clustered { stamp, .. } => stamp.revoked().await,
            Self::Standalone => std::future::pending().await,
        }
    }
}

/// A pre-parse snapshot of the exact lifecycle generation and identity
/// revision. It holds no inventory slot and no lock across an await.
pub(crate) struct PreparedSocketAdmission {
    permit: NodeAdmissionPermit,
    source: IdentitySource,
    lease: NodeLeaseIdentity,
}

impl PreparedSocketAdmission {
    pub(crate) fn clustered(
        lifecycle: &NodeLifecycle,
        source: &SharedNodeIdentity,
    ) -> Result<Self, SocketAdmissionError> {
        let stamp = source
            .current_stamp()
            .ok_or(SocketAdmissionError::IdentityRevoked)?;
        let lease = NodeLeaseIdentity::from_node_identity(stamp.identity());
        let permit = source
            .with_current_stamp(&stamp, |_| lifecycle.admit())
            .ok_or(SocketAdmissionError::IdentityRevoked)??;
        Ok(Self {
            permit,
            source: IdentitySource::Clustered {
                source: source.clone(),
                stamp,
            },
            lease,
        })
    }

    pub(crate) fn standalone(lifecycle: &NodeLifecycle) -> Result<Self, SocketAdmissionError> {
        Ok(Self {
            permit: lifecycle.admit()?,
            source: IdentitySource::Standalone,
            lease: NodeLeaseIdentity::local_process(),
        })
    }

    pub(crate) fn reserve(
        self,
        inventory: &IoCustodianInventory,
        process_stop: CancellationToken,
        live_interrupt: CancellationToken,
    ) -> Result<SocketAdmission, SocketAdmissionError> {
        SocketAdmission::reserve(
            self.permit,
            self.source,
            self.lease,
            inventory,
            process_stop,
            live_interrupt,
        )
    }
}

/// A single, non-cloneable pre-upgrade reservation. Production constructs it
/// through the `AppState`-bound gate; the generic test path accepts handles
/// independently for isolated transport tests.
pub(crate) struct SocketAdmission {
    authority: Arc<TransportAuthority>,
    reservation: IoCustodianReservation,
}

/// Owns split halves until the supervised startup has taken responsibility.
/// Before that handoff, a dropped real socket starts one independent bounded
/// Close attempt while keeping its reservation alive until the attempt ends.
struct UnstartedSocketParts<R, W>
where
    W: Sink<Message> + Unpin + Send + 'static,
    W::Error: Send + Sync + 'static,
{
    authority: Arc<TransportAuthority>,
    reservation: Option<IoCustodianReservation>,
    reader: Option<R>,
    writer: Option<W>,
    retirement_deadline: Duration,
    close_on_drop: bool,
    close_task: Option<tokio::task::JoinHandle<()>>,
    #[cfg(feature = "test-support")]
    close_probe: Option<Arc<ActorWriteProbe>>,
}

impl<R, W> UnstartedSocketParts<R, W>
where
    W: Sink<Message> + Unpin + Send + 'static,
    W::Error: Send + Sync + 'static,
{
    fn new(
        authority: Arc<TransportAuthority>,
        reservation: IoCustodianReservation,
        reader: R,
        writer: W,
        retirement_deadline: Duration,
        close_on_drop: bool,
        #[cfg(feature = "test-support")] close_probe: Option<Arc<ActorWriteProbe>>,
    ) -> Self {
        Self {
            authority,
            reservation: Some(reservation),
            reader: Some(reader),
            writer: Some(writer),
            retirement_deadline,
            close_on_drop,
            close_task: None,
            #[cfg(feature = "test-support")]
            close_probe,
        }
    }

    fn begin_close(&mut self) {
        if self.close_task.is_some() {
            return;
        }
        if !self.close_on_drop {
            self.reader.take();
            self.writer.take();
            self.reservation.take();
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (Some(mut writer), Some(reservation)) = (self.writer.take(), self.reservation.take())
        else {
            return;
        };
        self.reader.take();
        let deadline = self.retirement_deadline;
        #[cfg(feature = "test-support")]
        let close_probe = self.close_probe.take();
        self.close_task = Some(runtime.spawn(async move {
            let _reservation = reservation;
            let close = async {
                #[cfg(feature = "test-support")]
                if let Some(probe) = close_probe {
                    probe.before_close().await;
                }
                let _ = writer.send(Message::Close(None)).await;
                let _ = writer.close().await;
            };
            let _ = tokio::time::timeout(deadline, close).await;
        }));
    }

    async fn close_bounded(&mut self) {
        self.begin_close();
        if let Some(task) = self.close_task.as_mut() {
            let _ = task.await;
        }
    }
}

impl<R, W> Drop for UnstartedSocketParts<R, W>
where
    W: Sink<Message> + Unpin + Send + 'static,
    W::Error: Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.begin_close();
    }
}

#[cfg_attr(
    not(feature = "test-support"),
    expect(
        dead_code,
        reason = "the inactive connection owner is not integrated yet"
    )
)]
impl SocketAdmission {
    pub(crate) fn clustered(
        lifecycle: &NodeLifecycle,
        source: &SharedNodeIdentity,
        inventory: &IoCustodianInventory,
        process_stop: CancellationToken,
        live_interrupt: CancellationToken,
    ) -> Result<Self, SocketAdmissionError> {
        PreparedSocketAdmission::clustered(lifecycle, source)?.reserve(
            inventory,
            process_stop,
            live_interrupt,
        )
    }

    pub(crate) fn standalone(
        lifecycle: &NodeLifecycle,
        inventory: &IoCustodianInventory,
        process_stop: CancellationToken,
        live_interrupt: CancellationToken,
    ) -> Result<Self, SocketAdmissionError> {
        PreparedSocketAdmission::standalone(lifecycle)?.reserve(
            inventory,
            process_stop,
            live_interrupt,
        )
    }

    fn standalone_with_identity(
        lifecycle: &NodeLifecycle,
        lease: NodeLeaseIdentity,
        inventory: &IoCustodianInventory,
        process_stop: CancellationToken,
        live_interrupt: CancellationToken,
    ) -> Result<Self, SocketAdmissionError> {
        Self::reserve(
            lifecycle.admit()?,
            IdentitySource::Standalone,
            lease,
            inventory,
            process_stop,
            live_interrupt,
        )
    }

    fn reserve(
        permit: NodeAdmissionPermit,
        source: IdentitySource,
        lease: NodeLeaseIdentity,
        inventory: &IoCustodianInventory,
        process_stop: CancellationToken,
        live_interrupt: CancellationToken,
    ) -> Result<Self, SocketAdmissionError> {
        let authority = Arc::new(TransportAuthority {
            connection_ref: ConnectionRef::fresh(lease),
            source,
            permit,
            process_stop,
            live_interrupt,
            terminal_started: CancellationToken::new(),
            phase: RwLock::new(TransportPhase::Open),
            #[cfg(feature = "test-support")]
            actor_write_probe: Mutex::new(None),
            #[cfg(feature = "test-support")]
            actor_reaper_exit: Mutex::new(None),
        });
        let reservation = inventory.reserve(authority.connection_ref.clone())?;
        let admission = Self::bind(authority, reservation)?;
        admission.authority.with_start_authority(|| ())?;
        Ok(admission)
    }

    fn bind(
        authority: Arc<TransportAuthority>,
        reservation: IoCustodianReservation,
    ) -> Result<Self, SocketAdmissionError> {
        if !reservation.is_for(&authority.connection_ref) {
            return Err(SocketAdmissionError::WrongReservation);
        }
        Ok(Self {
            authority,
            reservation,
        })
    }

    pub(crate) fn connection_ref(&self) -> &ConnectionRef {
        &self.authority.connection_ref
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn install_actor_write_probe(&self, probe: Arc<ActorWriteProbe>) {
        let mut saved = self
            .authority
            .actor_write_probe
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        *saved = Some(probe);
    }

    /// Keep the eventual pump inventory nonterminal until every dark actor-side
    /// task has returned and its independent cleanup owner is joined.
    #[cfg(feature = "test-support")]
    #[expect(dead_code, reason = "the actor-owned endpoint does not use this yet")]
    pub(crate) fn retain_actor_cleanup(
        &mut self,
    ) -> Result<
        crate::server::routes::websocket::supervised_io::AuxiliaryCleanupGuard,
        CustodianInventoryError,
    > {
        self.reservation.retain_until_auxiliary_cleanup()
    }

    /// Consume the admission into the single-task WebSocket loop. The loop
    /// starts its pumps with [`LegacySocketAdmission::start_socket`]; until
    /// then the reservation is only a capacity hold.
    pub(crate) fn into_legacy(self) -> LegacySocketAdmission {
        let permit = match &self.authority.source {
            IdentitySource::Clustered { source, stamp } => self
                .authority
                .permit
                .with_identity(source.clone(), stamp.clone()),
            IdentitySource::Standalone => self.authority.permit.clone(),
        };
        LegacySocketAdmission {
            authority: self.authority,
            permit,
            reservation: Some(self.reservation),
        }
    }

    /// The only production socket entry consumes the actual upgraded Axum
    /// socket and splits it synchronously under this admission's guards.
    #[cfg_attr(
        not(feature = "test-support"),
        expect(
            dead_code,
            reason = "the inactive connection owner is not integrated yet"
        )
    )]
    pub(crate) async fn start_socket(
        self,
        socket: WebSocket,
        limits: TransportLimits,
        retirement_deadline: Duration,
    ) -> Result<
        (
            IoCustodianHandle,
            WebSocketInput<axum::Error>,
            WebSocketOutput<axum::Error>,
        ),
        SocketAdmissionError,
    > {
        self.start_socket_with_hook(
            socket,
            limits,
            retirement_deadline,
            None,
            || std::future::ready(()),
            |_| std::future::ready(()),
        )
        .await
    }

    async fn start_socket_with_hook<AfterSplitHook, AfterSplitFuture, Hook, HookFuture>(
        self,
        socket: WebSocket,
        limits: TransportLimits,
        retirement_deadline: Duration,
        demand: Option<Arc<ReadDemand>>,
        after_split: AfterSplitHook,
        hook: Hook,
    ) -> Result<
        (
            IoCustodianHandle,
            WebSocketInput<axum::Error>,
            WebSocketOutput<axum::Error>,
        ),
        SocketAdmissionError,
    >
    where
        AfterSplitHook: FnOnce() -> AfterSplitFuture + Send,
        AfterSplitFuture: Future<Output = ()> + Send,
        Hook: FnMut(ReservedStartStage) -> HookFuture + Send,
        HookFuture: Future<Output = ()> + Send,
    {
        let Self {
            authority,
            reservation,
        } = self;
        let mut socket = Some(socket);
        // The exact source and lifecycle guards cover the only physical split.
        // On rejection retain the admission reservation until the
        // unsplit upgraded socket has completed its bounded Close attempt.
        let split = authority.with_start_authority(|| {
            let Some(socket) = socket.take() else {
                unreachable!("one admission owns one socket")
            };
            let (writer, reader) = socket.split();
            (DemandGated::new(reader, demand.clone()), writer)
        });
        let (reader, writer) = match split {
            Ok(halves) => halves,
            Err(error) => {
                if let Some(mut socket) = socket {
                    #[cfg(feature = "test-support")]
                    let close_probe = authority
                        .actor_write_probe
                        .lock()
                        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                        .clone();
                    let _ = tokio::time::timeout(retirement_deadline, async {
                        #[cfg(feature = "test-support")]
                        if let Some(probe) = close_probe {
                            probe.before_close().await;
                        }
                        let _ = socket.send(Message::Close(None)).await;
                        let _ = socket.close().await;
                    })
                    .await;
                }
                drop(reservation);
                return Err(error);
            }
        };
        #[cfg(feature = "test-support")]
        let close_probe = authority
            .actor_write_probe
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .clone();
        let parts = UnstartedSocketParts::new(
            Arc::clone(&authority),
            reservation,
            reader,
            writer,
            retirement_deadline,
            true,
            #[cfg(feature = "test-support")]
            close_probe,
        );
        after_split().await;
        let (custodian, mut input, output) = Self::start_with_parts(
            parts,
            limits,
            retirement_deadline,
            axum_send_after_close,
            hook,
        )
        .await?;
        input.demand = demand;
        Ok((custodian, input, output))
    }
}

/// Non-cloneable admission for the single-task WebSocket loop. Until the loop
/// starts its pumps the reservation is a capacity hold that is released when
/// the owner drops it. Authority-bound writes use the captured source revision
/// and lifecycle generation at their synchronous enqueue boundary.
pub(crate) struct LegacySocketAdmission {
    authority: Arc<TransportAuthority>,
    permit: NodeAdmissionPermit,
    reservation: Option<IoCustodianReservation>,
}

/// The supervised transport of one live connection.
pub(crate) struct LiveSocketTransport {
    /// The only controlling handle is inside `output`: dropping the output
    /// stops both pumps. This observer does not keep them running.
    pub(crate) observer: IoCustodianObserver,
    /// Keeps the connection's inventory slot until the owner has finished its
    /// own cleanup. See [`LiveSocketCleanup`].
    pub(crate) cleanup: LiveSocketCleanup,
    pub(crate) input: WebSocketInput<axum::Error>,
    pub(crate) output: WebSocketMessageSink<axum::Error>,
}

/// The owner's half of the inventory slot. Dropping it reports that the owner
/// holds nothing more for this connection, whether it settled, unwound or was
/// stopped without settling. The owner therefore drops its transport handles
/// first on every path: the live state declares this guard after its socket
/// halves, and settlement consumes the halves before the state is dropped. A
/// failed owner fences the node separately. The custodian releases the slot
/// only after the pumps have joined as well.
pub(crate) struct LiveSocketCleanup {
    guard: Option<crate::server::routes::websocket::supervised_io::AuxiliaryCleanupGuard>,
    /// Ends the marker task the custodian joins before it reports the
    /// connection joined.
    _owner_finished: Option<oneshot::Sender<()>>,
}

impl Drop for LiveSocketCleanup {
    fn drop(&mut self) {
        if let Some(guard) = self.guard.take() {
            guard.complete();
        }
    }
}

impl LegacySocketAdmission {
    /// Split the upgraded socket into supervised pumps that read on demand.
    ///
    /// The reservation moves into the pump custodian, which holds the
    /// inventory slot until the pumps have joined and the returned cleanup
    /// guard has been completed. On failure the socket has already had its
    /// bounded Close attempt.
    pub(crate) async fn start_socket(
        &mut self,
        socket: WebSocket,
        inventory: &IoCustodianInventory,
        limits: TransportLimits,
        retirement_deadline: Duration,
    ) -> Result<LiveSocketTransport, SocketAdmissionError> {
        let Some(mut reservation) = self.reservation.take() else {
            return Err(SocketAdmissionError::WrongReservation);
        };
        // From here the custodian waits for this guard and for one marker
        // task before it frees the slot, on success and on failure alike.
        let mut cleanup = LiveSocketCleanup {
            guard: Some(reservation.retain_until_auxiliary_cleanup()?),
            _owner_finished: None,
        };
        let admission = SocketAdmission {
            authority: Arc::clone(&self.authority),
            reservation,
        };
        let started = admission
            .start_socket_with_hook(
                socket,
                limits,
                retirement_deadline,
                Some(Arc::new(ReadDemand::default())),
                || std::future::ready(()),
                |_| std::future::ready(()),
            )
            .await;
        let (owner_finished, owner_running) = oneshot::channel::<()>();
        let registered = inventory.register_auxiliary_task(
            &self.authority.connection_ref,
            tokio::spawn(async move {
                let _ = owner_running.await;
            }),
        );
        match started {
            Ok((custodian, input, output)) => {
                // Without the marker the custodian would never report this
                // connection joined, so its slot could not be freed.
                registered?;
                cleanup._owner_finished = Some(owner_finished);
                Ok(LiveSocketTransport {
                    observer: custodian.observer(),
                    cleanup,
                    input,
                    output: output.into_message_sink(),
                })
            }
            // A start that failed before its reservation became an indexed
            // entry has already returned the slot and has nothing to register
            // with. Otherwise the marker ends here with `owner_finished`.
            Err(error) => Err(error),
        }
    }

    pub(crate) fn revalidate(&self) -> Result<(), SocketAdmissionError> {
        self.authority.with_start_authority(|| ())
    }

    pub(crate) fn connection_ref(&self) -> &ConnectionRef {
        &self.authority.connection_ref
    }

    pub(crate) fn permit(&self) -> &NodeAdmissionPermit {
        &self.permit
    }

    pub(crate) async fn identity_revoked(&self) {
        self.authority.source.revoked().await;
    }
}

/// Test-only synchronization at the HTTP upgrade boundaries.
#[cfg(feature = "test-support")]
pub struct AdmissionTestBarrier {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}

#[cfg(feature = "test-support")]
impl Default for AdmissionTestBarrier {
    fn default() -> Self {
        Self {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}

#[cfg(feature = "test-support")]
impl AdmissionTestBarrier {
    pub async fn wait_until_entered(&self) {
        self.entered
            .acquire()
            .await
            .expect("test barrier open")
            .forget();
    }

    pub fn release(&self) {
        self.release.add_permits(1);
    }

    pub(crate) async fn arrive(&self) {
        self.entered.add_permits(1);
        self.release
            .acquire()
            .await
            .expect("test barrier open")
            .forget();
    }
}

/// Barriers are absent in release builds and inert unless a test installs them.
#[cfg(feature = "test-support")]
#[derive(Default)]
pub struct AdmissionTestBarriers {
    pub before_parse: Option<Arc<AdmissionTestBarrier>>,
    pub before_response: Option<Arc<AdmissionTestBarrier>>,
    pub in_callback: Option<Arc<AdmissionTestBarrier>>,
    pub in_inbound_handler: Option<Arc<AdmissionTestBarrier>>,
    pub before_cleanup: Option<Arc<AdmissionTestBarrier>>,
    pub after_cleanup: Option<Arc<AdmissionTestBarrier>>,
    pub owner_panic_active: Option<Arc<AdmissionTestBarrier>>,
    pub owner_panic_caught: Option<Arc<AdmissionTestBarrier>>,
    pub owner_panic_fenced: Option<Arc<AdmissionTestBarrier>>,
    /// Panics the owner at the start of settlement, once released.
    pub owner_panic_settling: Option<Arc<AdmissionTestBarrier>>,
    pub callback_waiter: Option<Arc<CallbackWaiterTestControl>>,
    pub live_actor: Option<Arc<LiveActorTestTap>>,
}

/// Test-only reach into the actor hosting a live connection. The owner task
/// installs the operations when it starts the actor; the actor reference
/// itself stays inside them.
///
/// A state serves one tap to the first connection an actor hosts; later
/// connections of the same state are not observed.
#[cfg(feature = "test-support")]
#[derive(Default)]
pub struct LiveActorTestTap {
    ops: std::sync::OnceLock<LiveActorTestOps>,
    hosted: CancellationToken,
    pings_handled: std::sync::atomic::AtomicUsize,
    loop_events_handled: std::sync::atomic::AtomicUsize,
    /// Loop events handled before each ping, in the order pings were handled.
    loop_events_before_ping: Mutex<Vec<usize>>,
    /// Pings handled before each loop event, in the order events were handled.
    pings_before_loop_event: Mutex<Vec<usize>>,
    park_requested: std::sync::atomic::AtomicBool,
    park: AdmissionTestBarrier,
}

#[cfg(feature = "test-support")]
pub(crate) struct LiveActorTestOps {
    /// Offer this many no-op messages without waiting; returns how many the
    /// mailbox accepted.
    pub(crate) flood: Box<dyn Fn(usize) -> usize + Send + Sync>,
    /// Send one no-op message and wait for the actor to handle it.
    pub(crate) ping: Box<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>,
    pub(crate) kill: Box<dyn Fn() + Send + Sync>,
}

#[cfg(feature = "test-support")]
impl LiveActorTestTap {
    /// Returns whether this connection is the one the tap observes.
    pub(crate) fn install(&self, ops: LiveActorTestOps) -> bool {
        let installed = self.ops.set(ops).is_ok();
        if installed {
            self.hosted.cancel();
        }
        installed
    }

    pub(crate) fn note_ping_handled(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.loop_events_before_ping
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .push(self.loop_events_handled.load(SeqCst));
        self.pings_handled.fetch_add(1, SeqCst);
    }

    pub(crate) fn note_loop_event_handled(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.pings_before_loop_event
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .push(self.pings_handled.load(SeqCst));
        self.loop_events_handled.fetch_add(1, SeqCst);
    }

    /// Called by the hosted loop at the top of every turn. Returns whether
    /// the loop was parked here.
    pub(crate) async fn before_turn(&self) -> bool {
        let parked = self
            .park_requested
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        if parked {
            self.park.arrive().await;
        }
        parked
    }

    /// Stop the hosted loop at the top of its next turn and wait until it is
    /// there. One no-op message wakes the loop, so `pings_handled` grows by
    /// one. Sources that become ready while the loop is parked are all ready
    /// together when [`Self::resume`] lets it wait again, and that wait
    /// offers the mailbox.
    pub async fn park(&self) {
        self.park_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(self.flood(1), 1, "the wake-up message was refused");
        self.park.wait_until_entered().await;
    }

    pub fn resume(&self) {
        self.park.release();
    }

    pub fn loop_events_before_each_ping(&self) -> Vec<usize> {
        self.loop_events_before_ping
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .clone()
    }

    pub fn pings_before_each_loop_event(&self) -> Vec<usize> {
        self.pings_before_loop_event
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .clone()
    }

    pub async fn wait_until_hosted(&self) {
        self.hosted.cancelled().await;
    }

    /// Offer `count` no-op messages without waiting. Returns how many the
    /// mailbox accepted; zero before an actor is hosted.
    pub fn flood(&self, count: usize) -> usize {
        self.ops.get().map_or(0, |ops| (ops.flood)(count))
    }

    /// Whether the actor handled one no-op message.
    pub async fn ping(&self) -> bool {
        match self.ops.get() {
            Some(ops) => (ops.ping)().await,
            None => false,
        }
    }

    pub fn kill(&self) {
        if let Some(ops) = self.ops.get() {
            (ops.kill)();
        }
    }

    pub fn pings_handled(&self) -> usize {
        self.pings_handled.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Test-only control of the actual upgrade callback's observational waiter.
#[cfg(feature = "test-support")]
#[derive(Default)]
pub struct CallbackWaiterTestControl {
    pub drop_before_poll: bool,
    pub drop_waiter: CancellationToken,
    pub waiter_dropped: CancellationToken,
}

/// Test-only access to the same non-cloneable admission and generic factory.
#[cfg(feature = "test-support")]
pub struct TestSocketAdmission(SocketAdmission);

#[cfg(feature = "test-support")]
impl TestSocketAdmission {
    pub fn with_actor_write_probe(self, probe: Arc<ActorWriteProbe>) -> Self {
        self.0.install_actor_write_probe(probe);
        self
    }

    pub fn clustered(
        lifecycle: &NodeLifecycle,
        source: &SharedNodeIdentity,
        inventory: &IoCustodianInventory,
        process_stop: CancellationToken,
        live_interrupt: CancellationToken,
    ) -> Result<Self, SocketAdmissionError> {
        SocketAdmission::clustered(lifecycle, source, inventory, process_stop, live_interrupt)
            .map(Self)
    }

    pub fn standalone(
        lifecycle: &NodeLifecycle,
        inventory: &IoCustodianInventory,
        process_stop: CancellationToken,
        live_interrupt: CancellationToken,
    ) -> Result<Self, SocketAdmissionError> {
        SocketAdmission::standalone(lifecycle, inventory, process_stop, live_interrupt).map(Self)
    }

    /// Inject a separate process incarnation only in deterministic tests.
    pub fn standalone_with_identity(
        lifecycle: &NodeLifecycle,
        lease: NodeLeaseIdentity,
        inventory: &IoCustodianInventory,
        process_stop: CancellationToken,
        live_interrupt: CancellationToken,
    ) -> Result<Self, SocketAdmissionError> {
        SocketAdmission::standalone_with_identity(
            lifecycle,
            lease,
            inventory,
            process_stop,
            live_interrupt,
        )
        .map(Self)
    }

    pub fn connection_ref(&self) -> &ConnectionRef {
        self.0.connection_ref()
    }

    /// Exercise full-reference rejection; both unused slots are released.
    pub fn replace_reservation(
        self,
        reservation: IoCustodianReservation,
    ) -> Result<Self, SocketAdmissionError> {
        let SocketAdmission {
            authority,
            reservation: previous,
        } = self.0;
        drop(previous);
        SocketAdmission::bind(authority, reservation).map(Self)
    }

    pub async fn start_socket(
        self,
        socket: WebSocket,
        limits: TransportLimits,
        retirement_deadline: Duration,
    ) -> Result<
        (
            IoCustodianHandle,
            WebSocketInput<axum::Error>,
            WebSocketOutput<axum::Error>,
        ),
        SocketAdmissionError,
    > {
        self.0
            .start_socket(socket, limits, retirement_deadline)
            .await
    }
}

/// Test-only barrier immediately before the source/lifecycle-fenced sink
/// attempt. It can model a noncooperative synchronous pump poll.
#[cfg(feature = "test-support")]
pub struct ActorWriteProbe {
    state: Mutex<ActorWriteProbeState>,
    release: std::sync::Condvar,
    entered: tokio::sync::watch::Sender<u64>,
    attempts: std::sync::atomic::AtomicU64,
    close_armed: std::sync::atomic::AtomicBool,
    close_entered: tokio::sync::watch::Sender<bool>,
    close_release: tokio::sync::Semaphore,
    after_attempt_armed: std::sync::atomic::AtomicBool,
    after_attempt_entered: tokio::sync::watch::Sender<bool>,
    after_attempt_release: tokio::sync::Semaphore,
    after_start_state: Mutex<ActorWriteProbeState>,
    after_start_release: std::sync::Condvar,
    after_start_entered: tokio::sync::watch::Sender<u64>,
}

#[cfg(feature = "test-support")]
#[derive(Default)]
struct ActorWriteProbeState {
    armed: bool,
    released: bool,
    entries: u64,
}

#[cfg(feature = "test-support")]
impl Default for ActorWriteProbe {
    fn default() -> Self {
        let (entered, _) = tokio::sync::watch::channel(0);
        let (close_entered, _) = tokio::sync::watch::channel(false);
        let (after_attempt_entered, _) = tokio::sync::watch::channel(false);
        let (after_start_entered, _) = tokio::sync::watch::channel(0);
        Self {
            state: Mutex::new(ActorWriteProbeState::default()),
            release: std::sync::Condvar::new(),
            entered,
            attempts: std::sync::atomic::AtomicU64::new(0),
            close_armed: std::sync::atomic::AtomicBool::new(false),
            close_entered,
            close_release: tokio::sync::Semaphore::new(0),
            after_attempt_armed: std::sync::atomic::AtomicBool::new(false),
            after_attempt_entered,
            after_attempt_release: tokio::sync::Semaphore::new(0),
            after_start_state: Mutex::new(ActorWriteProbeState::default()),
            after_start_release: std::sync::Condvar::new(),
            after_start_entered,
        }
    }
}

#[cfg(feature = "test-support")]
impl ActorWriteProbe {
    pub fn pause_next(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        state.armed = true;
        state.released = false;
    }

    pub async fn wait_for_entry(&self) {
        let mut entered = self.entered.subscribe();
        let _ = entered.wait_for(|entries| *entries > 0).await;
    }

    pub fn release(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        state.released = true;
        self.release.notify_all();
    }

    pub fn pause_close(&self) {
        self.close_armed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub async fn wait_for_close(&self) {
        let mut entered = self.close_entered.subscribe();
        let _ = entered.wait_for(|entered| *entered).await;
    }

    pub fn release_close(&self) {
        self.close_release.add_permits(1);
    }

    async fn before_close(&self) {
        if self
            .close_armed
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.close_entered.send_replace(true);
            if let Ok(permit) = self.close_release.acquire().await {
                permit.forget();
            }
        }
    }

    pub fn attempts(&self) -> u64 {
        self.attempts.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn pause_after_attempt(&self) {
        self.after_attempt_armed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub async fn wait_for_after_attempt(&self) {
        let mut entered = self.after_attempt_entered.subscribe();
        let _ = entered.wait_for(|entered| *entered).await;
    }

    pub fn release_after_attempt(&self) {
        self.after_attempt_release.add_permits(1);
    }

    /// Pause after `start_send` returns but before its receipt is marked
    /// attempted. This exposes the otherwise narrow fast-ACK race in tests.
    pub fn pause_after_start_send(&self) {
        let mut state = self
            .after_start_state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        state.armed = true;
        state.released = false;
    }

    pub async fn wait_for_after_start_send(&self) {
        let mut entered = self.after_start_entered.subscribe();
        let _ = entered.wait_for(|entries| *entries > 0).await;
    }

    pub fn release_after_start_send(&self) {
        let mut state = self
            .after_start_state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        state.released = true;
        self.after_start_release.notify_all();
    }

    fn after_start_send(&self) {
        let mut state = self
            .after_start_state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if !state.armed {
            return;
        }
        state.armed = false;
        state.entries += 1;
        self.after_start_entered.send_replace(state.entries);
        while !state.released {
            state = self
                .after_start_release
                .wait(state)
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        }
    }

    async fn after_attempt(&self) {
        if self
            .after_attempt_armed
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.after_attempt_entered.send_replace(true);
            if let Ok(permit) = self.after_attempt_release.acquire().await {
                permit.forget();
            }
        }
    }

    fn before_attempt(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if !state.armed {
            return;
        }
        state.armed = false;
        state.entries += 1;
        self.entered.send_replace(state.entries);
        while !state.released {
            state = self
                .release
                .wait(state)
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        }
    }

    fn record_attempt(&self) {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

struct TransportAuthority {
    connection_ref: ConnectionRef,
    source: IdentitySource,
    permit: NodeAdmissionPermit,
    process_stop: CancellationToken,
    live_interrupt: CancellationToken,
    terminal_started: CancellationToken,
    phase: RwLock<TransportPhase>,
    #[cfg(feature = "test-support")]
    actor_write_probe: Mutex<Option<Arc<ActorWriteProbe>>>,
    #[cfg(feature = "test-support")]
    actor_reaper_exit:
        Mutex<Option<crate::server::routes::websocket::supervised_io::ReservedReaperExitPoint>>,
}

impl TransportAuthority {
    fn with_start_authority<R>(
        &self,
        effect: impl FnOnce() -> R,
    ) -> Result<R, SocketAdmissionError> {
        self.source
            .with_current(|| {
                self.permit.with_current(|| {
                    if self.process_stop.is_cancelled() || self.live_interrupt.is_cancelled() {
                        return Err(SocketAdmissionError::Cancelled);
                    }
                    Ok(effect())
                })?
            })
            .ok_or(SocketAdmissionError::IdentityRevoked)?
    }

    async fn startup_revoked(&self) {
        tokio::select! {
            biased;
            _ = self.source.revoked() => {},
            _ = self.permit.revoked() => {},
            _ = self.process_stop.cancelled() => {},
            _ = self.live_interrupt.cancelled() => {},
        }
    }

    fn enter_terminal(&self, requested: TransportPhase) -> bool {
        let mut phase = self
            .phase
            .write()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if *phase == TransportPhase::Open {
            *phase = requested;
            self.terminal_started.cancel();
        }
        *phase == requested
    }

    fn phase(&self) -> TransportPhase {
        *self
            .phase
            .read()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }
}

struct FrameAuthority {
    transport: Arc<TransportAuthority>,
    mode: FrameMode,
    actor: Option<(Arc<ActorOutboundCustody>, ActorOutboundId)>,
}

impl WriteAuthority for FrameAuthority {
    fn revoked(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            match self.mode {
                FrameMode::Data => tokio::select! {
                    biased;
                    _ = self.transport.source.revoked() => {},
                    _ = self.transport.permit.revoked() => {},
                    _ = self.transport.process_stop.cancelled() => {},
                    _ = self.transport.live_interrupt.cancelled() => {},
                    _ = self.transport.terminal_started.cancelled() => {},
                },
                FrameMode::LiveDetach => tokio::select! {
                    biased;
                    _ = self.transport.source.revoked() => {},
                    _ = self.transport.permit.revoked() => {},
                    _ = self.transport.process_stop.cancelled() => {},
                },
                FrameMode::Shutdown | FrameMode::TransportClose => {
                    std::future::pending::<()>().await;
                }
            }
        })
    }

    fn attempt(
        &self,
        connection_ref: &ConnectionRef,
        stop: &StopSignal,
        attempt: Box<dyn FnOnce() + '_>,
    ) -> bool {
        if connection_ref != &self.transport.connection_ref {
            return false;
        }
        match self.mode {
            FrameMode::Data | FrameMode::LiveDetach => {
                #[cfg(feature = "test-support")]
                let probe = self
                    .transport
                    .actor_write_probe
                    .lock()
                    .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                    .clone();
                #[cfg(feature = "test-support")]
                if let Some(probe) = &probe {
                    probe.before_attempt();
                }
                // The process and live-interruption tokens are independently
                // cancellable, so those checks remain best effort. Source,
                // node generation and the first pump stop fence the sink
                // enqueue. Lock source, node, stop, then transport phase; no
                // guard survives an await or enters actor/effect-owner locks.
                let result = self.transport.source.with_current(|| {
                    self.transport.permit.start_send_if_current(|| {
                        stop.start_send_if_running(|| {
                            let phase = self
                                .transport
                                .phase
                                .read()
                                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
                            let phase_matches = matches!(
                                (self.mode, *phase),
                                (FrameMode::Data, TransportPhase::Open)
                                    | (FrameMode::LiveDetach, TransportPhase::LiveDetach)
                            );
                            if !phase_matches
                                || self.transport.process_stop.is_cancelled()
                                || (matches!(self.mode, FrameMode::Data)
                                    && self.transport.live_interrupt.is_cancelled())
                            {
                                return Err(());
                            }
                            if let Some((custody, id)) = &self.actor {
                                custody
                                    .attempt_with(id, || {
                                        #[cfg(feature = "test-support")]
                                        if let Some(probe) = &probe {
                                            probe.record_attempt();
                                        }
                                        attempt();
                                        #[cfg(feature = "test-support")]
                                        if let Some(probe) = &probe {
                                            probe.after_start_send();
                                        }
                                    })
                                    .map_or(Err(()), |_| Ok(()))
                            } else {
                                #[cfg(feature = "test-support")]
                                if let Some(probe) = &probe {
                                    probe.record_attempt();
                                }
                                attempt();
                                #[cfg(feature = "test-support")]
                                if let Some(probe) = &probe {
                                    probe.after_start_send();
                                }
                                Ok(())
                            }
                        })
                        .unwrap_or(Err(()))
                    })
                });
                matches!(result, Some(Ok(Ok(()))))
            }
            FrameMode::Shutdown | FrameMode::TransportClose => {
                let phase = self
                    .transport
                    .phase
                    .read()
                    .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
                let phase_matches = matches!(
                    (self.mode, *phase),
                    (FrameMode::Shutdown, TransportPhase::Shutdown)
                        | (FrameMode::TransportClose, TransportPhase::Shutdown)
                        | (FrameMode::TransportClose, TransportPhase::LiveDetach)
                        | (FrameMode::TransportClose, TransportPhase::CloseOnly)
                );
                if !phase_matches {
                    return false;
                }
                attempt();
                true
            }
        }
    }

    fn completed(&self) {
        if let Some((custody, id)) = &self.actor {
            custody.mark_flushed(id);
        }
    }

    #[cfg(feature = "test-support")]
    fn after_attempt(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let probe = self
                .transport
                .actor_write_probe
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                .clone();
            if let Some(probe) = probe {
                probe.after_attempt().await;
            }
        })
    }
}

/// An outbound frame bound to the exact transport that constructed it.
/// Its contents and authority mode have no public unchecked constructor.
pub struct WebSocketOutboundFrame {
    frame: Message,
    authority: Arc<FrameAuthority>,
}

impl WebSocketOutboundFrame {
    fn new(frame: Message, transport: &Arc<TransportAuthority>, mode: FrameMode) -> Self {
        Self {
            frame,
            authority: Arc::new(FrameAuthority {
                transport: Arc::clone(transport),
                mode,
                actor: None,
            }),
        }
    }

    #[expect(dead_code, reason = "the actor-owned endpoint does not use this yet")]
    fn actor_data(
        frame: Message,
        transport: &Arc<TransportAuthority>,
        custody: Arc<ActorOutboundCustody>,
        id: ActorOutboundId,
    ) -> Self {
        Self {
            frame,
            authority: Arc::new(FrameAuthority {
                transport: Arc::clone(transport),
                mode: FrameMode::Data,
                actor: Some((custody, id)),
            }),
        }
    }
}

/// Local write progress tied to the exact physical connection that submitted it.
/// No milestone proves that the peer processed or acknowledged the frame.
#[derive(Debug, Clone)]
pub struct WebSocketWriteReceipt {
    connection_ref: ConnectionRef,
    receipt: WriteReceipt,
}

impl WebSocketWriteReceipt {
    /// Physical connection that owns this frame's sink operation.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// Whether this receipt belongs to the supplied exact physical connection.
    pub fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        &self.connection_ref == connection_ref
    }

    /// Current local progress; a completed flush is not a peer acknowledgement.
    pub fn progress(&self) -> WriteProgress {
        self.receipt.progress()
    }

    /// Wait until a sink attempt or an earlier terminal outcome is observable.
    pub async fn wait_for_attempt(&self) -> WriteProgress {
        self.receipt.wait_for_attempt().await
    }

    /// Wait for a final local write outcome.
    pub async fn wait_for_terminal(&self) -> WriteProgress {
        self.receipt.wait_for_terminal().await
    }
}

/// A bounded output sink and its nonblocking, receipt-bearing submission API.
pub struct WebSocketOutput<E> {
    sender: WriteSender<Message>,
    custodian: IoCustodianHandle,
    authority: Arc<TransportAuthority>,
    error: Arc<Mutex<Option<Arc<E>>>>,
    pending: Option<BoxFuture<'static, Result<OutstandingWrite, AdmissionError>>>,
    outstanding: VecDeque<OutstandingWrite>,
    waiting: Option<BoxFuture<'static, WriteProgress>>,
    close_wait: Option<BoxFuture<'static, CustodianState>>,
    close_timer: Option<Pin<Box<tokio::time::Sleep>>>,
    close_deadline: Duration,
    max_bytes: usize,
    close_submitted: bool,
    closing: bool,
}

struct OutstandingWrite {
    receipt: WriteReceipt,
    terminal: bool,
}

impl<E> WebSocketOutput<E>
where
    E: Send + Sync + 'static,
{
    pub fn connection_ref(&self) -> &ConnectionRef {
        self.custodian.connection_ref()
    }

    /// Bind an ordinary data or WebSocket control frame to this connection's
    /// admission generation, process stop, and live-detach interruption.
    pub fn data_frame(&self, frame: Message) -> Result<WebSocketOutboundFrame, OutputError<E>> {
        if self.authority.phase() != TransportPhase::Open || matches!(frame, Message::Close(_)) {
            return Err(OutputError::InvalidFrame);
        }
        Ok(WebSocketOutboundFrame::new(
            frame,
            &self.authority,
            FrameMode::Data,
        ))
    }

    #[expect(dead_code, reason = "the actor-owned endpoint does not use this yet")]
    pub(crate) fn actor_data_frame(
        &self,
        frame: Message,
        custody: Arc<ActorOutboundCustody>,
        id: ActorOutboundId,
    ) -> Result<WebSocketOutboundFrame, OutputError<E>> {
        if custody.connection() != self.connection_ref() || id.connection() != self.connection_ref()
        {
            return Err(OutputError::WrongConnection);
        }
        if self.authority.phase() != TransportPhase::Open || matches!(frame, Message::Close(_)) {
            return Err(OutputError::InvalidFrame);
        }
        Ok(WebSocketOutboundFrame::actor_data(
            frame,
            &self.authority,
            custody,
            id,
        ))
    }

    /// Stop further ordinary data before constructing fixed shutdown frames.
    pub fn begin_shutdown(&mut self) -> Result<(), OutputError<E>> {
        if self.authority.enter_terminal(TransportPhase::Shutdown) {
            Ok(())
        } else {
            Err(OutputError::InvalidFrame)
        }
    }

    /// Stop ordinary data before constructing the fixed conflict close. The
    /// resulting frames still require the node generation and process token.
    pub fn begin_live_detach(&mut self) -> Result<(), OutputError<E>> {
        if self.authority.enter_terminal(TransportPhase::LiveDetach) {
            Ok(())
        } else {
            Err(OutputError::InvalidFrame)
        }
    }

    /// Fixed system-shutdown stream error, available only in shutdown mode.
    pub fn shutdown_error_frame(&self) -> Result<WebSocketOutboundFrame, OutputError<E>> {
        if self.authority.phase() != TransportPhase::Shutdown {
            return Err(OutputError::InvalidFrame);
        }
        Ok(WebSocketOutboundFrame::new(
            fixed_terminal_text(FixedTerminalText::SystemShutdown),
            &self.authority,
            FrameMode::Shutdown,
        ))
    }

    /// Fixed conflict stream error, available only for live detach.
    pub fn conflict_error_frame(&self) -> Result<WebSocketOutboundFrame, OutputError<E>> {
        if self.authority.phase() != TransportPhase::LiveDetach {
            return Err(OutputError::InvalidFrame);
        }
        Ok(WebSocketOutboundFrame::new(
            fixed_terminal_text(FixedTerminalText::Conflict),
            &self.authority,
            FrameMode::LiveDetach,
        ))
    }

    /// Fixed XMPP framing close, subject to the current terminal mode.
    pub fn framing_close_frame(&self) -> Result<WebSocketOutboundFrame, OutputError<E>> {
        let mode = match self.authority.phase() {
            TransportPhase::Shutdown => FrameMode::Shutdown,
            TransportPhase::LiveDetach => FrameMode::LiveDetach,
            _ => return Err(OutputError::InvalidFrame),
        };
        Ok(WebSocketOutboundFrame::new(
            fixed_terminal_text(FixedTerminalText::FramingClose),
            &self.authority,
            mode,
        ))
    }

    /// Enter terminal mode if needed and create a WebSocket Close frame.
    pub fn transport_close_frame(
        &mut self,
        close: Option<axum::extract::ws::CloseFrame>,
    ) -> Result<WebSocketOutboundFrame, OutputError<E>> {
        if self.authority.phase() == TransportPhase::Open {
            self.authority.enter_terminal(TransportPhase::CloseOnly);
        }
        Ok(WebSocketOutboundFrame::new(
            Message::Close(close),
            &self.authority,
            FrameMode::TransportClose,
        ))
    }

    /// Wait only for bounded queue admission and return this frame's receipt.
    /// Pin and keep the future across `select!` turns; dropping an unfinished
    /// admission drops its still-unadmitted frame. Poll its receipt separately
    /// while continuing to service inbound protocol frames and lifecycle control.
    /// Terminal frames use this same bounded queue; select their admission
    /// against the owner's shutdown deadline and independent stop control.
    pub async fn submit(
        &mut self,
        frame: WebSocketOutboundFrame,
    ) -> Result<WebSocketWriteReceipt, OutputError<E>> {
        self.validate_frame(&frame)?;
        let terminal = !matches!(frame.authority.mode, FrameMode::Data);
        let close = matches!(frame.frame, Message::Close(_));
        if terminal {
            if self.closing || self.close_submitted {
                return Err(OutputError::Closing);
            }
            poll_fn(|cx| self.poll_ready_terminal(cx, close)).await?;
        } else {
            poll_fn(|cx| self.poll_ready_inner(cx)).await?;
        }
        let size = frame_len_for_limit(&frame.frame);
        let receipt = self
            .sender
            .send_wait_fenced(frame.frame, size, frame.authority)
            .await
            .map_err(OutputError::Admission)?;
        self.close_submitted |= close;
        self.outstanding.push_back(OutstandingWrite {
            receipt: receipt.clone(),
            terminal,
        });
        Ok(WebSocketWriteReceipt {
            connection_ref: self.custodian.connection_ref().clone(),
            receipt,
        })
    }

    fn validate_frame(&self, frame: &WebSocketOutboundFrame) -> Result<(), OutputError<E>> {
        if !Arc::ptr_eq(&self.authority, &frame.authority.transport) {
            return Err(OutputError::WrongConnection);
        }
        Ok(())
    }

    fn error_or_interrupted(&self, progress: WriteProgress) -> OutputError<E> {
        let error = match self.error.lock() {
            Ok(error) => error.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        match error {
            Some(error) => OutputError::Transport(error),
            None => OutputError::Interrupted(progress),
        }
    }

    fn poll_pending(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), OutputError<E>>> {
        if let Some(pending) = &mut self.pending {
            match pending.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(receipt)) => self.outstanding.push_back(receipt),
                Poll::Ready(Err(error)) => {
                    self.pending = None;
                    return Poll::Ready(Err(OutputError::Admission(error)));
                }
            }
            self.pending = None;
        }
        Poll::Ready(Ok(()))
    }

    fn poll_ready_inner(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), OutputError<E>>> {
        if self.closing || self.close_submitted {
            return Poll::Ready(Err(OutputError::Closing));
        }
        std::task::ready!(self.poll_pending(cx))?;
        while let Some(item) = self.outstanding.front() {
            match item.receipt.progress() {
                WriteProgress::Completed => {
                    self.outstanding.pop_front();
                    self.waiting = None;
                }
                WriteProgress::Uncertain | WriteProgress::NotAttempted => {
                    return Poll::Ready(Err(self.error_or_interrupted(item.receipt.progress())));
                }
                WriteProgress::Admitted | WriteProgress::Attempted => break,
            }
        }
        Poll::Ready(Ok(()))
    }

    fn poll_ready_terminal(
        &mut self,
        cx: &mut Context<'_>,
        transport_close: bool,
    ) -> Poll<Result<(), OutputError<E>>> {
        std::task::ready!(self.poll_pending(cx))?;
        while let Some(item) = self.outstanding.front() {
            match item.receipt.progress() {
                WriteProgress::Completed => {}
                WriteProgress::NotAttempted | WriteProgress::Uncertain
                    if !item.terminal || transport_close => {}
                WriteProgress::NotAttempted | WriteProgress::Uncertain => {
                    return Poll::Ready(Err(self.error_or_interrupted(item.receipt.progress())));
                }
                WriteProgress::Admitted | WriteProgress::Attempted => break,
            }
            self.outstanding.pop_front();
            self.waiting = None;
        }
        Poll::Ready(Ok(()))
    }

    fn poll_flush_inner(
        &mut self,
        cx: &mut Context<'_>,
        retiring: bool,
    ) -> Poll<Result<(), OutputError<E>>> {
        std::task::ready!(self.poll_pending(cx))?;
        while let Some(item) = self.outstanding.front() {
            if self.waiting.is_none() {
                let receipt = item.receipt.clone();
                self.waiting = Some(Box::pin(async move { receipt.wait_for_terminal().await }));
            }
            let progress = match self.waiting.as_mut() {
                Some(waiting) => std::task::ready!(waiting.as_mut().poll(cx)),
                None => return Poll::Ready(Err(OutputError::RetirementFailed)),
            };
            self.waiting = None;
            match progress {
                WriteProgress::Completed => {
                    self.outstanding.pop_front();
                }
                _ if retiring && !item.terminal => {
                    self.outstanding.pop_front();
                }
                _ => return Poll::Ready(Err(self.error_or_interrupted(progress))),
            }
        }
        Poll::Ready(Ok(()))
    }
}

/// The `Sink` interface is for ordinary data frames only. Use `submit` for
/// fixed terminal frames so earlier revoked data cannot block cleanup.
impl<E> Sink<WebSocketOutboundFrame> for WebSocketOutput<E>
where
    E: Send + Sync + 'static,
{
    type Error = OutputError<E>;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut().poll_ready_inner(cx)
    }

    fn start_send(self: Pin<&mut Self>, frame: WebSocketOutboundFrame) -> Result<(), Self::Error> {
        let this = self.get_mut();
        if this.closing || this.close_submitted || this.pending.is_some() {
            return Err(OutputError::Closing);
        }
        this.validate_frame(&frame)?;
        if !matches!(frame.authority.mode, FrameMode::Data) {
            return Err(OutputError::InvalidFrame);
        }
        let size = frame_len_for_limit(&frame.frame);
        if size > this.max_bytes || u32::try_from(size).is_err() {
            return Err(OutputError::Admission(AdmissionError::Overloaded));
        }
        let sender = this.sender.clone();
        this.pending = Some(Box::pin(async move {
            sender
                .send_wait_fenced(frame.frame, size, frame.authority)
                .await
                .map(|receipt| OutstandingWrite {
                    receipt,
                    terminal: false,
                })
        }));
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut().poll_flush_inner(cx, false)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let timer = this
            .close_timer
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(this.close_deadline)));
        if timer.as_mut().poll(cx).is_ready() {
            // A blocking poll cannot be aborted until it returns. The indexed
            // reaper retains both handles, and this error makes no join claim.
            this.custodian.request_stop(StopReason::ShutdownDeadline);
            return Poll::Ready(Err(OutputError::RetirementFailed));
        }
        let retiring = this.authority.phase() != TransportPhase::Open;
        std::task::ready!(this.poll_flush_inner(cx, retiring))?;
        if !this.closing {
            this.closing = true;
            if this.close_submitted {
                this.custodian.request_graceful_after_local_close();
            } else {
                this.custodian.request_graceful_shutdown();
            }
            let observer = this.custodian.observer();
            this.close_wait = Some(Box::pin(async move { observer.wait_until_joined().await }));
        }
        let state = match this.close_wait.as_mut() {
            Some(wait) => std::task::ready!(wait.as_mut().poll(cx)),
            None => return Poll::Ready(Err(OutputError::RetirementFailed)),
        };
        if state != CustodianState::Joined
            || this.custodian.stop_reason() == Some(StopReason::ShutdownDeadline)
        {
            return Poll::Ready(Err(OutputError::RetirementFailed));
        }
        if let Some(summary) = this.custodian.terminal_summary() {
            if matches!(
                summary.outbound,
                OutboundTerminal::ShutdownClose | OutboundTerminal::PeerClosedReplyFlushed
            ) {
                return Poll::Ready(Ok(()));
            }
        }
        Poll::Ready(Err(this.error_or_interrupted(WriteProgress::NotAttempted)))
    }
}

impl<E> WebSocketOutput<E>
where
    E: Send + Sync + 'static,
{
    /// Present this output as a plain `Sink<Message>`. See
    /// [`WebSocketMessageSink`] for how frames are classified.
    pub fn into_message_sink(self) -> WebSocketMessageSink<E> {
        WebSocketMessageSink { output: self }
    }

    /// Park one frame for bounded admission. The caller has already checked
    /// readiness for the frame's class and that no admission is parked.
    fn start_queued(&mut self, frame: WebSocketOutboundFrame) -> Result<(), OutputError<E>> {
        self.validate_frame(&frame)?;
        let terminal = !matches!(frame.authority.mode, FrameMode::Data);
        let close = is_close(&frame.frame);
        let size = frame_len_for_limit(&frame.frame);
        if size > self.max_bytes || u32::try_from(size).is_err() {
            return Err(OutputError::Admission(AdmissionError::Overloaded));
        }
        let sender = self.sender.clone();
        self.pending = Some(Box::pin(async move {
            sender
                .send_wait_fenced(frame.frame, size, frame.authority)
                .await
                .map(|receipt| OutstandingWrite { receipt, terminal })
        }));
        self.close_submitted |= close;
        Ok(())
    }
}

/// Which readiness rule applies to a classified frame.
#[derive(Clone, Copy)]
enum MessageClass {
    Data,
    Terminal { close: bool },
}

/// A `Sink<Message>` over a supervised output, for an owner that writes one
/// frame at a time and treats a completed flush as a completed socket write.
///
/// `poll_flush` returns `Ok` only after the pump has flushed every frame this
/// sink started, so `SinkExt::send` keeps the meaning it has on a split
/// socket. A frame the pump refuses or cannot finish is a flush error.
///
/// Frames are classified by content, because the caller has no typed way to
/// name a terminal frame:
///
/// - `Message::Close` becomes the transport close and ends ordinary data.
/// - The fixed system-shutdown stream error moves the output into shutdown
///   mode, where it and the framing close that follows are written even after
///   the node generation or process token is revoked. Any other frame is then
///   refused.
/// - Every other frame is ordinary data and carries the usual authority.
pub struct WebSocketMessageSink<E> {
    output: WebSocketOutput<E>,
}

impl<E> WebSocketMessageSink<E>
where
    E: Send + Sync + 'static,
{
    pub fn connection_ref(&self) -> &ConnectionRef {
        self.output.connection_ref()
    }

    fn classify(
        &mut self,
        frame: Message,
    ) -> Result<(WebSocketOutboundFrame, MessageClass), OutputError<E>> {
        if let Message::Close(close) = frame {
            let frame = self.output.transport_close_frame(close)?;
            return Ok((frame, MessageClass::Terminal { close: true }));
        }
        match self.output.authority.phase() {
            TransportPhase::Open
                if frame == fixed_terminal_text(FixedTerminalText::SystemShutdown) =>
            {
                self.output.begin_shutdown()?;
                let frame = self.output.shutdown_error_frame()?;
                Ok((frame, MessageClass::Terminal { close: false }))
            }
            TransportPhase::Open => Ok((self.output.data_frame(frame)?, MessageClass::Data)),
            TransportPhase::Shutdown
                if frame == fixed_terminal_text(FixedTerminalText::FramingClose) =>
            {
                let frame = self.output.framing_close_frame()?;
                Ok((frame, MessageClass::Terminal { close: false }))
            }
            TransportPhase::CloseOnly => Err(OutputError::Closing),
            TransportPhase::Shutdown | TransportPhase::LiveDetach => Err(OutputError::InvalidFrame),
        }
    }

    /// Classify and park one frame. No admission may be parked.
    fn start(&mut self, frame: Message) -> Result<(), OutputError<E>> {
        if self.output.closing || self.output.close_submitted || self.output.pending.is_some() {
            return Err(OutputError::Closing);
        }
        let (frame, class) = self.classify(frame)?;
        // With no parked admission these checks only inspect receipts that
        // already exist, so they never register the waker.
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        let ready = match class {
            MessageClass::Data => self.output.poll_ready_inner(&mut cx),
            MessageClass::Terminal { close } => self.output.poll_ready_terminal(&mut cx, close),
        };
        match ready {
            Poll::Ready(result) => result?,
            Poll::Pending => return Err(OutputError::Closing),
        }
        self.output.start_queued(frame)
    }
}

impl<E> Sink<Message> for WebSocketMessageSink<E>
where
    E: Send + Sync + 'static,
{
    type Error = OutputError<E>;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        if this.output.closing || this.output.close_submitted {
            return Poll::Ready(Err(OutputError::Closing));
        }
        this.output.poll_pending(cx)
    }

    fn start_send(self: Pin<&mut Self>, frame: Message) -> Result<(), Self::Error> {
        self.get_mut().start(frame)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let retiring = this.output.authority.phase() != TransportPhase::Open;
        this.output.poll_flush_inner(cx, retiring)
    }

    /// Write the transport close if none was sent, then wait for its flush.
    /// This does not wait for the peer's reply or for the pumps to join; the
    /// owner retires the custodian separately.
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        std::task::ready!(this.output.poll_pending(cx))?;
        if !this.output.close_submitted {
            this.start(Message::Close(None))?;
        }
        this.output.poll_flush_inner(cx, true)
    }
}

fn frame_len_for_limit(frame: &Message) -> usize {
    match frame {
        Message::Text(text) => text.len(),
        Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes) => bytes.len(),
        Message::Close(None) => 0,
        Message::Close(Some(frame)) => frame.reason.len().saturating_add(2),
    }
}

fn is_close(frame: &Message) -> bool {
    matches!(frame, Message::Close(_))
}

fn axum_send_after_close(error: &axum::Error) -> bool {
    use std::error::Error;
    matches!(
        error
            .source()
            .and_then(|source| source.downcast_ref::<tungstenite::Error>()),
        Some(tungstenite::Error::Protocol(
            tungstenite::error::ProtocolError::SendAfterClosing
        ))
    )
}

struct RevocationBridge(tokio::task::JoinHandle<()>);

impl Drop for RevocationBridge {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl SocketAdmission {
    #[cfg(feature = "test-support")]
    async fn start_with_factory<
        R,
        W,
        ReadError,
        WriteError,
        Classify,
        Factory,
        FactoryFuture,
        Hook,
        HookFuture,
    >(
        self,
        limits: TransportLimits,
        retirement_deadline: Duration,
        is_send_after_close: Classify,
        factory: Factory,
        hook: Hook,
    ) -> Result<
        (
            IoCustodianHandle,
            WebSocketInput<ReadError>,
            WebSocketOutput<WriteError>,
        ),
        SocketAdmissionError,
    >
    where
        R: Stream<Item = Result<Message, ReadError>> + Unpin + Send + 'static,
        W: Sink<Message, Error = WriteError> + Unpin + Send + 'static,
        ReadError: Send + 'static,
        WriteError: Send + Sync + 'static,
        Classify: Fn(&WriteError) -> bool + Send + Sync + 'static,
        Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
        FactoryFuture: Future<Output = (R, W)> + Send,
        Hook: FnMut(ReservedStartStage) -> HookFuture + Send,
        HookFuture: Future<Output = ()> + Send,
    {
        let Self {
            authority,
            reservation,
        } = self;
        if !reservation.is_for(&authority.connection_ref) {
            return Err(SocketAdmissionError::WrongReservation);
        }
        // Invoke the synchronous factory only while this exact source revision
        // and lifecycle generation are current. No guard crosses the await.
        let factory_future =
            authority.with_start_authority(|| factory(&authority.connection_ref))?;
        // Drop an uncompleted factory (including any already-split halves it
        // owns) before returning the reservation on startup cancellation.
        let factory_result = {
            tokio::pin!(factory_future);
            tokio::select! {
                biased;
                _ = authority.startup_revoked() => None,
                halves = &mut factory_future => Some(halves),
            }
        };
        let (reader, writer) = match factory_result {
            Some(halves) => halves,
            None => {
                drop(reservation);
                return Err(SocketAdmissionError::Cancelled);
            }
        };
        let parts = UnstartedSocketParts::new(
            Arc::clone(&authority),
            reservation,
            reader,
            writer,
            retirement_deadline,
            false,
            #[cfg(feature = "test-support")]
            None,
        );
        Self::start_with_parts(
            parts,
            limits,
            retirement_deadline,
            is_send_after_close,
            hook,
        )
        .await
    }

    async fn start_with_parts<R, W, ReadError, WriteError, Classify, Hook, HookFuture>(
        mut parts: UnstartedSocketParts<R, W>,
        limits: TransportLimits,
        retirement_deadline: Duration,
        is_send_after_close: Classify,
        hook: Hook,
    ) -> Result<
        (
            IoCustodianHandle,
            WebSocketInput<ReadError>,
            WebSocketOutput<WriteError>,
        ),
        SocketAdmissionError,
    >
    where
        R: Stream<Item = Result<Message, ReadError>> + Unpin + Send + 'static,
        W: Sink<Message, Error = WriteError> + Unpin + Send + 'static,
        ReadError: Send + 'static,
        WriteError: Send + Sync + 'static,
        Classify: Fn(&WriteError) -> bool + Send + Sync + 'static,
        Hook: FnMut(ReservedStartStage) -> HookFuture + Send,
        HookFuture: Future<Output = ()> + Send,
    {
        let authority = Arc::clone(&parts.authority);
        let Some(reservation) = parts.reservation.as_ref() else {
            return Err(SocketAdmissionError::WrongReservation);
        };
        if !reservation.is_for(&authority.connection_ref) {
            parts.close_bounded().await;
            return Err(SocketAdmissionError::WrongReservation);
        }
        if let Err(error) = authority.with_start_authority(|| ()) {
            parts.close_bounded().await;
            return Err(error);
        }
        let revocation = CancellationToken::new();
        let bridge_authority = Arc::clone(&authority);
        let bridge_token = revocation.clone();
        let (bridge_ready_tx, bridge_ready_rx) = oneshot::channel();
        let bridge = RevocationBridge(tokio::spawn(async move {
            if bridge_ready_rx.await.is_ok() {
                bridge_authority.startup_revoked().await;
                bridge_token.cancel();
            }
        }));
        let (terminal_tx, terminal_rx) = oneshot::channel();
        let error = Arc::new(Mutex::new(None));
        let factory_error = Arc::clone(&error);
        let config = ReservedIoStartConfig::new(
            limits,
            ShutdownProtocol::new(Message::Close(None), move |error: &Arc<WriteError>| {
                is_send_after_close(error.as_ref())
            }),
            frame_len_for_limit,
            is_close,
            retirement_deadline,
        );
        #[cfg(feature = "test-support")]
        let config = {
            let exit = *authority
                .actor_reaper_exit
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            match exit {
                Some(exit) => config.with_test_reaper_exit_point(exit),
                None => config,
            }
        };
        let Some(reservation) = parts.reservation.take() else {
            return Err(SocketAdmissionError::WrongReservation);
        };
        let Some(reader) = parts.reader.take() else {
            return Err(SocketAdmissionError::WrongReservation);
        };
        let Some(writer) = parts.writer.take() else {
            return Err(SocketAdmissionError::WrongReservation);
        };
        let mut bridge_ready_tx = Some(bridge_ready_tx);
        let mut hook = hook;
        let start_hook = move |stage| {
            if stage == ReservedStartStage::BeforeInboundSpawn {
                if let Some(ready) = bridge_ready_tx.take() {
                    let _ = ready.send(());
                }
            }
            hook(stage)
        };
        let (custodian, sender, frames) = start_reserved_supervised_io(
            reservation,
            config,
            move |_| {
                std::future::ready((
                    reader,
                    ErrorCapture {
                        inner: writer,
                        error: factory_error,
                    },
                ))
            },
            start_hook,
            Some(revocation.clone()),
            Some(terminal_tx),
        )
        .await?;
        // The starter's cancellation watch ends at handoff. Keep a separate weak
        // watch through terminal join without keeping the controlling handle alive.
        let watch = custodian.watch_transport_cancellation(revocation, retirement_deadline);
        tokio::spawn(async move {
            let _bridge = bridge;
            let _ = watch.await;
        });
        let input = WebSocketInput {
            connection_ref: authority.connection_ref.clone(),
            frames,
            terminal: terminal_rx,
            finished: false,
            demand: None,
            demand_outstanding: false,
            settled: None,
        };
        let output = WebSocketOutput {
            sender,
            custodian: custodian.clone(),
            authority,
            error,
            pending: None,
            outstanding: VecDeque::new(),
            waiting: None,
            close_wait: None,
            close_timer: None,
            close_deadline: retirement_deadline,
            max_bytes: limits.outbound().max_bytes(),
            close_submitted: false,
            closing: false,
        };
        Ok((custodian, input, output))
    }
}

/// Test-only generic factory for the same bound admission; unlike the real
/// socket constructor, this cannot prove the origin of the supplied halves.
#[cfg(feature = "test-support")]
pub async fn start_websocket_transport<
    R,
    W,
    ReadError,
    WriteError,
    Classify,
    Factory,
    FactoryFuture,
>(
    admission: TestSocketAdmission,
    limits: TransportLimits,
    retirement_deadline: Duration,
    is_send_after_close: Classify,
    factory: Factory,
) -> Result<
    (
        IoCustodianHandle,
        WebSocketInput<ReadError>,
        WebSocketOutput<WriteError>,
    ),
    SocketAdmissionError,
>
where
    R: Stream<Item = Result<Message, ReadError>> + Unpin + Send + 'static,
    W: Sink<Message, Error = WriteError> + Unpin + Send + 'static,
    ReadError: Send + 'static,
    WriteError: Send + Sync + 'static,
    Classify: Fn(&WriteError) -> bool + Send + Sync + 'static,
    Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
    FactoryFuture: Future<Output = (R, W)> + Send,
{
    admission
        .0
        .start_with_factory(
            limits,
            retirement_deadline,
            is_send_after_close,
            factory,
            |_| std::future::ready(()),
        )
        .await
}

/// Test-only barriers expose both pump handoff boundaries.
#[cfg(feature = "test-support")]
pub async fn start_websocket_transport_with_hook<
    R,
    W,
    ReadError,
    WriteError,
    Classify,
    Factory,
    FactoryFuture,
    Hook,
    HookFuture,
>(
    admission: TestSocketAdmission,
    limits: TransportLimits,
    retirement_deadline: Duration,
    is_send_after_close: Classify,
    factory: Factory,
    hook: Hook,
) -> Result<
    (
        IoCustodianHandle,
        WebSocketInput<ReadError>,
        WebSocketOutput<WriteError>,
    ),
    SocketAdmissionError,
>
where
    R: Stream<Item = Result<Message, ReadError>> + Unpin + Send + 'static,
    W: Sink<Message, Error = WriteError> + Unpin + Send + 'static,
    ReadError: Send + 'static,
    WriteError: Send + Sync + 'static,
    Classify: Fn(&WriteError) -> bool + Send + Sync + 'static,
    Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
    FactoryFuture: Future<Output = (R, W)> + Send,
    Hook: FnMut(ReservedStartStage) -> HookFuture + Send,
    HookFuture: Future<Output = ()> + Send,
{
    admission
        .0
        .start_with_factory(
            limits,
            retirement_deadline,
            is_send_after_close,
            factory,
            hook,
        )
        .await
}

#[cfg(feature = "test-support")]
pub fn axum_send_after_close_for_test(error: &axum::Error) -> bool {
    axum_send_after_close(error)
}
