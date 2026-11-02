//! Test-support-only connection actor proving single ownership of connection
//! protocol state and its exact reserved physical pump pair.
//!
//! This is a dark vertical slice. It does not replace the production
//! WebSocket loop, perform authentication or publish a registry binding. The
//! fixture begins with non-resumable XEP-0198 state so it can exercise the
//! existing handled-prefix tracker and the typed `<r/>` → `<a h='N'/>` path.

use super::state::WsConnState;
use super::supervised_io::{
    AdmissionError, CustodianInventoryError, CustodianState, IoCustodianHandle,
    IoCustodianInventory, IoCustodianObserver, QueueItem, ReservedIoStartConfig,
    ReservedStartError, StopReason, WriteSender,
};
use crate::server::routes::interpret::OrderedRelayInboundSequence;
use kameo::actor::{ActorRef, Spawn};
use kameo::mailbox;
use kameo::message::{Context, Message};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::stream_management::{SmAck, SmRequest};

const MAX_HANDLED_INBOUND_OPERATIONS: usize = 256;

/// Typed input and output carried by the isolated connection transport.
/// Serialization stays in the caller's transport sink adapter.
#[derive(Debug, Clone, Copy)]
pub enum ConnectionWireFrame {
    /// A peer requested an XEP-0198 acknowledgment.
    RequestAcknowledgement(SmRequest),
    /// An XEP-0198 acknowledgment authored by the connection actor.
    Acknowledgement(SmAck),
    /// Transport close marker used by the supervised writer.
    Close,
}

impl ConnectionWireFrame {
    /// Count payload bytes using the typed stanza serializer at the transport
    /// boundary. Close frames have no XMPP payload.
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::RequestAcknowledgement(_) => SmRequest::to_xml().len(),
            Self::Acknowledgement(ack) => ack.to_xml().len(),
            Self::Close => 0,
        }
    }

    /// Whether this is the WebSocket close marker used by the pump protocol.
    pub fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}

/// Failure to start the reserved physical pumps for a connection actor.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionActorStartError {
    /// Exact physical-connection reservation failed before the factory ran.
    #[error(transparent)]
    Inventory(#[from] CustodianInventoryError),
    /// The independently owned pump pair did not complete startup.
    #[error(transparent)]
    Pumps(#[from] ReservedStartError),
}

/// Start the test-only actor after reserving the exact connection identity
/// minted in its real [`WsConnState`]. The pump factory cannot run before this
/// reservation succeeds.
pub async fn start_connection_actor<
    R,
    W,
    ReadError,
    WriteError,
    Size,
    IsClose,
    IsSendAfterClose,
    Factory,
    FactoryFuture,
>(
    inventory: &IoCustodianInventory,
    node_lease: NodeLeaseIdentity,
    config: ReservedIoStartConfig<ConnectionWireFrame, IsSendAfterClose, Size, IsClose>,
    factory: Factory,
) -> Result<ConnectionActorController, ConnectionActorStartError>
where
    R: futures::Stream<Item = Result<ConnectionWireFrame, ReadError>> + Unpin + Send + 'static,
    W: futures::Sink<ConnectionWireFrame, Error = WriteError> + Unpin + Send + 'static,
    ReadError: Send + 'static,
    WriteError: Send + 'static,
    Size: Fn(&ConnectionWireFrame) -> usize + Send + 'static,
    IsClose: Fn(&ConnectionWireFrame) -> bool + Send + 'static,
    IsSendAfterClose: Fn(&WriteError) -> bool + Send + Sync + 'static,
    Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
    FactoryFuture: Future<Output = (R, W)> + Send,
{
    let mut state = WsConnState::new_for_node_lease(node_lease);
    state
        .sm_state
        .enable("dark-connection-actor".to_owned(), false, None);
    let connection_ref = state.connection_ref().clone();

    // This is the admission boundary: no factory, pump, or reaper exists
    // until the one physical identity owned by `state` has a slot.
    let reservation = inventory.reserve(connection_ref.clone())?;
    let (custodian, outbound, inbound) = reservation.start_with(config, factory).await?;
    let io_control = Arc::new(ConnectionActorIoControl {
        custodian,
        inventory: inventory.clone(),
        connection_ref: connection_ref.clone(),
    });
    let actor = ConnectionActor::spawn_with_mailbox(
        ConnectionActor {
            state,
            io_control: Arc::clone(&io_control),
            outbound,
        },
        mailbox::bounded(256),
    );
    actor.wait_for_startup().await;

    let (forwarded_tx, forwarded_rx) = watch::channel(0usize);
    let pump_actor = actor.clone();
    let input_forwarder = tokio::spawn(async move {
        forward_inbound_frames(inbound, pump_actor, forwarded_tx).await;
    });

    Ok(ConnectionActorController {
        actor,
        io_control,
        _input_forwarder: input_forwarder,
        inbound_forwarded: forwarded_rx,
    })
}

async fn forward_inbound_frames(
    mut inbound: mpsc::Receiver<QueueItem<ConnectionWireFrame>>,
    actor: ActorRef<ConnectionActor>,
    forwarded: watch::Sender<usize>,
) {
    let mut forwarded_count = 0usize;
    while let Some(item) = inbound.recv().await {
        if matches!(item.value, ConnectionWireFrame::RequestAcknowledgement(_)) {
            if actor
                .tell(ForwardedRequestAcknowledgement(item))
                .send()
                .await
                .is_err()
            {
                return;
            }
            forwarded_count = forwarded_count.saturating_add(1);
            forwarded.send_replace(forwarded_count);
        } else {
            drop(item);
        }
    }
}

struct ConnectionActorIoControl {
    custodian: IoCustodianHandle,
    inventory: IoCustodianInventory,
    connection_ref: ConnectionRef,
}

impl ConnectionActorIoControl {
    fn observer(&self) -> Option<IoCustodianObserver> {
        self.inventory.observe(&self.connection_ref)
    }

    fn request_stop(&self, reason: StopReason) {
        self.custodian.request_stop(reason);
    }
}

/// Addressable Kameo actor with the single mutable owner of connection state.
#[derive(kameo::Actor)]
pub struct ConnectionActor {
    state: WsConnState,
    io_control: Arc<ConnectionActorIoControl>,
    outbound: WriteSender<ConnectionWireFrame>,
}

impl Drop for ConnectionActor {
    fn drop(&mut self) {
        // Actor retirement is independent of mailbox traffic. The custodian
        // remains indexed and owns/join-reaps the actual pump handles.
        self.io_control.request_stop(StopReason::Requested);
    }
}

impl ConnectionActor {
    fn enqueue_acknowledgement(&self) -> Result<(), ConnectionActorError> {
        if !self.state.sm_state.enabled {
            return Err(ConnectionActorError::StreamManagementDisabled);
        }
        let ack = SmAck::new(self.state.sm_state.inbound_count());
        let byte_len = ack.to_xml().len();
        self.outbound
            .try_send(ConnectionWireFrame::Acknowledgement(ack), byte_len)
            .map(|_| ())
            .map_err(ConnectionActorError::from)
    }
}

/// Addressable actor handle plus an independent exact-identity stop and
/// observation path. The inventory observation survives actor retirement.
pub struct ConnectionActorController {
    actor: ActorRef<ConnectionActor>,
    io_control: Arc<ConnectionActorIoControl>,
    _input_forwarder: JoinHandle<()>,
    inbound_forwarded: watch::Receiver<usize>,
}

impl ConnectionActorController {
    /// Address of the sole actor that owns this connection's mutable state.
    pub fn actor(&self) -> &ActorRef<ConnectionActor> {
        &self.actor
    }

    /// Exact physical socket identity held by the actor and inventory.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.io_control.connection_ref
    }

    /// Reacquire independent observation by the exact connection identity.
    pub fn observer(&self) -> Option<IoCustodianObserver> {
        self.io_control.observer()
    }

    /// Interrupt physical pumps without relying on the actor data mailbox.
    pub fn request_stop(&self, reason: StopReason) {
        self.io_control.request_stop(reason);
    }

    /// First reason recorded by the independent pump owner.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.io_control.custodian.stop_reason()
    }

    /// Wait for the indexed custodian to join every pump it started.
    pub async fn wait_until_joined(&self) -> Option<CustodianState> {
        let observer = self.observer()?;
        Some(observer.wait_until_joined().await)
    }

    /// Wait until the inbound forwarder has placed this many retained queue
    /// items into the actor mailbox. This is a test-support synchronization
    /// point for proving that admission permits survive mailbox wait time.
    pub async fn wait_for_inbound_forwarded(&self, count: usize) -> bool {
        let mut forwarded = self.inbound_forwarded.clone();
        loop {
            if *forwarded.borrow_and_update() >= count {
                return true;
            }
            if forwarded.changed().await.is_err() {
                return false;
            }
        }
    }
}

impl Drop for ConnectionActorController {
    fn drop(&mut self) {
        // Controller retirement is a stop request, never join proof. Abort the
        // forwarding adapter so it cannot keep the actor address alive while
        // the independently indexed custodian stops and joins its pumps.
        self.io_control.request_stop(StopReason::Requested);
        self._input_forwarder.abort();
    }
}

/// Snapshot used to verify that the actor is the only state owner.
#[derive(Debug, Clone, PartialEq, Eq, kameo::Reply)]
pub struct ConnectionActorSnapshot {
    /// Exact physical connection identity.
    pub connection_ref: ConnectionRef,
    /// Current binding attempt, if a bind or resume attempt is active.
    pub binding_ref: Option<BindingRef>,
    /// XEP-0198 handled inbound prefix.
    pub handled_h: u32,
    /// Whether the isolated actor's SM state is active.
    pub stream_management_enabled: bool,
}

/// Opaque handle for one inbound operation reserved from the actor-owned
/// handled-prefix tracker.
#[derive(Debug, Clone, PartialEq, Eq, kameo::Reply)]
pub struct ConnectionActorSequence {
    connection_ref: ConnectionRef,
    binding_ref: Option<BindingRef>,
    sequence: OrderedRelayInboundSequence,
}

/// Actor-side failures while sequencing a control event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionActorError {
    /// The control event arrived before the fixture's XEP-0198 state was enabled.
    #[error("XEP-0198 is not enabled on this connection")]
    StreamManagementDisabled,
    /// Binding identity belongs to a different physical connection.
    #[error("binding attempt belongs to a different physical connection")]
    BindingConnectionMismatch,
    /// Binding transition cannot strand handled work from the current stream.
    #[error("binding transition has unresolved handled work")]
    BindingHasPendingHandledInbound,
    /// Completion belongs to a different physical connection.
    #[error("handled completion belongs to a different physical connection")]
    SequenceConnectionMismatch,
    /// Completion belongs to a retired or superseded binding attempt.
    #[error("handled completion belongs to a different binding attempt")]
    SequenceBindingMismatch,
    /// Completion token has already completed or was never reserved here.
    #[error("handled completion is not pending on this connection")]
    SequenceNotPending,
    /// The bounded handled-prefix window is full.
    #[error("handled-prefix window is full")]
    HandledInboundFull,
    /// The bounded writer queue rejected this output.
    #[error("outbound writer queue rejected the frame: {0:?}")]
    Admission(AdmissionError),
}

impl From<AdmissionError> for ConnectionActorError {
    fn from(error: AdmissionError) -> Self {
        Self::Admission(error)
    }
}

/// Reserve the next inbound handled-prefix sequence in the actor-owned state.
pub struct ReserveHandledInbound;

impl Message<ReserveHandledInbound> for ConnectionActor {
    type Reply = Result<ConnectionActorSequence, ConnectionActorError>;

    async fn handle(
        &mut self,
        _: ReserveHandledInbound,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.state.sm_inbound_completion.outstanding_count() >= MAX_HANDLED_INBOUND_OPERATIONS {
            return Err(ConnectionActorError::HandledInboundFull);
        }
        Ok(ConnectionActorSequence {
            connection_ref: self.state.connection_ref().clone(),
            binding_ref: self.state.binding_ref.clone(),
            sequence: self
                .state
                .sm_inbound_completion
                .reserve(&self.state.sm_state),
        })
    }
}

/// Complete one inbound operation; out-of-order completion cannot skip a hole.
pub struct CompleteHandledInbound(pub ConnectionActorSequence);

impl Message<CompleteHandledInbound> for ConnectionActor {
    type Reply = Result<u32, ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: CompleteHandledInbound,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if msg.0.connection_ref != *self.state.connection_ref() {
            return Err(ConnectionActorError::SequenceConnectionMismatch);
        }
        if msg.0.binding_ref != self.state.binding_ref {
            return Err(ConnectionActorError::SequenceBindingMismatch);
        }
        if !self.state.sm_inbound_completion.is_pending(msg.0.sequence) {
            return Err(ConnectionActorError::SequenceNotPending);
        }
        self.state
            .sm_inbound_completion
            .complete(msg.0.sequence, &mut self.state.sm_state);
        Ok(self.state.sm_state.inbound_count())
    }
}

/// Answer the peer's XEP-0198 `<r/>` from this actor's current handled prefix.
pub struct RequestAcknowledgement;

/// Actor-level result for enqueueing a typed XEP-0198 acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub struct AcknowledgementResult(pub Result<(), ConnectionActorError>);

impl Message<RequestAcknowledgement> for ConnectionActor {
    type Reply = AcknowledgementResult;

    async fn handle(
        &mut self,
        _: RequestAcknowledgement,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        AcknowledgementResult(self.enqueue_acknowledgement())
    }
}

/// Forward one inbound transport frame while retaining its count and byte
/// permits through actor mailbox waiting and handling.
pub struct ForwardedRequestAcknowledgement(QueueItem<ConnectionWireFrame>);

impl Message<ForwardedRequestAcknowledgement> for ConnectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ForwardedRequestAcknowledgement,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let item = msg.0;
        if matches!(item.value, ConnectionWireFrame::RequestAcknowledgement(_)) {
            let _ = self.enqueue_acknowledgement();
        }
        drop(item);
    }
}

/// Pause the actor mailbox on deterministic barriers in capacity tests.
pub struct PauseConnectionActor {
    /// Signaled after this message owns the actor execution slot.
    pub entered: oneshot::Sender<()>,
    /// Releases the actor to process its queued messages.
    pub release: oneshot::Receiver<()>,
}

impl Message<PauseConnectionActor> for ConnectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: PauseConnectionActor,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _ = msg.entered.send(());
        let _ = msg.release.await;
    }
}

/// Install one bind or resume attempt on this physical connection.
pub struct BindConnection(pub BindingRef);

impl Message<BindConnection> for ConnectionActor {
    type Reply = Result<(), ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: BindConnection,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if msg.0.connection_ref() != self.state.connection_ref() {
            return Err(ConnectionActorError::BindingConnectionMismatch);
        }
        if self.state.binding_ref.as_ref() != Some(&msg.0)
            && self.state.sm_inbound_completion.outstanding_count() > 0
        {
            return Err(ConnectionActorError::BindingHasPendingHandledInbound);
        }
        self.state.binding_ref = Some(msg.0);
        Ok(())
    }
}

/// Retire one exact binding attempt. A stale completion cannot clear a later
/// binding on the same physical socket.
pub struct RetireBinding(pub BindingRef);

impl Message<RetireBinding> for ConnectionActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RetireBinding,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.state.binding_ref.as_ref() == Some(&msg.0)
            && msg.0.connection_ref() == self.state.connection_ref()
            && self.state.sm_inbound_completion.outstanding_count() == 0
        {
            self.state.binding_ref = None;
            true
        } else {
            false
        }
    }
}

/// Read the current exact identity and handled-prefix state.
pub struct SnapshotConnection;

impl Message<SnapshotConnection> for ConnectionActor {
    type Reply = ConnectionActorSnapshot;

    async fn handle(
        &mut self,
        _: SnapshotConnection,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        ConnectionActorSnapshot {
            connection_ref: self.state.connection_ref().clone(),
            binding_ref: self.state.binding_ref.clone(),
            handled_h: self.state.sm_state.inbound_count(),
            stream_management_enabled: self.state.sm_state.enabled,
        }
    }
}

/// Deliberate panic used to prove that actor death leaves the pump owner
/// independently discoverable. This message is only exported by test support.
pub struct PanicConnectionActor;

impl Message<PanicConnectionActor> for ConnectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _: PanicConnectionActor,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        panic!("injected connection actor panic");
    }
}

/// Stop the actor normally, exercising its drop-time physical stop path.
pub struct StopConnectionActor;

impl Message<StopConnectionActor> for ConnectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _: StopConnectionActor,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        ctx.stop();
    }
}
