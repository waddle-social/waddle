use axum::extract::{ws::WebSocketUpgrade, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::serve::Listener;
use axum::Router;
use futures::{Sink, SinkExt, StreamExt};
use kameo::actor::Spawn;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use waddle_server::clustering::NodeLifecycle;
use waddle_server::test_support::actor_local_endpoint::{
    ActorEndpointCleanupIndex, ActorLocalEndpointError, ActorLocalEndpointOwner,
    ActorLocalPublishError, ActorLocalWriteError, ActorPublicationProbe,
    ConnectionActorDriverTestGate, ConnectionActorDriverTestSnapshot,
    ConnectionActorDriverTestUnknownReason,
};
use waddle_server::test_support::connection_actor::{
    start_connection_actor, start_connection_actor_with_effect, start_connection_actor_with_hooks,
    CompleteHandledInbound, CompleteHandledInboundAtLifecycleGate,
    CompleteHandledInboundBeforeLifecycleGate, ConfirmBindingInstall, ConnectionActorError,
    ConnectionActorGateBarrier, ConnectionActorOperationId, ConnectionActorStartBarrier,
    ConnectionActorStartError, ConnectionActorStartHooks, ConnectionWireFrame,
    CountableEffectDisposition, CountableInboundEffect, InstallBinding, OutputAdmissionPause,
    PanicConnectionActor, PauseConnectionActor, PrepareBinding, RequestAcknowledgement,
    ReserveHandledInbound, RetireBinding, SeedInboundCounterForTest, SnapshotConnection,
    StopConnectionActor,
};
use waddle_server::test_support::supervised_io::{
    AdmissionError, CustodianInventoryError, CustodianState, IoCustodianInventory, Limits,
    OutboundTerminal, ReservedIoStartConfig, ShutdownProtocol, StopReason, TransportLimits,
};
use waddle_server::test_support::websocket_state;
use waddle_server::websocket_transport::{
    ActorWriteProbe, TestSocketAdmission, WebSocketInput, WebSocketOutput,
};
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::ownership::{ConnectionRef, NodeIdentity, NodeLeaseIdentity, SharedNodeIdentity};
use waddle_xmpp::protocol::frame::{parse_frame_with_metadata, InboundFrame, ParseError};
use waddle_xmpp::registry::user_actor::{GetResources, HoldUserActorForPublication, UserActor};
use waddle_xmpp::registry::{
    ActorOutboundStage, ActorSocketOutboundEndpoint, ArchiveDispatchRefusalReason, ConnectionEntry,
    ConnectionRegistry, LocalRetryDispatchOutcome, LocalRetryRefusal, OutboundStanza,
    OutboundWriteAcceptance, TrySendDirectWithCustody,
};
use waddle_xmpp::stream_management::{SmAck, SmRequest, SmStanza};
use xmpp_parsers::minidom::Element;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const HANDLED_OPERATION_DEADLINE: Duration = Duration::from_secs(30);

fn new_operation_id() -> ConnectionActorOperationId {
    ConnectionActorOperationId::fresh().expect("operation ID counter remains available")
}

fn reserve_request() -> ReserveHandledInbound {
    ReserveHandledInbound::new(new_operation_id())
}

struct FlushGate {
    released: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

struct ReleaseAdmissionBarrier(Arc<ConnectionActorGateBarrier>);

impl Drop for ReleaseAdmissionBarrier {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl FlushGate {
    fn new() -> Self {
        Self {
            released: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }

    fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
        if let Ok(mut waker) = self.waker.lock() {
            if let Some(waker) = waker.take() {
                waker.wake();
            }
        }
    }

    fn register(&self, waker: &Waker) {
        if let Ok(mut current) = self.waker.lock() {
            *current = Some(waker.clone());
        }
    }
}

struct GatedFlushSink<S> {
    inner: S,
    gate: Arc<FlushGate>,
    entered: Option<oneshot::Sender<()>>,
    pending_flush: bool,
}

impl<S> Sink<ConnectionWireFrame> for GatedFlushSink<S>
where
    S: Sink<Message, Error = Error> + Unpin,
{
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, frame: ConnectionWireFrame) -> Result<(), Self::Error> {
        let message = match frame {
            ConnectionWireFrame::RequestAcknowledgement { .. } => {
                Message::Text(SmRequest::to_xml().into())
            }
            ConnectionWireFrame::Acknowledgement(ack) => Message::Text(ack.to_xml().into()),
            ConnectionWireFrame::CountableStanza { .. }
            | ConnectionWireFrame::RejectedInbound { .. }
            | ConnectionWireFrame::Prebind { .. } => {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "inbound-only frame cannot be written",
                )));
            }
            ConnectionWireFrame::Close => Message::Close(None),
        };
        self.pending_flush = true;
        Pin::new(&mut self.inner).start_send(message)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.pending_flush && !self.gate.is_released() {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            self.gate.register(cx.waker());
            return Poll::Pending;
        }
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                self.pending_flush = false;
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

type ClientSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start_actor(
    inventory: &IoCustodianInventory,
    max_outbound_count: usize,
    max_outbound_bytes: usize,
    gate: Arc<FlushGate>,
    retirement_deadline: Duration,
) -> (
    waddle_server::test_support::connection_actor::ConnectionActorController,
    ClientSocket,
    oneshot::Receiver<()>,
) {
    start_actor_with_inbound_limit(
        inventory,
        8,
        max_outbound_count,
        max_outbound_bytes,
        gate,
        retirement_deadline,
        HANDLED_OPERATION_DEADLINE,
    )
    .await
}

async fn start_actor_with_lifecycle_deadline(
    inventory: &IoCustodianInventory,
    max_outbound_count: usize,
    max_outbound_bytes: usize,
    gate: Arc<FlushGate>,
    lifecycle_deadline: Duration,
) -> (
    waddle_server::test_support::connection_actor::ConnectionActorController,
    ClientSocket,
    oneshot::Receiver<()>,
) {
    start_actor_with_inbound_limit(
        inventory,
        8,
        max_outbound_count,
        max_outbound_bytes,
        gate,
        TEST_TIMEOUT,
        lifecycle_deadline,
    )
    .await
}

async fn start_actor_with_inbound_limit(
    inventory: &IoCustodianInventory,
    max_inbound_count: usize,
    max_outbound_count: usize,
    max_outbound_bytes: usize,
    gate: Arc<FlushGate>,
    retirement_deadline: Duration,
    handled_operation_deadline: Duration,
) -> (
    waddle_server::test_support::connection_actor::ConnectionActorController,
    ClientSocket,
    oneshot::Receiver<()>,
) {
    start_actor_with_inbound_limits_and_effect(
        inventory,
        test_transport_limits(
            max_inbound_count,
            2048,
            max_outbound_count,
            max_outbound_bytes,
        ),
        gate,
        retirement_deadline,
        handled_operation_deadline,
        |_| async { CountableEffectDisposition::Handled },
    )
    .await
}

async fn start_actor_with_inbound_limits_and_effect<Effect, EffectFuture>(
    inventory: &IoCustodianInventory,
    limits: TransportLimits,
    gate: Arc<FlushGate>,
    retirement_deadline: Duration,
    handled_operation_deadline: Duration,
    effect: Effect,
) -> (
    waddle_server::test_support::connection_actor::ConnectionActorController,
    ClientSocket,
    oneshot::Receiver<()>,
)
where
    Effect: Fn(CountableInboundEffect) -> EffectFuture + Send + Sync + 'static,
    EffectFuture: std::future::Future<Output = CountableEffectDisposition> + Send + 'static,
{
    let (server, client) = super::pumps::socket_pair().await;
    let (entered_tx, entered_rx) = oneshot::channel();
    let config = ReservedIoStartConfig::new(
        limits,
        ShutdownProtocol::new(ConnectionWireFrame::Close, |error: &Error| {
            matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
        }),
        ConnectionWireFrame::encoded_len,
        ConnectionWireFrame::is_close,
        retirement_deadline,
    );
    let controller = start_connection_actor_with_effect(
        inventory,
        NodeLeaseIdentity::local_process(),
        handled_operation_deadline,
        config,
        move |_| async move {
            let (writer, reader) = server.split();
            let reader = reader
                .filter_map(|incoming| async move {
                    match incoming {
                        Ok(Message::Text(text)) => {
                            Some(Ok(connection_frame_from_text(text.as_str())))
                        }
                        Ok(Message::Binary(bytes)) => {
                            Some(Ok(ConnectionWireFrame::RejectedInbound {
                                wire_bytes: bytes.len(),
                            }))
                        }
                        Ok(Message::Close(_)) => Some(Ok(ConnectionWireFrame::Close)),
                        Ok(_) => None,
                        Err(error) => Some(Err(error)),
                    }
                })
                .boxed();
            let writer = GatedFlushSink {
                inner: writer,
                gate,
                entered: Some(entered_tx),
                pending_flush: false,
            };
            (reader, writer)
        },
        effect,
    )
    .await
    .expect("reserved connection actor starts");
    (controller, client, entered_rx)
}

async fn spawn_actor_start_with_hooks(
    inventory: IoCustodianInventory,
    hooks: ConnectionActorStartHooks,
) -> (
    tokio::task::JoinHandle<()>,
    ClientSocket,
    oneshot::Receiver<ConnectionRef>,
) {
    let (server, client) = super::pumps::socket_pair().await;
    let (connection_ref_tx, connection_ref_rx) = oneshot::channel();
    let (flush_entered_tx, _flush_entered_rx) = oneshot::channel();
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let config = ReservedIoStartConfig::new(
        test_transport_limits(2, 2048, 2, 256),
        ShutdownProtocol::new(ConnectionWireFrame::Close, |error: &Error| {
            matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
        }),
        ConnectionWireFrame::encoded_len,
        ConnectionWireFrame::is_close,
        Duration::from_millis(500),
    );
    let start_task = tokio::spawn(async move {
        let result = start_connection_actor_with_hooks(
            &inventory,
            NodeLeaseIdentity::local_process(),
            HANDLED_OPERATION_DEADLINE,
            config,
            move |connection_ref| {
                let _ = connection_ref_tx.send(connection_ref.clone());
                async move {
                    let (writer, reader) = server.split();
                    let reader = reader
                        .filter_map(|incoming| async move {
                            match incoming {
                                Ok(Message::Text(text)) => {
                                    Some(Ok(connection_frame_from_text(text.as_str())))
                                }
                                Ok(Message::Binary(bytes)) => {
                                    Some(Ok(ConnectionWireFrame::RejectedInbound {
                                        wire_bytes: bytes.len(),
                                    }))
                                }
                                Ok(Message::Close(_)) => Some(Ok(ConnectionWireFrame::Close)),
                                Ok(_) => None,
                                Err(error) => Some(Err(error)),
                            }
                        })
                        .boxed();
                    let writer = GatedFlushSink {
                        inner: writer,
                        gate,
                        entered: Some(flush_entered_tx),
                        pending_flush: false,
                    };
                    (reader, writer)
                }
            },
            |_| async { CountableEffectDisposition::Handled },
            hooks,
        )
        .await;
        drop(result);
    });
    (start_task, client, connection_ref_rx)
}

struct DropNotification(Option<oneshot::Sender<()>>);

impl Drop for DropNotification {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

struct BlockingDropStream<S> {
    inner: S,
    id: u8,
    dropped: mpsc::UnboundedSender<u8>,
    release: std_mpsc::Receiver<()>,
}

impl<S> Drop for BlockingDropStream<S> {
    fn drop(&mut self) {
        let _ = self.dropped.send(self.id);
        let _ = self.release.recv();
    }
}

impl<S: futures::Stream + Unpin> futures::Stream for BlockingDropStream<S> {
    type Item = S::Item;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

struct BlockingDropSink<S> {
    inner: S,
    id: u8,
    dropped: mpsc::UnboundedSender<u8>,
    release: std_mpsc::Receiver<()>,
}

impl<S> Drop for BlockingDropSink<S> {
    fn drop(&mut self) {
        let _ = self.dropped.send(self.id);
        let _ = self.release.recv();
    }
}

impl<S: Sink<Message> + Unpin> Sink<Message> for BlockingDropSink<S> {
    type Error = S::Error;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        Pin::new(&mut self.inner).start_send(item)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

fn test_transport_limits(
    inbound_count: usize,
    inbound_bytes: usize,
    outbound_count: usize,
    outbound_bytes: usize,
) -> TransportLimits {
    TransportLimits::new(
        Limits::new(inbound_count, inbound_bytes).expect("inbound bounds"),
        Limits::new(outbound_count, outbound_bytes).expect("outbound bounds"),
    )
}

fn connection_frame_from_text(text: &str) -> ConnectionWireFrame {
    let wire_bytes = text.len();
    match parse_frame_with_metadata(text) {
        Ok(parsed) => match parsed.frame {
            InboundFrame::Stanza(stanza) => ConnectionWireFrame::CountableStanza {
                stanza: Arc::new(*stanza),
                message_stanza_lang: parsed.message_stanza_lang,
                wire_bytes,
            },
            _ => ConnectionWireFrame::RejectedInbound { wire_bytes },
        },
        Err(ParseError::UnknownRoot(root)) if root == "r" => {
            if matches!(SmStanza::parse(text), Some(SmStanza::Request)) {
                ConnectionWireFrame::RequestAcknowledgement { wire_bytes }
            } else {
                ConnectionWireFrame::RejectedInbound { wire_bytes }
            }
        }
        _ => ConnectionWireFrame::RejectedInbound { wire_bytes },
    }
}

async fn read_ack(peer: &mut ClientSocket) -> SmAck {
    loop {
        let incoming = timeout(TEST_TIMEOUT, peer.next())
            .await
            .expect("peer receives acknowledgment")
            .expect("peer socket remains open")
            .expect("loopback WebSocket frame");
        if let Message::Text(xml) = incoming {
            if let Some(ack) = SmAck::parse(xml.as_str()) {
                return ack;
            }
        }
    }
}

async fn send_request(peer: &mut ClientSocket) {
    peer.send(Message::Text(SmRequest::to_xml().into()))
        .await
        .expect("send XEP-0198 request over loopback WebSocket");
}

async fn send_stanza(peer: &mut ClientSocket, xml: &str) {
    peer.send(Message::Text(xml.to_owned().into()))
        .await
        .expect("send countable XMPP stanza over loopback WebSocket");
}

fn message_with_body(body: &str) -> String {
    let stanza = Element::builder("message", waddle_xmpp::protocol::frame::CLIENT_STANZA_NS)
        .append(
            Element::builder("body", waddle_xmpp::protocol::frame::CLIENT_STANZA_NS)
                .append(body.to_owned())
                .build(),
        )
        .build();
    let mut bytes = Vec::new();
    stanza
        .write_to(&mut bytes)
        .expect("serialize typed stanza fixture");
    String::from_utf8(bytes).expect("serialized XML is UTF-8")
}

async fn actor_ack(
    controller: &waddle_server::test_support::connection_actor::ConnectionActorController,
) -> Result<(), ConnectionActorError> {
    controller
        .actor()
        .ask(RequestAcknowledgement)
        .send()
        .await
        .expect("connection actor remains addressable")
        .0
}

struct ActorOwnerDuplexListener(Option<DuplexStream>);

impl Listener for ActorOwnerDuplexListener {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.take() {
            Some(stream) => (stream, ()),
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(())
    }
}

type StartedActorOwner = (
    ActorLocalEndpointOwner,
    WebSocketInput<axum::Error>,
    WebSocketOutput<axum::Error>,
);

struct ActorOwnerRoute {
    lifecycle: NodeLifecycle,
    inventory: IoCustodianInventory,
    index: ActorEndpointCleanupIndex,
    write_probe: Option<Arc<ActorWriteProbe>>,
    started: mpsc::Sender<(
        ConnectionRef,
        Result<StartedActorOwner, ActorLocalEndpointError>,
    )>,
}

async fn actor_owner_route(
    State(route): State<Arc<ActorOwnerRoute>>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    let admission = TestSocketAdmission::standalone(
        &route.lifecycle,
        &route.inventory,
        tokio_util::sync::CancellationToken::new(),
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("standalone socket admission");
    let admission = match &route.write_probe {
        Some(probe) => admission.with_actor_write_probe(Arc::clone(probe)),
        None => admission,
    };
    let connection = admission.connection_ref().clone();
    upgrade
        .protocols(["xmpp"])
        .on_upgrade(move |socket| async move {
            let started = admission
                .start_actor_local_endpoint(
                    socket,
                    test_transport_limits(8, 2048, 2, 4096),
                    TEST_TIMEOUT,
                    &route.index,
                    2,
                    4096,
                )
                .await;
            let _ = route.started.send((connection, started)).await;
        })
}

struct ActorOwnerDriverRoute {
    lifecycle: NodeLifecycle,
    identity: Option<SharedNodeIdentity>,
    inventory: IoCustodianInventory,
    index: ActorEndpointCleanupIndex,
    retirement_deadline: Duration,
    write_probe: Option<Arc<ActorWriteProbe>>,
    started: mpsc::Sender<(
        ConnectionRef,
        Result<StartedActorOwner, ActorLocalEndpointError>,
    )>,
}

async fn actor_owner_driver_route(
    State(route): State<Arc<ActorOwnerDriverRoute>>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    let admission = match &route.identity {
        Some(identity) => TestSocketAdmission::clustered(
            &route.lifecycle,
            identity,
            &route.inventory,
            tokio_util::sync::CancellationToken::new(),
            tokio_util::sync::CancellationToken::new(),
        ),
        None => TestSocketAdmission::standalone(
            &route.lifecycle,
            &route.inventory,
            tokio_util::sync::CancellationToken::new(),
            tokio_util::sync::CancellationToken::new(),
        ),
    }
    .expect("socket admission");
    let admission = match &route.write_probe {
        Some(probe) => admission.with_actor_write_probe(Arc::clone(probe)),
        None => admission,
    };
    let connection = admission.connection_ref().clone();
    upgrade
        .protocols(["xmpp"])
        .on_upgrade(move |socket| async move {
            let started = admission
                .start_actor_local_endpoint(
                    socket,
                    test_transport_limits(8, 2048, 2, 4096),
                    route.retirement_deadline,
                    &route.index,
                    2,
                    4096,
                )
                .await;
            let _ = route.started.send((connection, started)).await;
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_cleanup_index_recovers_driver_after_caller_and_task_handles_drop() {
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let index = ActorEndpointCleanupIndex::new(1).expect("app cleanup inventory");
    let (started, mut ready) = mpsc::channel(1);
    let route = Arc::new(ActorOwnerDriverRoute {
        lifecycle: NodeLifecycle::new(),
        identity: None,
        inventory: inventory.clone(),
        index: index.clone(),
        retirement_deadline: TEST_TIMEOUT,
        write_probe: None,
        started,
    });
    let app = Router::new()
        .route("/actor-owner-driver", get(actor_owner_driver_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (_peer, _) =
        tokio_tungstenite::client_async("ws://localhost/actor-owner-driver", client_socket)
            .await
            .expect("real WebSocket upgrade");
    let (connection, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner event");
    let (owner, input, output) = started.expect("endpoint owner starts");
    let driver = owner
        .attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("driver attaches to exact endpoint");
    assert_eq!(owner.connection_ref(), &connection);
    let observer = inventory.observe(&connection).expect("physical observer");

    let gate = ConnectionActorDriverTestGate::default();
    let caller_ticket = driver.submit_paused(&gate).expect("operation admitted");
    let id = caller_ticket.id();
    timeout(TEST_TIMEOUT, gate.wait_until_entered())
        .await
        .expect("driver entered handler");
    drop(caller_ticket);
    drop(driver);

    let successor = ConnectionRef::fresh(connection.node_lease().clone());
    assert!(index.lookup_driver_for_test(&successor, id).is_none());
    assert!(index.contains(&connection));
    assert!(matches!(
        owner.attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        ),
        Err(ActorLocalEndpointError::DriverAlreadyAttached)
    ));

    gate.release();
    let recovered = index
        .lookup_driver_for_test(&connection, id)
        .expect("app-owned exact-ref ledger recovers after handle drop");
    assert_eq!(
        timeout(TEST_TIMEOUT, recovered.wait())
            .await
            .expect("operation completion is retained"),
        ConnectionActorDriverTestSnapshot::Completed
    );
    assert_eq!(recovered.run_count(), 1);
    assert!(
        index.contains(&connection),
        "unacknowledged result keeps its slot"
    );

    owner.request_stop(StopReason::Requested);
    assert!(matches!(
        owner.attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        ),
        Err(ActorLocalEndpointError::DriverCleanupStarted)
    ));
    drop(input);
    drop(output);
    drop(owner);
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("physical pumps join"),
        CustodianState::Joined
    );
    timeout(TEST_TIMEOUT, async {
        while !index.join_cleanup_finished_for_test(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("endpoint join task attempted its one prune before direct ticket ack");
    assert!(
        index.contains(&connection),
        "both joins retain unacknowledged result"
    );
    assert!(
        recovered.acknowledge(),
        "direct ticket acknowledgement settles custody"
    );
    timeout(TEST_TIMEOUT, async {
        while index.contains(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("direct ticket acknowledgement prunes the joined exact record");
    assert!(!index.acknowledge_driver_for_test(&connection, id));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attached_driver_binds_the_captured_identity_revision() {
    let lifecycle = NodeLifecycle::new();
    let identity = SharedNodeIdentity::new(NodeIdentity::new("actor-custody-test", "epoch-a"));
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let index = ActorEndpointCleanupIndex::new(1).expect("app cleanup inventory");
    let (started, mut ready) = mpsc::channel(1);
    let route = Arc::new(ActorOwnerDriverRoute {
        lifecycle: lifecycle.clone(),
        identity: Some(identity.clone()),
        inventory: inventory.clone(),
        index: index.clone(),
        retirement_deadline: TEST_TIMEOUT,
        write_probe: None,
        started,
    });
    let app = Router::new()
        .route("/actor-owner-driver", get(actor_owner_driver_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (_peer, _) =
        tokio_tungstenite::client_async("ws://localhost/actor-owner-driver", client_socket)
            .await
            .expect("real WebSocket upgrade");
    let (connection, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner event");
    let (owner, input, output) = started.expect("endpoint owner starts");
    let driver = owner
        .attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("driver attaches to exact endpoint");
    let observer = inventory.observe(&connection).expect("physical observer");
    let gate = ConnectionActorDriverTestGate::default();
    let active = driver.submit_paused(&gate).expect("operation admitted");
    timeout(TEST_TIMEOUT, gate.wait_until_entered())
        .await
        .expect("handler is in flight");

    identity
        .rotate(NodeIdentity::new("actor-custody-test", "epoch-b"))
        .await;
    assert!(
        lifecycle.is_ready(),
        "identity loss does not change Serving"
    );
    assert!(
        driver.submit_immediate().is_err(),
        "stale source rejects admission"
    );
    gate.release();
    assert_eq!(
        timeout(TEST_TIMEOUT, active.wait())
            .await
            .expect("in-flight operation settles under revoked identity"),
        ConnectionActorDriverTestSnapshot::CompletionUnknown(
            ConnectionActorDriverTestUnknownReason::AuthorityRevoked
        )
    );

    owner.request_stop(StopReason::LeaseLost);
    drop(input);
    drop(output);
    drop(owner);
    drop(driver);
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("physical pumps join"),
        CustodianState::Joined
    );
    assert!(index.acknowledge_driver_for_test(&connection, active.id()));
    timeout(TEST_TIMEOUT, async {
        while index.contains(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("revoked operation settlement releases the joined record");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pump_stop_starts_driver_deadline_while_other_pump_is_held() {
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let index = ActorEndpointCleanupIndex::new(1).expect("app cleanup inventory");
    let (started, mut ready) = mpsc::channel(1);
    let write_probe = Arc::new(ActorWriteProbe::default());
    let route = Arc::new(ActorOwnerDriverRoute {
        lifecycle: NodeLifecycle::new(),
        identity: None,
        inventory: inventory.clone(),
        index: index.clone(),
        retirement_deadline: Duration::from_millis(800),
        write_probe: Some(Arc::clone(&write_probe)),
        started,
    });
    let app = Router::new()
        .route("/actor-owner-driver", get(actor_owner_driver_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (mut peer, _) =
        tokio_tungstenite::client_async("ws://localhost/actor-owner-driver", client_socket)
            .await
            .expect("real WebSocket upgrade");
    let (connection, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner event");
    let (owner, input, mut output) = started.expect("endpoint owner starts");
    let driver = owner
        .attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("driver attaches to exact endpoint");
    let observer = inventory.observe(&connection).expect("physical observer");
    let gate = ConnectionActorDriverTestGate::default();
    let active = driver
        .submit_paused(&gate)
        .expect("active handler admitted");
    let queued = driver.submit_immediate().expect("queued handler admitted");
    let active_id = active.id();
    let queued_id = queued.id();
    timeout(TEST_TIMEOUT, gate.wait_until_entered())
        .await
        .expect("handler is held in flight");

    write_probe.pause_next();
    let frame = output
        .data_frame(axum::extract::ws::Message::Text(
            "held outbound write".into(),
        ))
        .expect("current data frame");
    let output_task = tokio::spawn(async move { output.submit(frame).await });
    timeout(TEST_TIMEOUT, write_probe.wait_for_entry())
        .await
        .expect("other pump is held inside its synchronous sink attempt");
    output_task
        .await
        .expect("output submit task joins")
        .expect("outbound frame is admitted");

    peer.send(Message::Close(None))
        .await
        .expect("peer initiates pump-triggered stop");
    let late_tickets = timeout(TEST_TIMEOUT, async {
        let mut tickets = Vec::new();
        loop {
            match driver.submit_immediate() {
                Ok(ticket) => {
                    tickets.push(ticket);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(_) => return tickets,
            }
        }
    })
    .await
    .expect("pump stop immediately seals driver admission");
    assert_eq!(observer.stop_reason(), Some(StopReason::PeerClosed));

    assert_eq!(
        timeout(TEST_TIMEOUT, active.wait())
            .await
            .expect("driver uses the stop-time deadline while pumps retire"),
        ConnectionActorDriverTestSnapshot::CompletionUnknown(
            ConnectionActorDriverTestUnknownReason::WorkerAborted
        )
    );
    assert_eq!(
        timeout(TEST_TIMEOUT, queued.wait())
            .await
            .expect("queued operation is classified after worker join"),
        ConnectionActorDriverTestSnapshot::NotStarted
    );
    assert!(late_tickets
        .iter()
        .all(|ticket| { ticket.snapshot() == ConnectionActorDriverTestSnapshot::NotStarted }));
    assert_ne!(
        observer.state(),
        CustodianState::Joined,
        "physical cleanup still waits for the held outbound pump"
    );
    assert!(index.contains(&connection), "both join proofs gate pruning");

    write_probe.release();
    drop(input);
    drop(owner);
    drop(driver);
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("physical pump cleanup completes after release"),
        CustodianState::Joined
    );
    timeout(TEST_TIMEOUT, async {
        while !index.join_cleanup_finished_for_test(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("endpoint cleanup task finishes its first prune attempt");
    assert!(index.acknowledge_driver_for_test(&connection, active_id));
    assert!(index.acknowledge_driver_for_test(&connection, queued_id));
    for ticket in late_tickets {
        assert!(index.acknowledge_driver_for_test(&connection, ticket.id()));
    }
    timeout(TEST_TIMEOUT, async {
        while index.contains(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("joined and acknowledged owner record prunes");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graceful_output_close_starts_driver_deadline_without_peer_reply() {
    let retirement_deadline = Duration::from_millis(400);
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let index = ActorEndpointCleanupIndex::new(1).expect("app cleanup inventory");
    let (started, mut ready) = mpsc::channel(1);
    let route = Arc::new(ActorOwnerDriverRoute {
        lifecycle: NodeLifecycle::new(),
        identity: None,
        inventory: inventory.clone(),
        index: index.clone(),
        retirement_deadline,
        write_probe: None,
        started,
    });
    let app = Router::new()
        .route("/actor-owner-driver", get(actor_owner_driver_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (_peer, _) =
        tokio_tungstenite::client_async("ws://localhost/actor-owner-driver", client_socket)
            .await
            .expect("real WebSocket upgrade");
    let (connection, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner event");
    let (owner, input, mut output) = started.expect("endpoint owner starts");
    let driver = owner
        .attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("driver attaches to exact endpoint");
    let observer = inventory.observe(&connection).expect("physical observer");
    let gate = ConnectionActorDriverTestGate::default();
    let active = driver
        .submit_paused(&gate)
        .expect("active handler admitted");
    timeout(TEST_TIMEOUT, gate.wait_until_entered())
        .await
        .expect("driver handler is stalled");

    let close = tokio::spawn(async move { output.close().await });
    let stopped_at = timeout(TEST_TIMEOUT, driver.wait_stopped())
        .await
        .expect("graceful close synchronously seals driver admission");
    assert!(driver.submit_immediate().is_err());

    assert_eq!(
        timeout(
            retirement_deadline + Duration::from_millis(250),
            active.wait()
        )
        .await
        .expect("driver uses the grace from graceful-close initiation"),
        ConnectionActorDriverTestSnapshot::CompletionUnknown(
            ConnectionActorDriverTestUnknownReason::WorkerAborted
        )
    );
    assert!(
        stopped_at.elapsed() < retirement_deadline + Duration::from_millis(250),
        "physical retirement must not add a second driver grace"
    );
    drop(_peer);
    drop(input);
    drop(owner);
    drop(driver);
    let _ = timeout(TEST_TIMEOUT, close).await;
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("physical pumps join after peer release"),
        CustodianState::Joined
    );
    timeout(TEST_TIMEOUT, async {
        while !index.join_cleanup_finished_for_test(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("endpoint cleanup finishes its prune attempt");
    assert!(index.acknowledge_driver_for_test(&connection, active.id()));
    timeout(TEST_TIMEOUT, async {
        while index.contains(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("settled endpoint record is pruned");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identity_only_revocation_arms_driver_deadline_while_lifecycle_serves() {
    let retirement_deadline = Duration::from_millis(160);
    let lifecycle = NodeLifecycle::new();
    let identity = SharedNodeIdentity::new(NodeIdentity::new("actor-custody-test", "epoch-a"));
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let index = ActorEndpointCleanupIndex::new(1).expect("app cleanup inventory");
    let (started, mut ready) = mpsc::channel(1);
    let route = Arc::new(ActorOwnerDriverRoute {
        lifecycle: lifecycle.clone(),
        identity: Some(identity.clone()),
        inventory: inventory.clone(),
        index: index.clone(),
        retirement_deadline,
        write_probe: None,
        started,
    });
    let app = Router::new()
        .route("/actor-owner-driver", get(actor_owner_driver_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (_peer, _) =
        tokio_tungstenite::client_async("ws://localhost/actor-owner-driver", client_socket)
            .await
            .expect("real WebSocket upgrade");
    let (connection, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner event");
    let (owner, input, output) = started.expect("endpoint owner starts");
    let driver = owner
        .attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("driver attaches to exact endpoint");
    let observer = inventory.observe(&connection).expect("physical observer");
    let gate = ConnectionActorDriverTestGate::default();
    let active = driver
        .submit_paused(&gate)
        .expect("active handler admitted");
    let queued = driver
        .submit_immediate()
        .expect("queued operation admitted");
    timeout(TEST_TIMEOUT, gate.wait_until_entered())
        .await
        .expect("driver handler is stalled");

    identity
        .rotate(NodeIdentity::new("actor-custody-test", "epoch-b"))
        .await;
    assert!(
        lifecycle.is_ready(),
        "only the identity revision was revoked"
    );
    assert!(
        driver.submit_immediate().is_err(),
        "stale permit rejects admission"
    );
    assert_eq!(
        timeout(
            retirement_deadline + Duration::from_millis(120),
            active.wait()
        )
        .await
        .expect("authority watcher starts the one configured grace"),
        ConnectionActorDriverTestSnapshot::CompletionUnknown(
            ConnectionActorDriverTestUnknownReason::WorkerAborted
        )
    );
    timeout(TEST_TIMEOUT, driver.wait_joined())
        .await
        .expect("deadline abort is joined by the independent driver supervisor");
    assert_eq!(
        queued.snapshot(),
        ConnectionActorDriverTestSnapshot::NotStarted
    );
    owner.request_stop(StopReason::LeaseLost);
    drop(input);
    drop(output);
    drop(owner);
    drop(driver);
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("physical pumps join after explicit owner stop"),
        CustodianState::Joined
    );
    timeout(TEST_TIMEOUT, async {
        while !index.join_cleanup_finished_for_test(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("endpoint cleanup finishes its prune attempt");
    assert!(index.acknowledge_driver_for_test(&connection, active.id()));
    assert!(index.acknowledge_driver_for_test(&connection, queued.id()));
    timeout(TEST_TIMEOUT, async {
        while index.contains(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("revoked operation settlement releases the joined record");
    drop(_peer);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_cleanup_deadline_retains_unknown_active_and_not_started_queued() {
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let index = ActorEndpointCleanupIndex::new(1).expect("app cleanup inventory");
    let (started, mut ready) = mpsc::channel(1);
    let route = Arc::new(ActorOwnerDriverRoute {
        lifecycle: NodeLifecycle::new(),
        identity: None,
        inventory: inventory.clone(),
        index: index.clone(),
        retirement_deadline: Duration::from_millis(25),
        write_probe: None,
        started,
    });
    let app = Router::new()
        .route("/actor-owner-driver", get(actor_owner_driver_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (_peer, _) =
        tokio_tungstenite::client_async("ws://localhost/actor-owner-driver", client_socket)
            .await
            .expect("real WebSocket upgrade");
    let (connection, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner event");
    let (owner, input, output) = started.expect("endpoint owner starts");
    let driver = owner
        .attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("driver attaches to exact endpoint");
    let observer = inventory.observe(&connection).expect("physical observer");

    let gate = ConnectionActorDriverTestGate::default();
    let active = driver
        .submit_paused(&gate)
        .expect("active operation accepted");
    let queued = driver
        .submit_immediate()
        .expect("queued operation accepted");
    let active_id = active.id();
    let queued_id = queued.id();
    timeout(TEST_TIMEOUT, gate.wait_until_entered())
        .await
        .expect("active handler stalls");
    drop(active);
    drop(queued);
    drop(driver);

    owner.request_stop(StopReason::ShutdownDeadline);
    drop(input);
    drop(output);
    drop(owner);
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("physical pumps join"),
        CustodianState::Joined
    );

    let active = index
        .lookup_driver_for_test(&connection, active_id)
        .expect("active operation remains in app custody");
    let queued = index
        .lookup_driver_for_test(&connection, queued_id)
        .expect("queued operation remains in app custody");
    timeout(TEST_TIMEOUT, active.wait())
        .await
        .expect("driver deadline abort completes its cleanup record");
    timeout(TEST_TIMEOUT, queued.wait())
        .await
        .expect("queued classification completes with the worker join");
    assert_eq!(
        active.snapshot(),
        ConnectionActorDriverTestSnapshot::CompletionUnknown(
            ConnectionActorDriverTestUnknownReason::WorkerAborted
        )
    );
    assert!(active.retains_input());
    assert_eq!(
        queued.snapshot(),
        ConnectionActorDriverTestSnapshot::NotStarted
    );
    assert!(index.acknowledge_driver_for_test(&connection, active_id));
    assert!(index.acknowledge_driver_for_test(&connection, queued_id));
    timeout(TEST_TIMEOUT, async {
        while index.contains(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("explicitly settled driver custody releases joined record");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_socket_actor_owner_keeps_exact_envelope_until_joined_terminal_settlement() {
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let index = ActorEndpointCleanupIndex::new(1).expect("terminal inventory");
    let (started, mut ready) = mpsc::channel(1);
    let route = Arc::new(ActorOwnerRoute {
        lifecycle: NodeLifecycle::new(),
        inventory: inventory.clone(),
        index: index.clone(),
        write_probe: None,
        started,
    });
    let app = Router::new()
        .route("/actor-owner", get(actor_owner_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (mut peer, _) =
        tokio_tungstenite::client_async("ws://localhost/actor-owner", client_socket)
            .await
            .expect("real WebSocket upgrade");
    let (_, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner started");
    let (mut owner, input, mut output) = started.expect("production physical start path");
    let connection = owner.connection_ref().clone();
    let (_, mut outsider_receiver, _) =
        ActorSocketOutboundEndpoint::channel(connection.clone(), 2, 4096)
            .expect("independent actor channel");
    assert!(matches!(
        owner
            .write_next_direct(&mut outsider_receiver, &mut output)
            .await,
        Err(ActorLocalWriteError::WrongOwner)
    ));
    let (failed_endpoint, failed_receiver) = owner.new_binding_channel().expect("first channel");
    let failed_binding = BindingRef::fresh(connection.clone());
    let jid: jid::FullJid = "alice@example.test/phone".parse().expect("full JID");
    let failed_candidate = ConnectionEntry::new_actor_provisional_for_binding(
        failed_endpoint,
        failed_binding.clone(),
        jid.clone(),
    )
    .expect("first owner channel and binding match");
    let registry = Arc::new(ConnectionRegistry::new());
    let actor = UserActor::spawn(UserActor::new(jid.to_bare()));
    assert!(matches!(
        owner
            .publish_binding(
                Arc::clone(&registry),
                actor.clone(),
                jid.clone(),
                failed_candidate,
                Some(waddle_xmpp::registry::LocalConnectionBindingId::fresh()),
                TEST_TIMEOUT,
            )
            .await,
        Err(ActorLocalPublishError::RegistryConflict)
    ));
    assert!(failed_receiver.custody().is_sealed());
    assert!(failed_receiver.custody().terminal_ids().is_some());
    assert!(registry.entry(&jid).is_none());
    drop(failed_receiver);
    let (endpoint, mut receiver) = timeout(TEST_TIMEOUT, async {
        loop {
            match owner.new_binding_channel() {
                Ok(channel) => break channel,
                Err(ActorLocalEndpointError::PriorBindingActive) => tokio::task::yield_now().await,
                Err(error) => panic!("fresh binding channel failed: {error}"),
            }
        }
    })
    .await
    .expect("failed binding retired before fresh channel");
    let binding = BindingRef::fresh(connection.clone());
    assert_ne!(binding, failed_binding);
    let candidate =
        ConnectionEntry::new_actor_provisional_for_binding(endpoint, binding, jid.clone())
            .expect("fresh owner channel and binding match");
    let publication_probe = Arc::new(ActorPublicationProbe::default());
    owner.install_publication_probe(Arc::clone(&publication_probe));
    let publishing_registry = Arc::clone(&registry);
    let publishing_actor = actor.clone();
    let publishing_jid = jid.clone();
    let publishing_candidate = candidate.clone();
    let publishing = tokio::spawn(async move {
        let result = owner
            .publish_binding(
                publishing_registry,
                publishing_actor,
                publishing_jid,
                publishing_candidate,
                None,
                TEST_TIMEOUT,
            )
            .await;
        (owner, result)
    });
    timeout(TEST_TIMEOUT, publication_probe.wait_for_mirror())
        .await
        .expect("both maps hold provisional candidate");
    let (refused_acceptance, mut refused_accepted) = OutboundWriteAcceptance::new();
    let refused_stanza = waddle_xmpp::Stanza::Message(xmpp_parsers::message::Message::new(None));
    let mut refused_outbound =
        OutboundStanza::with_write_acceptance(refused_stanza.clone(), refused_acceptance);
    refused_outbound.pending_row_id = Some(waddle_xmpp::pending_delivery::PendingRowId::new(
        "provisional-refusal-row",
    ));
    let LocalRetryDispatchOutcome::Refused(LocalRetryRefusal::Enqueue {
        owner: refused_owner,
        reason: ArchiveDispatchRefusalReason::Provisional,
        outbound: refused_outbound,
        ..
    }) = actor
        .ask(TrySendDirectWithCustody {
            jid: jid.clone(),
            outbound: refused_outbound,
        })
        .await
        .expect("provisional mirror refuses")
    else {
        panic!("expected exact provisional refusal");
    };
    assert_eq!(refused_owner, candidate.binding_id());
    assert_eq!(
        refused_outbound.stanza.to_element(),
        refused_stanza.to_element()
    );
    assert_eq!(
        refused_outbound
            .pending_row_id
            .as_ref()
            .map(|row| row.as_str()),
        Some("provisional-refusal-row")
    );
    assert!(refused_outbound.write_acceptance.is_some());
    assert!(matches!(
        refused_accepted.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(receiver.try_recv().is_err());
    publication_probe.release();
    let (mut owner, publication_result) = timeout(TEST_TIMEOUT, publishing)
        .await
        .expect("publication resumes")
        .expect("publication task returns");
    publication_result.expect("independently owned publication");
    assert!(matches!(
        owner
            .publish_binding(
                Arc::clone(&registry),
                actor.clone(),
                jid.clone(),
                candidate.clone(),
                None,
                TEST_TIMEOUT,
            )
            .await,
        Err(ActorLocalPublishError::RegistryConflict)
    ));
    assert_eq!(
        registry
            .entry(&jid)
            .expect("first publication remains")
            .binding_id(),
        candidate.binding_id()
    );

    let (acknowledgement, mut acknowledged) = OutboundWriteAcceptance::new();
    let stanza = waddle_xmpp::Stanza::Message(xmpp_parsers::message::Message::new(None));
    assert!(matches!(
        acknowledged.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(receiver.try_recv().is_err());
    let accepted = actor
        .ask(TrySendDirectWithCustody {
            jid: jid.clone(),
            outbound: OutboundStanza::with_write_acceptance(stanza.clone(), acknowledgement),
        })
        .await
        .expect("active dispatch");
    assert!(matches!(
        accepted,
        LocalRetryDispatchOutcome::AcceptedOrSettled
    ));
    let id = owner
        .write_next_direct(&mut receiver, &mut output)
        .await
        .expect("local direct write")
        .expect("one ID");
    assert_eq!(
        receiver.custody().stage(&id),
        Some(ActorOutboundStage::Flushed)
    );
    assert!(matches!(
        acknowledged.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    let frame = timeout(TEST_TIMEOUT, peer.next())
        .await
        .expect("peer read deadline")
        .expect("peer open")
        .expect("valid frame");
    let Message::Text(xml) = frame else {
        panic!("expected one direct XMPP frame");
    };
    let received: Element = xml.as_str().parse().expect("typed outbound XML");
    assert_eq!(received, stanza.to_element());

    drop(id);
    let observer = inventory.observe(&connection).expect("physical observer");
    owner.request_stop(StopReason::Requested);
    assert!(matches!(
        owner.new_binding_channel(),
        Err(ActorLocalEndpointError::PriorBindingActive)
    ));
    assert!(matches!(
        owner
            .publish_binding(
                Arc::clone(&registry),
                actor.clone(),
                jid.clone(),
                candidate,
                None,
                TEST_TIMEOUT,
            )
            .await,
        Err(ActorLocalPublishError::CallerCancelled)
    ));
    drop(output);
    drop(input);
    drop(owner);
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("pump joins"),
        CustodianState::Joined
    );
    assert!(observer.terminal_summary().is_some());
    let terminal = timeout(TEST_TIMEOUT, async {
        loop {
            if let Some(terminal) = index.drain_joined_terminal(&connection) {
                break terminal;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal inventory transfer");
    assert_eq!(terminal.len(), 1);
    let (_, retained, stage) = terminal.into_iter().next().expect("one retained envelope");
    assert_eq!(stage, ActorOutboundStage::Flushed);
    assert_eq!(retained.stanza.to_element(), stanza.to_element());
    assert!(retained.write_acceptance.is_some());
    timeout(TEST_TIMEOUT, async {
        while index.contains(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publication retirement and terminal slot release");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actor_cleanup_index_full_stops_split_pumps_under_independent_inventory() {
    let inventory = IoCustodianInventory::new(2).expect("two physical reservations");
    let index = ActorEndpointCleanupIndex::new(1).expect("one actor cleanup slot");
    let (started, mut ready) = mpsc::channel(2);
    let route = Arc::new(ActorOwnerRoute {
        lifecycle: NodeLifecycle::new(),
        inventory: inventory.clone(),
        index: index.clone(),
        write_probe: None,
        started,
    });
    let app = Router::new()
        .route("/actor-owner", get(actor_owner_route))
        .with_state(route);

    let (first_server_socket, first_client_socket) = tokio::io::duplex(64 * 1024);
    let first_server = tokio::spawn({
        let app = app.clone();
        async move {
            let _ = axum::serve(ActorOwnerDuplexListener(Some(first_server_socket)), app).await;
        }
    });
    let (_first_peer, _) =
        tokio_tungstenite::client_async("ws://localhost/actor-owner", first_client_socket)
            .await
            .expect("first real WebSocket upgrade");
    let (first_connection, first_started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("first owner start deadline")
        .expect("first owner event");
    let (first_owner, first_input, first_output) = first_started.expect("first owner starts");

    let (second_server_socket, second_client_socket) = tokio::io::duplex(64 * 1024);
    let second_server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(second_server_socket)), app).await;
    });
    let (_second_peer, _) =
        tokio_tungstenite::client_async("ws://localhost/actor-owner", second_client_socket)
            .await
            .expect("second real WebSocket upgrade");
    let (second_connection, second_started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("second owner start deadline")
        .expect("second owner event");
    assert!(matches!(
        second_started,
        Err(ActorLocalEndpointError::InventoryFull)
    ));
    assert!(!index.contains(&second_connection));
    let second_observer = inventory
        .observe(&second_connection)
        .expect("split pumps remain independently indexed");
    assert_eq!(
        timeout(TEST_TIMEOUT, second_observer.wait_until_joined())
            .await
            .expect("failed owner pumps join"),
        CustodianState::Joined
    );
    assert!(second_observer.terminal_summary().is_some());

    let first_observer = inventory
        .observe(&first_connection)
        .expect("first physical owner remains indexed");
    first_owner.request_stop(StopReason::Requested);
    drop(first_output);
    drop(first_input);
    drop(first_owner);
    assert_eq!(
        timeout(TEST_TIMEOUT, first_observer.wait_until_joined())
            .await
            .expect("first owner pumps join"),
        CustodianState::Joined
    );
    first_server.abort();
    second_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actor_owner_panic_keeps_unwritten_envelope_in_app_held_terminal_inventory() {
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let app_index = ActorEndpointCleanupIndex::new(1).expect("app terminal inventory");
    let (started, mut ready) = mpsc::channel(1);
    let route = Arc::new(ActorOwnerRoute {
        lifecycle: NodeLifecycle::new(),
        inventory: inventory.clone(),
        index: app_index.clone(),
        write_probe: None,
        started,
    });
    let app = Router::new()
        .route("/actor-owner", get(actor_owner_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (_peer, _) = tokio_tungstenite::client_async("ws://localhost/actor-owner", client_socket)
        .await
        .expect("real WebSocket upgrade");
    let (_, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner event");
    let (mut owner, input, output) = started.expect("physical owner starts");
    let connection = owner.connection_ref().clone();
    let observer = inventory
        .observe(&connection)
        .expect("independent observer");
    let (endpoint, mut receiver) = owner.new_binding_channel().expect("actor queue");
    let jid: jid::FullJid = "alice@example.test/phone".parse().expect("full JID");
    let candidate = ConnectionEntry::new_actor_provisional_for_binding(
        endpoint,
        BindingRef::fresh(connection.clone()),
        jid.clone(),
    )
    .expect("binding matches socket");
    let registry = Arc::new(ConnectionRegistry::new());
    let actor = UserActor::spawn(UserActor::new(jid.to_bare()));
    owner
        .publish_binding(
            registry,
            actor.clone(),
            jid.clone(),
            candidate,
            None,
            TEST_TIMEOUT,
        )
        .await
        .expect("test-only activation");
    let (acceptance, mut acknowledged) = OutboundWriteAcceptance::new();
    let stanza = waddle_xmpp::Stanza::Message(xmpp_parsers::message::Message::new(None));
    assert!(matches!(
        actor
            .ask(TrySendDirectWithCustody {
                jid,
                outbound: OutboundStanza::with_write_acceptance(stanza.clone(), acceptance),
            })
            .await
            .expect("local dispatch"),
        LocalRetryDispatchOutcome::AcceptedOrSettled
    ));
    let id = receiver.try_recv().expect("accepted ID");
    drop(id);
    drop(receiver);
    let panicked_actor = tokio::spawn(async move {
        let _owned = owner;
        panic!("synthetic actor owner panic");
    });
    assert!(panicked_actor
        .await
        .expect_err("owner task panicked")
        .is_panic());
    drop(input);
    drop(output);
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("both pumps join after actor panic"),
        CustodianState::Joined
    );
    let terminal = timeout(TEST_TIMEOUT, async {
        loop {
            if let Some(terminal) = app_index.drain_joined_terminal(&connection) {
                break terminal;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("app retrieves lost actor IDs");
    assert_eq!(terminal.len(), 1);
    let (_, retained, stage) = terminal.into_iter().next().expect("one envelope");
    assert_eq!(stage, ActorOutboundStage::NotAttempted);
    assert_eq!(retained.stanza.to_element(), stanza.to_element());
    assert!(retained.write_acceptance.is_some());
    assert!(matches!(
        acknowledged.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    server.abort();
}

async fn real_socket_stop_at_physical_attempt(after_attempt: bool) {
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let app_index = ActorEndpointCleanupIndex::new(1).expect("app terminal inventory");
    let probe = Arc::new(ActorWriteProbe::default());
    if after_attempt {
        probe.pause_after_attempt();
    } else {
        probe.pause_next();
    }
    let (started, mut ready) = mpsc::channel(1);
    let route = Arc::new(ActorOwnerRoute {
        lifecycle: NodeLifecycle::new(),
        inventory: inventory.clone(),
        index: app_index.clone(),
        write_probe: Some(Arc::clone(&probe)),
        started,
    });
    let app = Router::new()
        .route("/actor-owner", get(actor_owner_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (_peer, _) = tokio_tungstenite::client_async("ws://localhost/actor-owner", client_socket)
        .await
        .expect("real WebSocket upgrade");
    let (_, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner event");
    let (mut owner, input, mut output) = started.expect("physical owner starts");
    let connection = owner.connection_ref().clone();
    let observer = inventory.observe(&connection).expect("physical observer");
    let (endpoint, mut receiver) = owner.new_binding_channel().expect("actor queue");
    let custody = Arc::clone(receiver.custody());
    let jid: jid::FullJid = "alice@example.test/phone".parse().expect("full JID");
    let candidate = ConnectionEntry::new_actor_provisional_for_binding(
        endpoint,
        BindingRef::fresh(connection.clone()),
        jid.clone(),
    )
    .expect("binding matches socket");
    let actor = UserActor::spawn(UserActor::new(jid.to_bare()));
    owner
        .publish_binding(
            Arc::new(ConnectionRegistry::new()),
            actor.clone(),
            jid.clone(),
            candidate,
            None,
            TEST_TIMEOUT,
        )
        .await
        .expect("test-only activation");
    let (acceptance, mut acknowledged) = OutboundWriteAcceptance::new();
    let stanza = waddle_xmpp::Stanza::Message(xmpp_parsers::message::Message::new(None));
    assert!(matches!(
        actor
            .ask(TrySendDirectWithCustody {
                jid,
                outbound: OutboundStanza::with_write_acceptance(stanza.clone(), acceptance),
            })
            .await
            .expect("local dispatch"),
        LocalRetryDispatchOutcome::AcceptedOrSettled
    ));
    let owner = Arc::new(owner);
    let writing_owner = Arc::clone(&owner);
    let write = tokio::spawn(async move {
        let result = writing_owner
            .write_next_direct(&mut receiver, &mut output)
            .await;
        (result, receiver, output)
    });
    if after_attempt {
        timeout(TEST_TIMEOUT, probe.wait_for_after_attempt())
            .await
            .expect("writer reached stalled flush boundary");
    } else {
        timeout(TEST_TIMEOUT, probe.wait_for_entry())
            .await
            .expect("writer reached physical attempt gate");
    }
    owner.request_stop(StopReason::Requested);
    if after_attempt {
        probe.release_after_attempt();
    } else {
        probe.release();
    }
    let (write_result, receiver, output) = timeout(TEST_TIMEOUT, write)
        .await
        .expect("blocked writer exits")
        .expect("writer task returns");
    assert!(write_result.is_err());
    let expected_stage = if after_attempt {
        ActorOutboundStage::AttemptedUncertain
    } else {
        ActorOutboundStage::NotAttempted
    };
    assert_eq!(probe.attempts(), u64::from(after_attempt));
    let ids = custody.terminal_ids().expect("stop sealed custody");
    assert_eq!(ids.len(), 1);
    assert_eq!(custody.stage(&ids[0]), Some(expected_stage));
    drop(receiver);
    drop(output);
    drop(input);
    drop(owner);
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("both pumps join"),
        CustodianState::Joined
    );
    let terminal = timeout(TEST_TIMEOUT, async {
        loop {
            if let Some(terminal) = app_index.drain_joined_terminal(&connection) {
                break terminal;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("joined app inventory");
    assert_eq!(terminal.len(), 1);
    let (_, retained, stage) = terminal.into_iter().next().expect("one envelope");
    assert_eq!(stage, expected_stage);
    assert_eq!(retained.stanza.to_element(), stanza.to_element());
    assert!(retained.write_acceptance.is_some());
    assert!(matches!(
        acknowledged.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_socket_stop_before_physical_attempt_recovers_exact_unwritten_envelope() {
    real_socket_stop_at_physical_attempt(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_socket_stop_during_stalled_flush_keeps_attempt_uncertain() {
    real_socket_stop_at_physical_attempt(true).await;
}

#[derive(Clone, Copy)]
enum PublicationLoss {
    MirrorTimeout,
    CallerCancelled,
    LeaseRevoked,
}

async fn real_socket_publication_loss_compensates_exact_candidate(loss: PublicationLoss) {
    let inventory = IoCustodianInventory::new(1).expect("physical inventory");
    let app_index = ActorEndpointCleanupIndex::new(1).expect("app terminal inventory");
    let lifecycle = NodeLifecycle::new();
    let (started, mut ready) = mpsc::channel(1);
    let route = Arc::new(ActorOwnerRoute {
        lifecycle: lifecycle.clone(),
        inventory: inventory.clone(),
        index: app_index.clone(),
        write_probe: None,
        started,
    });
    let app = Router::new()
        .route("/actor-owner", get(actor_owner_route))
        .with_state(route);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(ActorOwnerDuplexListener(Some(server_socket)), app).await;
    });
    let (_peer, _) = tokio_tungstenite::client_async("ws://localhost/actor-owner", client_socket)
        .await
        .expect("real WebSocket upgrade");
    let (_, started) = timeout(TEST_TIMEOUT, ready.recv())
        .await
        .expect("owner start deadline")
        .expect("owner event");
    let (mut owner, input, output) = started.expect("physical owner starts");
    let connection = owner.connection_ref().clone();
    let observer = inventory.observe(&connection).expect("physical observer");
    let (endpoint, mut receiver) = owner.new_binding_channel().expect("actor queue");
    let jid: jid::FullJid = "alice@example.test/phone".parse().expect("full JID");
    let candidate = ConnectionEntry::new_actor_provisional_for_binding(
        endpoint,
        BindingRef::fresh(connection.clone()),
        jid.clone(),
    )
    .expect("binding matches socket");
    let registry = Arc::new(ConnectionRegistry::new());
    let actor = UserActor::spawn(UserActor::new(jid.to_bare()));
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let mut release_tx = Some(release_tx);
    actor
        .tell(HoldUserActorForPublication {
            entered: entered_tx,
            release: release_rx,
        })
        .await
        .expect("hold actor mailbox");
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("actor barrier deadline")
        .expect("actor entered barrier");
    let owner = Arc::new(owner);
    let publishing_owner = Arc::clone(&owner);
    let publishing_registry = Arc::clone(&registry);
    let publishing_actor = actor.clone();
    let publishing_jid = jid.clone();
    let publishing_candidate = candidate.clone();
    let deadline = if matches!(loss, PublicationLoss::MirrorTimeout) {
        Duration::from_millis(30)
    } else {
        TEST_TIMEOUT
    };
    let publish = tokio::spawn(async move {
        publishing_owner
            .publish_binding(
                publishing_registry,
                publishing_actor,
                publishing_jid,
                publishing_candidate,
                None,
                deadline,
            )
            .await
    });
    timeout(TEST_TIMEOUT, async {
        while registry.entry(&jid).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provisional registry publication");
    assert!(receiver.try_recv().is_err());
    match loss {
        PublicationLoss::MirrorTimeout => {
            assert!(matches!(
                timeout(TEST_TIMEOUT, publish)
                    .await
                    .expect("mirror deadline completes")
                    .expect("publisher task returns"),
                Err(ActorLocalPublishError::MirrorUncertain)
            ));
        }
        PublicationLoss::CallerCancelled => {
            publish.abort();
            assert!(publish.await.expect_err("caller cancelled").is_cancelled());
        }
        PublicationLoss::LeaseRevoked => {
            lifecycle.begin_drain();
            let _ = release_tx.take().expect("release actor once").send(());
            assert!(timeout(TEST_TIMEOUT, publish)
                .await
                .expect("revoked publication completes")
                .expect("publisher task returns")
                .is_err());
        }
    }
    if !matches!(loss, PublicationLoss::LeaseRevoked) {
        let _ = release_tx.take().expect("release actor once").send(());
    }
    timeout(TEST_TIMEOUT, async {
        loop {
            let resources = actor.ask(GetResources).await.expect("mirror inspection");
            if registry.entry(&jid).is_none() && resources.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("exact registry and mirror compensation");
    assert!(receiver.custody().is_sealed());
    assert_eq!(receiver.custody().retained_count(), 0);
    owner.request_stop(StopReason::Requested);
    drop(receiver);
    drop(input);
    drop(output);
    drop(owner);
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("both pumps join"),
        CustodianState::Joined
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_socket_mirror_timeout_compensates_exact_provisional_binding() {
    real_socket_publication_loss_compensates_exact_candidate(PublicationLoss::MirrorTimeout).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_socket_cancelled_publisher_compensates_exact_provisional_binding() {
    real_socket_publication_loss_compensates_exact_candidate(PublicationLoss::CallerCancelled)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_socket_lease_revocation_compensates_exact_provisional_binding() {
    real_socket_publication_loss_compensates_exact_candidate(PublicationLoss::LeaseRevoked).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synthetic_ack_stop_wins_before_output_admission() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, _peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let barrier = Arc::new(ConnectionActorGateBarrier::new());
    let _release = ReleaseAdmissionBarrier(Arc::clone(&barrier));
    let (entered_tx, entered_rx) = oneshot::channel();
    let (result_tx, result_rx) = oneshot::channel();
    controller.pause_next_output_admission(
        OutputAdmissionPause::BeforeGate,
        entered_tx,
        Arc::clone(&barrier),
        result_tx,
    );
    let actor = controller.actor().clone();
    let request = tokio::spawn(async move { actor.ask(RequestAcknowledgement).send().await });
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("output reached pre-gate barrier")
        .expect("output barrier signal");

    controller.request_stop(StopReason::LeaseLost);
    assert_eq!(controller.stop_reason(), Some(StopReason::LeaseLost));
    barrier.release();
    assert_eq!(
        timeout(TEST_TIMEOUT, result_rx)
            .await
            .expect("admission decision")
            .expect("admission observation"),
        Err(ConnectionActorError::Admission(AdmissionError::Shutdown))
    );
    let _ = timeout(TEST_TIMEOUT, request)
        .await
        .expect("request resolves");
    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("stop joins the connection"),
        Some(CustodianState::Joined)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synthetic_ack_admission_wins_before_pump_stop() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let barrier = Arc::new(ConnectionActorGateBarrier::new());
    let _release = ReleaseAdmissionBarrier(Arc::clone(&barrier));
    let (entered_tx, entered_rx) = oneshot::channel();
    let (result_tx, mut result_rx) = oneshot::channel();
    controller.pause_next_output_admission(
        OutputAdmissionPause::InsideGate,
        entered_tx,
        Arc::clone(&barrier),
        result_tx,
    );
    let actor = controller.actor().clone();
    let request = tokio::spawn(async move { actor.ask(RequestAcknowledgement).send().await });
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("output owns both gates")
        .expect("output barrier signal");
    peer.send(Message::Close(None)).await.expect("close peer");
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_stop_signal_callbacks(1))
            .await
            .expect("pump stop reaches lifecycle callback")
    );
    assert!(matches!(
        result_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    barrier.release();
    assert_eq!(
        timeout(TEST_TIMEOUT, result_rx)
            .await
            .expect("admission decision")
            .expect("admission observation"),
        Ok(())
    );
    let _ = timeout(TEST_TIMEOUT, request)
        .await
        .expect("request resolves");
    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("pump stop joins"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::PeerClosed));
}

#[tokio::test]
async fn one_actor_owns_state_and_answers_xep0198_from_contiguous_handled_prefix() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;

    let first = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve first handled-prefix message");
    let second = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve second handled-prefix message");
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(second))
            .send()
            .await
            .expect("complete later operation message"),
        0,
        "later completion cannot skip the earlier hole"
    );

    assert!(controller.observer().is_some());
    assert_eq!(inventory.len(), 1);
    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 0);

    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(first))
            .send()
            .await
            .expect("complete earlier operation message"),
        2,
        "closing the hole advances the contiguous prefix through both completions"
    );
    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 2);

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn actor_processes_control_while_writer_flush_is_blocked_and_wire_waits_for_release() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    let (controller, mut peer, entered) =
        start_actor(&inventory, 4, 256, Arc::clone(&gate), TEST_TIMEOUT).await;

    let first = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve first sequence message");
    let second = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve second sequence message");
    controller
        .actor()
        .ask(CompleteHandledInbound(second))
        .send()
        .await
        .expect("complete later sequence message");

    send_request(&mut peer).await;
    timeout(TEST_TIMEOUT, entered)
        .await
        .expect("writer reaches the flush barrier")
        .expect("flush entered signal");
    let snapshot = controller
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("actor processes control while its writer is stalled");
    assert_eq!(snapshot.handled_h, 0);
    assert!(
        timeout(Duration::from_millis(75), peer.next())
            .await
            .is_err(),
        "the peer cannot observe `<a/>` before the sink flush is released"
    );

    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(first))
            .send()
            .await
            .expect("actor continues state transitions while sink is stalled"),
        2
    );
    gate.release();
    assert_eq!(read_ack(&mut peer).await.h, 0);
    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 2);

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn outbound_count_and_byte_limits_include_the_in_flight_write() {
    let ack_bytes = SmAck::new(0).to_xml().len();
    for (max_count, max_bytes) in [(1, 256), (2, ack_bytes)] {
        let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
        let gate = Arc::new(FlushGate::new());
        let (controller, _peer, entered) = start_actor(
            &inventory,
            max_count,
            max_bytes,
            Arc::clone(&gate),
            Duration::from_millis(50),
        )
        .await;

        actor_ack(&controller)
            .await
            .expect("first acknowledgment is admitted");
        timeout(TEST_TIMEOUT, entered)
            .await
            .expect("writer holds the first frame in poll_flush")
            .expect("flush entered signal");

        assert_eq!(
            timeout(TEST_TIMEOUT, actor_ack(&controller))
                .await
                .expect("overload notification does not deadlock the admission gates"),
            Err(ConnectionActorError::Admission(AdmissionError::Overloaded)),
            "the in-flight frame retains both count and byte permits"
        );
        assert_eq!(controller.stop_reason(), Some(StopReason::OutboundOverload));
        gate.release();
        assert_eq!(
            controller.wait_until_joined().await,
            Some(CustodianState::Joined)
        );
        assert_eq!(inventory.prune_joined(), 1);
    }
}

#[tokio::test]
async fn actor_panic_keeps_blocked_pumps_indexed_for_independent_reacquire_and_join() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    let (controller, _peer, entered) = start_actor(
        &inventory,
        4,
        256,
        Arc::clone(&gate),
        Duration::from_millis(50),
    )
    .await;
    let connection_ref = controller.connection_ref().clone();
    actor_ack(&controller)
        .await
        .expect("admit a frame before actor death");
    timeout(TEST_TIMEOUT, entered)
        .await
        .expect("writer is blocked before actor panic")
        .expect("flush entered signal");

    controller
        .actor()
        .tell(PanicConnectionActor)
        .try_send()
        .expect("deliver injected panic");
    timeout(TEST_TIMEOUT, controller.actor().wait_for_shutdown())
        .await
        .expect("panicked actor shuts down");
    controller.request_stop(StopReason::LeaseLost);

    let observer = inventory
        .observe(&connection_ref)
        .expect("independent inventory still retains the physical connection");
    assert!(observer.is_for(&connection_ref));
    assert_eq!(inventory.len(), 1, "unfinished cleanup keeps its slot");
    gate.release();
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("independent custodian joins after actor panic"),
        CustodianState::Joined
    );
    let summary = observer
        .terminal_summary()
        .expect("both pump outcomes remain observable after actor loss");
    assert!(summary.is_for(&connection_ref));
    assert_ne!(summary.outbound, OutboundTerminal::NotStarted);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn dropping_controller_stops_blocked_pumps_but_only_inventory_join_proves_completion() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    let (controller, _peer, entered) = start_actor(
        &inventory,
        4,
        256,
        Arc::clone(&gate),
        Duration::from_millis(50),
    )
    .await;
    let connection_ref = controller.connection_ref().clone();
    actor_ack(&controller)
        .await
        .expect("admit a frame before controller drop");
    timeout(TEST_TIMEOUT, entered)
        .await
        .expect("writer is blocked before controller drop")
        .expect("flush entered signal");

    drop(controller);
    let observer = inventory
        .observe(&connection_ref)
        .expect("inventory observation survives controller drop");
    assert!(observer.is_for(&connection_ref));
    assert_eq!(
        inventory.len(),
        1,
        "drop requested stop but did not free capacity"
    );
    gate.release();
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("independent observer waits for the real pump joins"),
        CustodianState::Joined
    );
    assert!(observer.terminal_summary().is_some());
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn binding_retirement_fences_stale_completion_without_resetting_handled_prefix() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let connection_ref = controller.connection_ref().clone();
    let binding_a_operation = new_operation_id();
    let binding_a_attempt = controller
        .actor()
        .ask(PrepareBinding::new(binding_a_operation))
        .send()
        .await
        .expect("prepare binding A");
    let binding_a = binding_a_attempt.binding_ref().clone();
    let stale_binding_a_install = binding_a_attempt.clone();
    controller
        .actor()
        .ask(InstallBinding::new(binding_a_attempt.clone()))
        .send()
        .await
        .expect("binding A is installed by its actor-issued transition");
    controller
        .actor()
        .ask(ConfirmBindingInstall(binding_a_attempt))
        .send()
        .await
        .expect("binding A installation handoff is confirmed");
    let sequence_a = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve sequence A message");
    let stale_a_completion = sequence_a.clone();
    assert!(
        !controller
            .actor()
            .ask(RetireBinding(binding_a.clone()))
            .send()
            .await
            .expect("retirement with unresolved work is handled"),
        "retiring a binding cannot strand its unresolved handled work"
    );
    let binding_b_operation = new_operation_id();
    assert_eq!(
        controller
            .actor()
            .ask(PrepareBinding::new(binding_b_operation))
            .send()
            .await
            .expect_err("unresolved work rejects binding replacement")
            .err()
            .expect("typed binding transition rejection"),
        ConnectionActorError::BindingHasPendingHandledInbound,
        "a new binding cannot strand in-flight handled work from the current stream"
    );
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(sequence_a))
            .send()
            .await
            .expect("complete sequence A message"),
        1
    );
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(stale_a_completion.clone()))
            .send()
            .await
            .expect_err("duplicate sequence completion is rejected")
            .err()
            .expect("typed duplicate completion rejection"),
        ConnectionActorError::SequenceTokenMismatch
    );
    assert!(controller
        .actor()
        .ask(RetireBinding(binding_a.clone()))
        .send()
        .await
        .expect("retire A"));
    let binding_b_attempt = controller
        .actor()
        .ask(PrepareBinding::new(binding_b_operation))
        .send()
        .await
        .expect("prepare binding B after A retirement");
    let binding_b = binding_b_attempt.binding_ref().clone();
    controller
        .actor()
        .ask(InstallBinding::new(binding_b_attempt.clone()))
        .send()
        .await
        .expect("binding B installs on the same physical socket");
    controller
        .actor()
        .ask(ConfirmBindingInstall(binding_b_attempt))
        .send()
        .await
        .expect("binding B installation handoff is confirmed");
    assert_eq!(
        controller
            .actor()
            .ask(PrepareBinding::new(binding_a_operation))
            .send()
            .await
            .expect_err("retired A operation ID cannot mint another attempt")
            .err()
            .expect("typed stale prepare rejection"),
        ConnectionActorError::OperationIdNotIncreasing,
        "replaying A's operation ID cannot mint a fresh identity or replace B"
    );
    assert_eq!(
        controller
            .actor()
            .ask(InstallBinding::new(stale_binding_a_install))
            .send()
            .await
            .expect_err("delayed install A is rejected after B is active")
            .err()
            .expect("typed stale binding transition rejection"),
        ConnectionActorError::StaleBindingTransition,
        "retired install token cannot replace the current B attempt"
    );
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(stale_a_completion))
            .send()
            .await
            .expect_err("retired binding completion is rejected")
            .err()
            .expect("typed stale binding rejection"),
        ConnectionActorError::SequenceBindingMismatch,
        "captured binding identity fences a late completion after rebinding"
    );
    let sequence_b = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve sequence B message");
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(sequence_b))
            .send()
            .await
            .expect("complete sequence B message"),
        2,
        "the existing non-resumable stream prefix continues across the binding attempt"
    );
    assert!(
        !controller
            .actor()
            .ask(RetireBinding(binding_a))
            .send()
            .await
            .expect("stale A retirement is processed"),
        "stale A retirement must not retire B"
    );
    let snapshot = controller
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("snapshot actor-owned state");
    assert_eq!(snapshot.connection_ref, connection_ref);
    assert_eq!(snapshot.binding_ref, Some(binding_b));
    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 2);

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn canceled_prepare_and_install_asks_recover_the_exact_binding_attempt() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, _peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let operation_id = new_operation_id();
    let actor = controller.actor().clone();

    let (prepare_entered_tx, prepare_entered_rx) = oneshot::channel();
    let (prepare_release_tx, prepare_release_rx) = oneshot::channel();
    let (observed_attempt_tx, observed_attempt_rx) = oneshot::channel();
    let prepare_actor = actor.clone();
    let canceled_prepare = tokio::spawn(async move {
        prepare_actor
            .ask(PrepareBinding::with_reply_barrier(
                operation_id,
                prepare_entered_tx,
                prepare_release_rx,
                observed_attempt_tx,
            ))
            .send()
            .await
    });
    timeout(TEST_TIMEOUT, prepare_entered_rx)
        .await
        .expect("prepare reaches actor-owned barrier")
        .expect("prepare barrier entered");
    let observed_attempt = timeout(TEST_TIMEOUT, observed_attempt_rx)
        .await
        .expect("prepared attempt is observable before reply")
        .expect("observed binding attempt");
    canceled_prepare.abort();
    let _ = canceled_prepare.await;
    let _ = prepare_release_tx.send(());

    let recovered_attempt = actor
        .ask(PrepareBinding::new(operation_id))
        .send()
        .await
        .expect("retry prepare returns its stable operation result");
    assert_eq!(recovered_attempt, observed_attempt);

    let (install_entered_tx, install_entered_rx) = oneshot::channel();
    let (install_release_tx, install_release_rx) = oneshot::channel();
    let install_actor = actor.clone();
    let attempt_for_install = recovered_attempt.clone();
    let canceled_install = tokio::spawn(async move {
        install_actor
            .ask(InstallBinding::with_reply_barrier(
                attempt_for_install,
                install_entered_tx,
                install_release_rx,
            ))
            .send()
            .await
    });
    timeout(TEST_TIMEOUT, install_entered_rx)
        .await
        .expect("install reaches actor-owned barrier")
        .expect("install barrier entered");
    canceled_install.abort();
    let _ = canceled_install.await;
    let _ = install_release_tx.send(());

    actor
        .ask(InstallBinding::new(recovered_attempt.clone()))
        .send()
        .await
        .expect("retry install reports the already-installed exact attempt");
    actor
        .ask(ConfirmBindingInstall(recovered_attempt.clone()))
        .send()
        .await
        .expect("explicit handoff confirms the recovered installation");
    let snapshot = actor
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("snapshot installed binding");
    assert_eq!(
        snapshot.binding_ref,
        Some(recovered_attempt.binding_ref().clone())
    );
    assert_eq!(snapshot.binding_epoch, 1);

    assert!(actor
        .ask(RetireBinding(recovered_attempt.binding_ref().clone()))
        .send()
        .await
        .expect("retire installed attempt"));
    assert_eq!(
        actor
            .ask(PrepareBinding::new(operation_id))
            .send()
            .await
            .expect_err("retired prepare operation cannot mint another identity")
            .err()
            .expect("typed stale operation ID rejection"),
        ConnectionActorError::OperationIdNotIncreasing
    );
    assert_eq!(
        actor
            .ask(InstallBinding::new(recovered_attempt))
            .send()
            .await
            .expect_err("retired install token is stale")
            .err()
            .expect("typed stale binding token rejection"),
        ConnectionActorError::StaleBindingTransition
    );

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn orphaned_prepare_is_one_bounded_pending_attempt_and_never_installs() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, _peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let actor = controller.actor().clone();
    let operation_a = new_operation_id();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (observed_tx, observed_rx) = oneshot::channel();
    let prepare_actor = actor.clone();
    let canceled_prepare = tokio::spawn(async move {
        prepare_actor
            .ask(PrepareBinding::with_reply_barrier(
                operation_a,
                entered_tx,
                release_rx,
                observed_tx,
            ))
            .send()
            .await
    });
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("prepare enters barrier")
        .expect("prepare barrier signal");
    let orphaned_attempt = timeout(TEST_TIMEOUT, observed_rx)
        .await
        .expect("actor has one prepared attempt")
        .expect("observed orphaned attempt");
    canceled_prepare.abort();
    let _ = canceled_prepare.await;
    let _ = release_tx.send(());

    let operation_b = new_operation_id();
    let attempt_b = actor
        .ask(PrepareBinding::new(operation_b))
        .send()
        .await
        .expect("new prepare supersedes the orphaned pending slot");
    assert_eq!(
        actor
            .ask(InstallBinding::new(orphaned_attempt))
            .send()
            .await
            .expect_err("superseded orphan cannot become active")
            .err()
            .expect("typed stale transition rejection"),
        ConnectionActorError::StaleBindingTransition
    );
    let snapshot = actor
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("snapshot after rejected orphan install");
    assert_eq!(snapshot.binding_ref, None);
    actor
        .ask(InstallBinding::new(attempt_b.clone()))
        .send()
        .await
        .expect("only the current pending attempt installs");
    actor
        .ask(ConfirmBindingInstall(attempt_b.clone()))
        .send()
        .await
        .expect("current binding installation is confirmed");
    assert_eq!(
        actor
            .ask(SnapshotConnection)
            .send()
            .await
            .expect("snapshot current install")
            .binding_ref,
        Some(attempt_b.binding_ref().clone())
    );

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn orphaned_install_deadline_stops_old_socket_and_requires_a_fresh_connection() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let deadline = Duration::from_millis(50);
    let (controller, _peer, _) =
        start_actor_with_lifecycle_deadline(&inventory, 4, 256, gate, deadline).await;
    let old_connection_ref = controller.connection_ref().clone();
    let actor = controller.actor().clone();
    let attempt = actor
        .ask(PrepareBinding::new(new_operation_id()))
        .send()
        .await
        .expect("prepare the binding whose install result will be orphaned");
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let install_actor = actor.clone();
    let orphaned_attempt = attempt.clone();
    let canceled_install = tokio::spawn(async move {
        install_actor
            .ask(InstallBinding::with_reply_barrier(
                orphaned_attempt,
                entered_tx,
                release_rx,
            ))
            .send()
            .await
    });
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("install is active before the canceled reply")
        .expect("install barrier entered");
    canceled_install.abort();
    let _ = canceled_install.await;
    let _ = release_tx.send(());

    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("unconfirmed install deadline stops and joins the exact socket"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::Requested));
    assert!(
        actor.wait_for_shutdown_result().await.is_ok(),
        "the independent stop kills the actor, so a late token cannot revive it"
    );
    assert_eq!(controller.handled_h(), 0);
    assert_eq!(inventory.prune_joined(), 1);

    let fresh_gate = Arc::new(FlushGate::new());
    fresh_gate.release();
    let (fresh_controller, _fresh_peer, _) =
        start_actor(&inventory, 4, 256, fresh_gate, TEST_TIMEOUT).await;
    assert_ne!(
        fresh_controller.connection_ref(),
        &old_connection_ref,
        "recovery after an orphaned install starts with a new physical identity"
    );
    let fresh_attempt = fresh_controller
        .actor()
        .ask(PrepareBinding::new(new_operation_id()))
        .send()
        .await
        .expect("fresh connection can prepare a binding");
    fresh_controller
        .actor()
        .ask(InstallBinding::new(fresh_attempt.clone()))
        .send()
        .await
        .expect("fresh connection installs its attempt");
    fresh_controller
        .actor()
        .ask(ConfirmBindingInstall(fresh_attempt.clone()))
        .send()
        .await
        .expect("fresh connection confirms installation");

    fresh_controller.request_stop(StopReason::Requested);
    assert_eq!(
        fresh_controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn canceled_reservation_ask_recovers_the_same_sequence_by_operation_id() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let actor = controller.actor().clone();
    let operation_id = new_operation_id();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let request_actor = actor.clone();
    let canceled_request = tokio::spawn(async move {
        request_actor
            .ask(ReserveHandledInbound::with_reply_barrier(
                operation_id,
                entered_tx,
                release_rx,
            ))
            .send()
            .await
    });
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("reservation is actor-owned before ask cancellation")
        .expect("reservation barrier entered");
    canceled_request.abort();
    let _ = canceled_request.await;
    let _ = release_tx.send(());

    let recovered = actor
        .ask(ReserveHandledInbound::new(operation_id))
        .send()
        .await
        .expect("retry retrieves the original reservation");
    assert_eq!(
        actor
            .ask(SnapshotConnection)
            .send()
            .await
            .expect("reservation alone does not advance h")
            .handled_h,
        0
    );
    assert_eq!(
        actor
            .ask(CompleteHandledInbound(recovered))
            .send()
            .await
            .expect("complete recovered reservation"),
        1,
        "retried operation closes the original ordinal rather than creating a gap"
    );
    assert_eq!(
        actor
            .ask(ReserveHandledInbound::new(operation_id))
            .send()
            .await
            .expect_err("settled operation ID cannot reserve a phantom sequence")
            .err()
            .expect("typed monotonic ID rejection"),
        ConnectionActorError::OperationIdNotIncreasing
    );
    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 1);

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn canceled_reservation_without_retry_stops_and_joins_exact_transport() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, _peer, _) = start_actor_with_inbound_limit(
        &inventory,
        8,
        4,
        256,
        gate,
        TEST_TIMEOUT,
        Duration::from_millis(40),
    )
    .await;
    let actor = controller.actor().clone();
    let operation_id = new_operation_id();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let request_actor = actor.clone();
    let canceled_request = tokio::spawn(async move {
        request_actor
            .ask(ReserveHandledInbound::with_reply_barrier(
                operation_id,
                entered_tx,
                release_rx,
            ))
            .send()
            .await
    });
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("unresolved reservation reaches the cancellation barrier")
        .expect("reservation barrier entered");
    canceled_request.abort();
    let _ = canceled_request.await;
    let _ = release_tx.send(());

    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("operation deadline stops pumps when no caller retries"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::Requested));
    assert_eq!(inventory.len(), 1, "joined observation remains indexed");
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn stopped_transport_rejects_late_handled_completion_without_advancing_h() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, _peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let token = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve before stop");

    controller.request_stop(StopReason::LeaseLost);
    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("lease-loss stop joins both pumps"),
        Some(CustodianState::Joined)
    );
    assert!(
        controller
            .actor()
            .ask(CompleteHandledInbound(token))
            .send()
            .await
            .is_err(),
        "the stopped actor rejects late completion"
    );
    assert_eq!(controller.handled_h(), 0);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completion_that_holds_lifecycle_gate_wins_over_its_deadline() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let deadline = Duration::from_millis(40);
    let (controller, _peer, _) =
        start_actor_with_lifecycle_deadline(&inventory, 4, 256, gate, deadline).await;
    let token = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve before deadline");

    let lifecycle_barrier = Arc::new(ConnectionActorGateBarrier::new());
    let (entered_tx, entered_rx) = oneshot::channel();
    let actor = controller.actor().clone();
    let message_barrier = Arc::clone(&lifecycle_barrier);
    let completion = tokio::spawn(async move {
        actor
            .ask(CompleteHandledInboundAtLifecycleGate::new(
                token,
                entered_tx,
                message_barrier,
            ))
            .send()
            .await
    });
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("completion holds the lifecycle gate before h mutation")
        .expect("completion gate barrier entered");

    tokio::time::sleep(Duration::from_millis(120)).await;
    lifecycle_barrier.release();
    assert_eq!(
        timeout(TEST_TIMEOUT, completion)
            .await
            .expect("completion finishes after releasing its gate")
            .expect("completion task joins")
            .expect("actor accepts completion"),
        1
    );
    assert_eq!(
        controller.stop_reason(),
        None,
        "deadline observes that the operation committed before the stop transition"
    );
    assert_eq!(
        controller
            .actor()
            .ask(SnapshotConnection)
            .send()
            .await
            .expect("snapshot completed state")
            .handled_h,
        1
    );

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_stop_signal_waits_for_an_in_progress_completion_gate() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, _peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let controller = Arc::new(controller);
    let token = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve before external stop");

    let lifecycle_barrier = Arc::new(ConnectionActorGateBarrier::new());
    let (entered_tx, entered_rx) = oneshot::channel();
    let actor = controller.actor().clone();
    let message_barrier = Arc::clone(&lifecycle_barrier);
    let completion = tokio::spawn(async move {
        actor
            .ask(CompleteHandledInboundAtLifecycleGate::new(
                token,
                entered_tx,
                message_barrier,
            ))
            .send()
            .await
    });
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("completion owns the shared lifecycle gate")
        .expect("completion gate barrier entered");

    let stop_controller = Arc::clone(&controller);
    let stop = tokio::task::spawn_blocking(move || {
        stop_controller.request_stop(StopReason::LeaseLost);
    });
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_stop_signal_callbacks(1))
            .await
            .expect("external caller enters StopSignal callback while completion owns the gate"),
        "StopSignal lifecycle callback is observable before it waits for the gate"
    );

    lifecycle_barrier.release();
    assert_eq!(
        timeout(TEST_TIMEOUT, completion)
            .await
            .expect("completion finishes before the waiting stop callback")
            .expect("completion task joins")
            .expect("actor accepts completion"),
        1
    );
    timeout(TEST_TIMEOUT, stop)
        .await
        .expect("stop callback continues after completion releases the gate")
        .expect("stop request task returns");
    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("external stop joins both pumps"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::LeaseLost));
    assert_eq!(
        controller.handled_h(),
        1,
        "completion wins because it already owns the lifecycle gate"
    );
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn deadline_that_wins_before_completion_keeps_h_unchanged() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let deadline = Duration::from_millis(40);
    let (controller, _peer, _) =
        start_actor_with_lifecycle_deadline(&inventory, 4, 256, gate, deadline).await;
    let token = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve before deadline");

    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let actor = controller.actor().clone();
    let completion = tokio::spawn(async move {
        actor
            .ask(CompleteHandledInboundBeforeLifecycleGate::new(
                token, entered_tx, release_rx,
            ))
            .send()
            .await
    });
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("completion pauses before the lifecycle gate")
        .expect("pre-gate barrier entered");

    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("deadline independently interrupts and joins both pumps"),
        Some(CustodianState::Joined)
    );
    let _ = release_tx.send(());
    let late_completion = timeout(TEST_TIMEOUT, completion)
        .await
        .expect("completion request finishes after the deadline")
        .expect("completion task joins");
    assert!(
        late_completion.is_err(),
        "the stopped actor rejects late work"
    );
    assert_eq!(
        controller.handled_h(),
        0,
        "a stop winner prevents the late handler from advancing h"
    );
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn pump_initiated_peer_close_wins_the_lifecycle_gate_before_late_completion() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let token = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve before peer close");

    peer.send(Message::Close(None))
        .await
        .expect("send peer close frame to reader pump");
    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("reader pump's stop callback joins both pumps"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::PeerClosed));
    assert!(
        controller
            .actor()
            .ask(CompleteHandledInbound(token))
            .send()
            .await
            .is_err(),
        "pump stop kills the actor before late completion can run"
    );
    assert_eq!(controller.handled_h(), 0);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn caller_operation_ids_are_strictly_increasing_per_actor() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, _peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let lower_id = new_operation_id();
    let higher_id = new_operation_id();
    let higher = controller
        .actor()
        .ask(ReserveHandledInbound::new(higher_id))
        .send()
        .await
        .expect("higher ID is admitted first");
    assert_eq!(
        controller
            .actor()
            .ask(ReserveHandledInbound::new(lower_id))
            .send()
            .await
            .expect_err("unseen lower ID is rejected after high-water advances")
            .err()
            .expect("typed out-of-order ID rejection"),
        ConnectionActorError::OperationIdNotIncreasing
    );
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(higher))
            .send()
            .await
            .expect("complete admitted higher ID"),
        1
    );

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn wrapped_xep_counter_does_not_alias_old_completion_generation() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let actor = controller.actor();
    let old_operation_id = new_operation_id();
    let old_token = actor
        .ask(ReserveHandledInbound::new(old_operation_id))
        .send()
        .await
        .expect("reserve old completion generation");
    let stale_old_token = old_token.clone();
    assert_eq!(old_token.sequence_number(), 1);
    assert_eq!(
        actor
            .ask(CompleteHandledInbound(old_token))
            .send()
            .await
            .expect("complete old generation"),
        1
    );

    for _ in 0..256 {
        let token = actor
            .ask(ReserveHandledInbound::new(new_operation_id()))
            .send()
            .await
            .expect("reserve operation to age out caller ID");
        actor
            .ask(CompleteHandledInbound(token))
            .send()
            .await
            .expect("complete operation to age out caller ID");
    }
    assert_eq!(
        actor
            .ask(ReserveHandledInbound::new(old_operation_id))
            .send()
            .await
            .expect_err("old caller ID remains rejected after many settlements")
            .err()
            .expect("typed permanent high-water rejection"),
        ConnectionActorError::OperationIdNotIncreasing
    );

    actor
        .ask(SeedInboundCounterForTest(0))
        .send()
        .await
        .expect("seed the XEP counter for a deterministic wrap alias");
    let new_token = actor
        .ask(ReserveHandledInbound::new(new_operation_id()))
        .send()
        .await
        .expect("reserve new generation at the same XEP ordinal");
    assert_eq!(
        new_token.sequence_number(),
        stale_old_token.sequence_number()
    );
    assert_ne!(
        new_token.nonce_generation(),
        stale_old_token.nonce_generation(),
        "the completion nonce advances independently of XEP h"
    );
    assert_eq!(
        actor
            .ask(CompleteHandledInbound(stale_old_token))
            .send()
            .await
            .expect_err("old generation cannot complete the same XEP ordinal")
            .err()
            .expect("typed stale operation rejection"),
        ConnectionActorError::SequenceTokenMismatch
    );
    assert_eq!(
        actor
            .ask(SnapshotConnection)
            .send()
            .await
            .expect("stale token does not invent progress")
            .handled_h,
        0
    );
    assert_eq!(
        actor
            .ask(CompleteHandledInbound(new_token))
            .send()
            .await
            .expect("complete current generation"),
        1
    );
    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 1);

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn handled_completion_token_is_fenced_by_exact_physical_connection() {
    let inventory = IoCustodianInventory::new(2).expect("inventory capacity");
    let gate_a = FlushGate::new();
    gate_a.release();
    let gate_b = FlushGate::new();
    gate_b.release();
    let (controller_a, _peer_a, _) =
        start_actor(&inventory, 4, 256, Arc::new(gate_a), TEST_TIMEOUT).await;
    let (controller_b, mut peer_b, _) =
        start_actor(&inventory, 4, 256, Arc::new(gate_b), TEST_TIMEOUT).await;

    let sequence_a = controller_a
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve A message");
    let sequence_a_for_b = sequence_a.clone();
    let sequence_b = controller_b
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve B message");
    assert_eq!(
        controller_b
            .actor()
            .ask(CompleteHandledInbound(sequence_a_for_b))
            .send()
            .await
            .expect_err("cross-connection completion is rejected")
            .err()
            .expect("typed physical connection rejection"),
        ConnectionActorError::SequenceConnectionMismatch,
        "equal ordinals from different physical sockets are not interchangeable"
    );
    assert_eq!(
        controller_b
            .actor()
            .ask(CompleteHandledInbound(sequence_b))
            .send()
            .await
            .expect("complete B message"),
        1
    );
    assert_eq!(
        controller_a
            .actor()
            .ask(CompleteHandledInbound(sequence_a))
            .send()
            .await
            .expect("complete A message"),
        1
    );
    send_request(&mut peer_b).await;
    assert_eq!(read_ack(&mut peer_b).await.h, 1);

    controller_a.request_stop(StopReason::Requested);
    controller_b.request_stop(StopReason::Requested);
    assert_eq!(
        controller_a.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
    assert_eq!(
        controller_b.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
    assert_eq!(inventory.prune_joined(), 2);
}

#[tokio::test]
async fn handled_prefix_window_bounds_pending_and_out_of_order_completions() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;

    let mut sequences = Vec::with_capacity(256);
    for _ in 0..256 {
        sequences.push(
            controller
                .actor()
                .ask(reserve_request())
                .send()
                .await
                .expect("reserve handled-prefix message"),
        );
    }
    assert_eq!(
        controller
            .actor()
            .ask(reserve_request())
            .send()
            .await
            .expect_err("pending handled-prefix window is full")
            .err()
            .expect("typed handled-prefix capacity rejection"),
        ConnectionActorError::HandledInboundFull,
        "uncompleted reservations cannot grow beyond the configured policy"
    );

    for sequence in sequences.iter().skip(1).cloned() {
        assert_eq!(
            controller
                .actor()
                .ask(CompleteHandledInbound(sequence))
                .send()
                .await
                .expect("complete later operation message"),
            0,
            "completed entries behind the earliest gap do not advance h"
        );
    }
    assert_eq!(
        controller
            .actor()
            .ask(reserve_request())
            .send()
            .await
            .expect_err("out-of-order handled-prefix window is full")
            .err()
            .expect("typed handled-prefix capacity rejection"),
        ConnectionActorError::HandledInboundFull,
        "completed entries behind the gap retain the same bounded window"
    );
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(sequences.remove(0)))
            .send()
            .await
            .expect("complete earliest operation message"),
        256
    );
    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 256);

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn inbound_permit_stays_held_while_actor_mailbox_is_paused() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor_with_inbound_limit(
        &inventory,
        1,
        4,
        256,
        gate,
        TEST_TIMEOUT,
        HANDLED_OPERATION_DEADLINE,
    )
    .await;
    let retained_actor = controller.actor().clone();

    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    controller
        .actor()
        .tell(PauseConnectionActor {
            entered: entered_tx,
            release: release_rx,
        })
        .send()
        .await
        .expect("enqueue actor pause barrier");
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("actor enters pause barrier")
        .expect("pause entry signal");

    send_request(&mut peer).await;
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_inbound_forwarded(1))
            .await
            .expect("inbound forwarder enqueues first retained item"),
        "the first admitted item reaches the paused actor mailbox"
    );
    send_request(&mut peer).await;
    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("second ingress exceeds the one-item inbound budget"),
        Some(CustodianState::Joined)
    );
    assert_eq!(
        controller.stop_reason(),
        Some(StopReason::InboundOverload),
        "the first item's count and byte permits remain held in the paused mailbox"
    );
    assert_eq!(
        inventory.len(),
        1,
        "the exact inventory slot remains held through actor mailbox cleanup"
    );
    assert!(
        retained_actor.wait_for_shutdown_result().await.is_ok(),
        "stop kills the paused actor even though an ActorRef is retained"
    );
    let _ = release_tx.send(());
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn peer_request_ack_saturation_stops_and_joins_the_physical_writer() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    let (controller, mut peer, entered) = start_actor(
        &inventory,
        1,
        256,
        Arc::clone(&gate),
        Duration::from_millis(50),
    )
    .await;

    send_request(&mut peer).await;
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_inbound_forwarded(1))
            .await
            .expect("first peer request reaches the actor mailbox"),
        "first typed `<r/>` is forwarded"
    );
    timeout(TEST_TIMEOUT, entered)
        .await
        .expect("first `<a h/>` reaches the blocked writer")
        .expect("writer flush barrier entered");

    send_request(&mut peer).await;
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_inbound_forwarded(2))
            .await
            .expect("second peer request reaches the actor mailbox"),
        "actor receives control while the first writer flush is stalled"
    );
    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("outbound overload interrupts and joins the writer"),
        Some(CustodianState::Joined)
    );
    assert_eq!(
        controller.stop_reason(),
        Some(StopReason::OutboundOverload),
        "the peer-driven second `<r/>` observes the in-flight writer permit"
    );
    gate.release();
    assert_eq!(
        inventory.len(),
        1,
        "joined cleanup remains indexed until pruned"
    );
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn normal_actor_stop_also_keeps_controller_stop_and_inventory_independent() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, _peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let connection_ref = controller.connection_ref().clone();
    controller
        .actor()
        .tell(StopConnectionActor)
        .try_send()
        .expect("request normal actor stop");
    timeout(TEST_TIMEOUT, controller.actor().wait_for_shutdown())
        .await
        .expect("actor drops its state owner");
    controller.request_stop(StopReason::LeaseLost);
    let observer = inventory
        .observe(&connection_ref)
        .expect("controller can reacquire after actor drop");
    assert_eq!(observer.wait_until_joined().await, CustodianState::Joined);
}

#[tokio::test]
async fn reservation_is_indexed_before_the_actor_pump_factory_runs() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let (server, _client) = super::pumps::socket_pair().await;
    let (seen_tx, seen_rx) = oneshot::channel();
    let inv = inventory.clone();
    let config = ReservedIoStartConfig::new(
        TransportLimits::new(
            Limits::new(2, 512).expect("inbound bounds"),
            Limits::new(2, 512).expect("outbound bounds"),
        ),
        ShutdownProtocol::new(ConnectionWireFrame::Close, |error: &Error| {
            matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
        }),
        ConnectionWireFrame::encoded_len,
        ConnectionWireFrame::is_close,
        TEST_TIMEOUT,
    );
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let controller = start_connection_actor(
        &inventory,
        NodeLeaseIdentity::local_process(),
        HANDLED_OPERATION_DEADLINE,
        config,
        move |connection_ref| {
            let connection_ref = connection_ref.clone();
            async move {
                assert_eq!(inv.len(), 1);
                assert_eq!(
                    inv.reserve(connection_ref.clone()).err(),
                    Some(CustodianInventoryError::DuplicateConnection),
                    "the exact connection is reserved before the pump factory runs"
                );
                let _ = seen_tx.send(connection_ref.clone());
                let (writer, reader) = server.split();
                let reader = reader
                    .filter_map(|incoming| async move {
                        match incoming {
                            Ok(Message::Text(text)) => {
                                matches!(SmStanza::parse(text.as_str()), Some(SmStanza::Request))
                                    .then_some(Ok(ConnectionWireFrame::RequestAcknowledgement {
                                        wire_bytes: text.len(),
                                    }))
                            }
                            Ok(Message::Close(_)) => Some(Ok(ConnectionWireFrame::Close)),
                            Ok(_) => None,
                            Err(error) => Some(Err(error)),
                        }
                    })
                    .boxed();
                let writer = GatedFlushSink {
                    inner: writer,
                    gate,
                    entered: None,
                    pending_flush: false,
                };
                (reader, writer)
            }
        },
    )
    .await
    .expect("reserved startup");
    let factory_ref = timeout(TEST_TIMEOUT, seen_rx)
        .await
        .expect("factory reports its exact reserved identity")
        .expect("factory identity signal");
    assert_eq!(&factory_ref, controller.connection_ref());

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn actor_core_retains_transferred_identity_and_existing_state() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let (identity_tx, identity_rx) = oneshot::channel();
    let config = ReservedIoStartConfig::new(
        test_transport_limits(2, 2048, 2, 256),
        ShutdownProtocol::new(
            ConnectionWireFrame::Close,
            |_: &std::convert::Infallible| false,
        ),
        ConnectionWireFrame::encoded_len,
        ConnectionWireFrame::is_close,
        TEST_TIMEOUT,
    );
    let controller =
        start_connection_actor_with_hooks(
            &inventory,
            NodeLeaseIdentity::local_process(),
            HANDLED_OPERATION_DEADLINE,
            config,
            move |connection_ref| {
                let _ = identity_tx.send(connection_ref.clone());
                async {
                    (
                        futures::stream::pending::<
                            Result<ConnectionWireFrame, std::convert::Infallible>,
                        >(),
                        futures::sink::drain::<ConnectionWireFrame>(),
                    )
                }
            },
            |_| async { CountableEffectDisposition::Handled },
            ConnectionActorStartHooks {
                initial_handled_count: Some(37),
                ..ConnectionActorStartHooks::default()
            },
        )
        .await
        .expect("reserved actor starts");

    let reserved_identity = identity_rx.await.expect("factory sees reserved identity");
    let snapshot = controller
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("snapshot transferred state");
    assert_eq!(snapshot.connection_ref, reserved_identity);
    assert_eq!(controller.connection_ref(), &reserved_identity);
    assert!(snapshot.stream_management_enabled);
    assert_eq!(snapshot.handled_h, 37);
    assert_eq!(controller.handled_h(), 37);

    let sequence = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve from transferred state");
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(sequence))
            .send()
            .await
            .expect("complete from transferred state"),
        38
    );
    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn failed_reservation_does_not_call_the_connection_pump_factory() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let (first, _first_peer, _) = start_actor(
        &inventory,
        4,
        256,
        Arc::new({
            let gate = FlushGate::new();
            gate.release();
            gate
        }),
        TEST_TIMEOUT,
    )
    .await;
    let (server, _client) = super::pumps::socket_pair().await;
    let called = Arc::new(AtomicBool::new(false));
    let called_factory = Arc::clone(&called);
    let config = ReservedIoStartConfig::new(
        TransportLimits::new(
            Limits::new(2, 512).expect("inbound bounds"),
            Limits::new(2, 512).expect("outbound bounds"),
        ),
        ShutdownProtocol::new(ConnectionWireFrame::Close, |error: &Error| {
            matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
        }),
        ConnectionWireFrame::encoded_len,
        ConnectionWireFrame::is_close,
        TEST_TIMEOUT,
    );
    let result = start_connection_actor(
        &inventory,
        NodeLeaseIdentity::local_process(),
        HANDLED_OPERATION_DEADLINE,
        config,
        move |_| async move {
            called_factory.store(true, Ordering::Release);
            let (writer, reader) = server.split();
            let reader = reader
                .filter_map(|incoming| async move {
                    match incoming {
                        Ok(Message::Text(text)) => {
                            matches!(SmStanza::parse(text.as_str()), Some(SmStanza::Request))
                                .then_some(Ok(ConnectionWireFrame::RequestAcknowledgement {
                                    wire_bytes: text.len(),
                                }))
                        }
                        Ok(Message::Close(_)) => Some(Ok(ConnectionWireFrame::Close)),
                        Ok(_) => None,
                        Err(error) => Some(Err(error)),
                    }
                })
                .boxed();
            let gate = Arc::new(FlushGate::new());
            gate.release();
            let writer = GatedFlushSink {
                inner: writer,
                gate,
                entered: None,
                pending_flush: false,
            };
            (reader, writer)
        },
    )
    .await;
    let error = match result {
        Ok(_) => panic!("the full inventory must refuse startup before the factory"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ConnectionActorStartError::Inventory(CustodianInventoryError::Full)
    ));
    assert!(!called.load(Ordering::Acquire));
    first.request_stop(StopReason::Requested);
    assert_eq!(
        first.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_countable_stanza_keeps_h_behind_a_hole_while_peer_control_progresses() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (effect_started_tx, mut effect_started_rx) = mpsc::unbounded_channel();
    let (effect_release_tx, effect_release_rx) = watch::channel(false);
    let (controller, mut peer, _) = start_actor_with_inbound_limits_and_effect(
        &inventory,
        test_transport_limits(2, 2048, 4, 256),
        gate,
        TEST_TIMEOUT,
        HANDLED_OPERATION_DEADLINE,
        move |effect| {
            let effect_started = effect_started_tx.clone();
            let mut release = effect_release_rx.clone();
            async move {
                let _ = effect_started.send(effect);
                loop {
                    if *release.borrow_and_update() {
                        return CountableEffectDisposition::Handled;
                    }
                    if release.changed().await.is_err() {
                        return CountableEffectDisposition::Uncertain;
                    }
                }
            }
        },
    )
    .await;

    send_stanza(&mut peer, "<message><body>slow effect</body></message>").await;
    let started = timeout(TEST_TIMEOUT, effect_started_rx.recv())
        .await
        .expect("typed stanza reaches external effect owner")
        .expect("effect start signal");
    assert_eq!(started.sequence.sequence_number(), 1);

    let later = controller
        .actor()
        .ask(reserve_request())
        .send()
        .await
        .expect("reserve later operation behind the running effect");
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(later))
            .send()
            .await
            .expect("complete out of order operation"),
        0,
        "later handled work cannot advance across the stanza effect hole"
    );

    send_request(&mut peer).await;
    assert_eq!(
        read_ack(&mut peer).await.h,
        0,
        "peer control is acknowledged with the pre-hole handled prefix"
    );
    assert_eq!(controller.handled_h(), 0);

    effect_release_tx
        .send(true)
        .expect("release the synthetic external effect");
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_effects_completed(1))
            .await
            .expect("effect completion and actor ask finish"),
        "effect owner completes the exact current token"
    );
    assert_eq!(controller.handled_h(), 2);
    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 2);

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_real_countable_effects_are_serialized_while_peer_ack_reports_the_prefix() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (effect_started_tx, mut effect_started_rx) = mpsc::unbounded_channel();
    let (effect_release_tx, effect_release_rx) = watch::channel(0u32);
    let (controller, mut peer, _) = start_actor_with_inbound_limits_and_effect(
        &inventory,
        test_transport_limits(3, 2048, 4, 256),
        gate,
        TEST_TIMEOUT,
        HANDLED_OPERATION_DEADLINE,
        move |effect| {
            let effect_started = effect_started_tx.clone();
            let mut release = effect_release_rx.clone();
            async move {
                let sequence = effect.sequence.sequence_number();
                let _ = effect_started.send(sequence);
                loop {
                    if *release.borrow_and_update() >= sequence {
                        return CountableEffectDisposition::Handled;
                    }
                    if release.changed().await.is_err() {
                        return CountableEffectDisposition::Uncertain;
                    }
                }
            }
        },
    )
    .await;

    send_stanza(&mut peer, "<message><body>first</body></message>").await;
    assert_eq!(
        timeout(TEST_TIMEOUT, effect_started_rx.recv())
            .await
            .expect("first effect starts")
            .expect("first effect token"),
        1
    );
    send_stanza(&mut peer, "<presence/>").await;
    assert!(controller.wait_for_inbound_forwarded(2).await);
    let _ = controller
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("actor processed both countable frames");
    assert!(
        matches!(
            effect_started_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ),
        "the serialized effect owner does not start stanza two while stanza one is unresolved"
    );

    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 0);
    effect_release_tx.send_replace(1);
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_effects_completed(1))
            .await
            .expect("first effect and actor completion finish")
    );
    assert_eq!(
        timeout(TEST_TIMEOUT, effect_started_rx.recv())
            .await
            .expect("second effect starts after first completion")
            .expect("second effect token"),
        2
    );
    assert_eq!(controller.handled_h(), 1);

    effect_release_tx.send_replace(2);
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_effects_completed(2))
            .await
            .expect("second effect and actor completion finish")
    );
    assert_eq!(controller.handled_h(), 2);
    send_request(&mut peer).await;
    assert_eq!(read_ack(&mut peer).await.h, 2);
    controller.request_stop(StopReason::Requested);
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_effects_completed(2))
            .await
            .expect("completed effect target is retained after stop")
    );
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_drops_the_active_effect_before_auxiliary_join_and_prune() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (effect_started_tx, effect_started_rx) = oneshot::channel();
    let (effect_dropped_tx, effect_dropped_rx) = oneshot::channel();
    let effect_started_tx = Arc::new(Mutex::new(Some(effect_started_tx)));
    let effect_dropped_tx = Arc::new(Mutex::new(Some(effect_dropped_tx)));
    let (controller, mut peer, _) = start_actor_with_inbound_limits_and_effect(
        &inventory,
        test_transport_limits(2, 2048, 4, 256),
        gate,
        TEST_TIMEOUT,
        HANDLED_OPERATION_DEADLINE,
        move |_| {
            let started = effect_started_tx
                .lock()
                .expect("effect start notifier lock")
                .take();
            let dropped = effect_dropped_tx
                .lock()
                .expect("effect drop notifier lock")
                .take();
            async move {
                if let Some(started) = started {
                    let _ = started.send(());
                }
                let _drop_notification = DropNotification(dropped);
                std::future::pending::<CountableEffectDisposition>().await
            }
        },
    )
    .await;

    send_stanza(&mut peer, "<message><body>blocked effect</body></message>").await;
    timeout(TEST_TIMEOUT, effect_started_rx)
        .await
        .expect("effect started before stop")
        .expect("effect start signal");
    controller.request_stop(StopReason::LeaseLost);
    timeout(TEST_TIMEOUT, effect_dropped_rx)
        .await
        .expect("stop cancels the active external effect")
        .expect("effect Drop signal");
    assert!(
        !timeout(TEST_TIMEOUT, controller.wait_for_effects_completed(2))
            .await
            .expect("stop resolves an unreachable completion wait"),
        "an unreachable effect count is false after stop"
    );
    assert_eq!(
        inventory.len(),
        1,
        "joined proof still owns inventory capacity"
    );
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn message_presence_iq_are_counted_and_malformed_xml_does_not_reserve_h() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;

    send_stanza(&mut peer, "not well-formed XML").await;
    send_stanza(&mut peer, "<message type='chat'><body>m</body></message>").await;
    send_stanza(&mut peer, "<presence/>").await;
    send_stanza(
        &mut peer,
        "<iq type='get' id='ping'><ping xmlns='urn:xmpp:ping'/></iq>",
    )
    .await;
    assert!(
        timeout(TEST_TIMEOUT, controller.wait_for_effects_completed(3))
            .await
            .expect("all three typed stanzas complete"),
        "message, presence and IQ each cross the countable effect boundary"
    );
    assert_eq!(controller.handled_h(), 3);

    send_request(&mut peer).await;
    assert_eq!(
        read_ack(&mut peer).await.h,
        3,
        "malformed XML did not consume a handled-prefix sequence"
    );
    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_item_limit_fails_closed_when_peer_requests_ack_during_slow_effect() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (effect_started_tx, mut effect_started_rx) = mpsc::unbounded_channel();
    let (effect_release_tx, effect_release_rx) = watch::channel(false);
    let (controller, mut peer, _) = start_actor_with_inbound_limits_and_effect(
        &inventory,
        test_transport_limits(1, 2048, 4, 256),
        gate,
        TEST_TIMEOUT,
        HANDLED_OPERATION_DEADLINE,
        move |effect| {
            let effect_started = effect_started_tx.clone();
            let mut release = effect_release_rx.clone();
            async move {
                let _ = effect_started.send(effect);
                loop {
                    if *release.borrow_and_update() {
                        return CountableEffectDisposition::Handled;
                    }
                    if release.changed().await.is_err() {
                        return CountableEffectDisposition::Uncertain;
                    }
                }
            }
        },
    )
    .await;

    send_stanza(&mut peer, "<message><body>held</body></message>").await;
    let _started = timeout(TEST_TIMEOUT, effect_started_rx.recv())
        .await
        .expect("slow stanza enters effect owner")
        .expect("effect start signal");
    send_request(&mut peer).await;

    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("saturated control ingress stops the connection"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::InboundOverload));
    assert_eq!(controller.handled_h(), 0);
    assert!(
        !controller.wait_for_inbound_forwarded(2).await,
        "the saturated `<r/>` never reaches the actor and cannot receive a fabricated ACK"
    );
    assert_eq!(inventory.len(), 1, "joined exact identity remains indexed");
    let _ = effect_release_tx.send(true);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inbound_byte_budget_includes_the_stanza_held_by_a_slow_effect() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (effect_started_tx, mut effect_started_rx) = mpsc::unbounded_channel();
    let (effect_release_tx, effect_release_rx) = watch::channel(false);
    let first = message_with_body(&"a".repeat(40));
    let second = message_with_body(&"b".repeat(20));
    let byte_limit = first.len() + 1;
    assert!(second.len() <= byte_limit);
    assert!(first.len() + second.len() > byte_limit);
    let (controller, mut peer, _) = start_actor_with_inbound_limits_and_effect(
        &inventory,
        test_transport_limits(2, byte_limit, 4, 256),
        gate,
        TEST_TIMEOUT,
        HANDLED_OPERATION_DEADLINE,
        move |effect| {
            let effect_started = effect_started_tx.clone();
            let mut release = effect_release_rx.clone();
            async move {
                let _ = effect_started.send(effect);
                loop {
                    if *release.borrow_and_update() {
                        return CountableEffectDisposition::Handled;
                    }
                    if release.changed().await.is_err() {
                        return CountableEffectDisposition::Uncertain;
                    }
                }
            }
        },
    )
    .await;

    send_stanza(&mut peer, &first).await;
    let _started = timeout(TEST_TIMEOUT, effect_started_rx.recv())
        .await
        .expect("first stanza enters the effect owner")
        .expect("effect start signal");
    send_stanza(&mut peer, &second).await;

    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("second item exceeds bytes still held by the first effect"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::InboundOverload));
    assert_eq!(controller.handled_h(), 0);
    let _ = effect_release_tx.send(true);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn effect_owner_loss_during_reserved_admission_stops_without_losing_the_hole() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let (entered_tx, entered_rx) = oneshot::channel();
    let barrier = Arc::new(ConnectionActorGateBarrier::new());
    controller.pause_effect_admission_for_test(entered_tx, Arc::clone(&barrier));

    send_stanza(&mut peer, "<message><body>race</body></message>").await;
    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("actor owns a slot before assigning h")
        .expect("admission barrier signal");
    controller.abort_effect_owner_for_test();
    barrier.release();

    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("effect owner loss is observed and pumps/actor are joined"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.handled_h(), 0);
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn failed_or_uncertain_effect_stops_without_advancing_h() {
    for disposition in [
        CountableEffectDisposition::Failed,
        CountableEffectDisposition::Uncertain,
    ] {
        let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
        let gate = Arc::new(FlushGate::new());
        gate.release();
        let (controller, mut peer, _) = start_actor_with_inbound_limits_and_effect(
            &inventory,
            test_transport_limits(2, 2048, 4, 256),
            gate,
            TEST_TIMEOUT,
            HANDLED_OPERATION_DEADLINE,
            move |_| async move { disposition },
        )
        .await;

        send_stanza(&mut peer, "<message><body>unknown outcome</body></message>").await;
        assert_eq!(
            timeout(TEST_TIMEOUT, controller.wait_until_joined())
                .await
                .expect("failed effect stops the exact connection"),
            Some(CustodianState::Joined)
        );
        assert_eq!(controller.handled_h(), 0);
        assert_eq!(inventory.prune_joined(), 1);
    }
}

#[tokio::test]
async fn oversized_inbound_stanza_is_rejected_before_sequence_reservation() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, mut peer, _) = start_actor_with_inbound_limits_and_effect(
        &inventory,
        test_transport_limits(2, 64, 4, 256),
        gate,
        TEST_TIMEOUT,
        HANDLED_OPERATION_DEADLINE,
        |_| async { CountableEffectDisposition::Handled },
    )
    .await;
    let oversized = message_with_body(&"x".repeat(128));
    send_stanza(&mut peer, &oversized).await;

    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("oversized raw frame exceeds the inbound byte bound"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::InboundOverload));
    assert_eq!(controller.handled_h(), 0);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn padded_peer_sm_request_uses_actual_raw_bytes_for_inbound_budget() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let canonical_request = SmRequest::to_xml();
    let padded_request = "<r xmlns='urn:xmpp:sm:3'                              />";
    assert!(padded_request.len() > canonical_request.len());
    let (controller, mut peer, _) = start_actor_with_inbound_limits_and_effect(
        &inventory,
        test_transport_limits(2, canonical_request.len(), 4, 256),
        gate,
        TEST_TIMEOUT,
        HANDLED_OPERATION_DEADLINE,
        |_| async { CountableEffectDisposition::Handled },
    )
    .await;

    send_stanza(&mut peer, padded_request).await;
    assert_eq!(
        timeout(TEST_TIMEOUT, controller.wait_until_joined())
            .await
            .expect("raw padded request exceeds the configured inbound byte budget"),
        Some(CustodianState::Joined)
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::InboundOverload));
    assert_eq!(controller.handled_h(), 0);
    assert!(!controller.wait_for_inbound_forwarded(1).await);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caller_cancellation_before_factory_releases_the_reserved_slot_without_invoking_factory() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let hooks = ConnectionActorStartHooks {
        before_factory: Some(ConnectionActorStartBarrier::new(entered_tx, release_rx)),
        ..ConnectionActorStartHooks::default()
    };
    let (start_task, client, connection_ref_rx) =
        spawn_actor_start_with_hooks(inventory.clone(), hooks).await;

    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("startup reached the pre-factory barrier")
        .expect("barrier entry");
    assert_eq!(
        inventory.len(),
        1,
        "owner already consumes one reserved slot"
    );
    start_task.abort();
    let _ = start_task.await;
    let _ = release_tx.send(());

    timeout(TEST_TIMEOUT, async {
        while !inventory.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("canceled pre-factory owner releases its unused reservation");
    assert!(
        connection_ref_rx.await.is_err(),
        "factory callback was not invoked"
    );
    drop(client);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caller_cancellation_during_pending_factory_drops_factory_and_its_socket_halves() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let (server, client) = super::pumps::socket_pair().await;
    let (factory_entered_tx, factory_entered_rx) = oneshot::channel();
    let (half_dropped_tx, mut half_dropped_rx) = mpsc::unbounded_channel();
    let (release_writer_tx, release_writer_rx) = std_mpsc::sync_channel(1);
    let (release_reader_tx, release_reader_rx) = std_mpsc::sync_channel(1);
    let (flush_entered_tx, _flush_entered_rx) = oneshot::channel();
    let gate = Arc::new(FlushGate::new());
    let config = ReservedIoStartConfig::new(
        test_transport_limits(2, 2048, 2, 256),
        ShutdownProtocol::new(ConnectionWireFrame::Close, |error: &Error| {
            matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
        }),
        ConnectionWireFrame::encoded_len,
        ConnectionWireFrame::is_close,
        Duration::from_millis(500),
    );
    let owner_inventory = inventory.clone();
    let start_task = tokio::spawn(async move {
        let result = start_connection_actor_with_hooks(
            &owner_inventory,
            NodeLeaseIdentity::local_process(),
            HANDLED_OPERATION_DEADLINE,
            config,
            move |_| {
                let (writer, reader) = server.split();
                let writer = BlockingDropSink {
                    inner: writer,
                    id: 0,
                    dropped: half_dropped_tx.clone(),
                    release: release_writer_rx,
                };
                let reader = BlockingDropStream {
                    inner: reader,
                    id: 1,
                    dropped: half_dropped_tx,
                    release: release_reader_rx,
                };
                async move {
                    let _ = factory_entered_tx.send(());
                    std::future::pending::<()>().await;
                    let reader = reader
                        .filter_map(|incoming| async move {
                            incoming.ok().map(|_| {
                                Ok::<ConnectionWireFrame, Error>(ConnectionWireFrame::Close)
                            })
                        })
                        .boxed();
                    let writer = GatedFlushSink {
                        inner: writer,
                        gate,
                        entered: Some(flush_entered_tx),
                        pending_flush: false,
                    };
                    (reader, writer)
                }
            },
            |_| async { CountableEffectDisposition::Handled },
            ConnectionActorStartHooks::default(),
        )
        .await;
        drop(result);
    });

    timeout(TEST_TIMEOUT, factory_entered_rx)
        .await
        .expect("factory future started")
        .expect("factory entry signal");
    assert_eq!(
        inventory.len(),
        1,
        "pending factory is bounded by its reservation"
    );
    start_task.abort();
    let _ = start_task.await;
    let first_half = timeout(TEST_TIMEOUT, half_dropped_rx.recv())
        .await
        .expect("first captured socket half begins dropping")
        .expect("first half drop ID");
    assert_eq!(
        inventory.len(),
        1,
        "capacity remains held during first half Drop"
    );
    match first_half {
        0 => {
            let _ = release_writer_tx.send(());
        }
        1 => {
            let _ = release_reader_tx.send(());
        }
        other => panic!("unexpected socket half ID {other}"),
    }
    let second_half = timeout(TEST_TIMEOUT, half_dropped_rx.recv())
        .await
        .expect("second captured socket half begins dropping")
        .expect("second half drop ID");
    assert_ne!(first_half, second_half);
    assert_eq!(
        inventory.len(),
        1,
        "capacity remains held during second half Drop"
    );
    match second_half {
        0 => {
            let _ = release_writer_tx.send(());
        }
        1 => {
            let _ = release_reader_tx.send(());
        }
        other => panic!("unexpected socket half ID {other}"),
    }
    timeout(TEST_TIMEOUT, async {
        while !inventory.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pre-activation cancellation releases inventory capacity");
    drop(client);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caller_cancellation_after_pump_handoff_keeps_inventory_until_both_pumps_join() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let hooks = ConnectionActorStartHooks {
        after_pumps_handed_off: Some(ConnectionActorStartBarrier::new(entered_tx, release_rx)),
        ..ConnectionActorStartHooks::default()
    };
    let (start_task, client, connection_ref_rx) =
        spawn_actor_start_with_hooks(inventory.clone(), hooks).await;

    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("both pump handles reached the reaper handoff barrier")
        .expect("barrier entry");
    let connection_ref = timeout(TEST_TIMEOUT, connection_ref_rx)
        .await
        .expect("factory ran after reservation")
        .expect("exact connection identity");
    let observer = inventory
        .observe(&connection_ref)
        .expect("active exact connection remains indexed");
    start_task.abort();
    let _ = start_task.await;
    timeout(TEST_TIMEOUT, async {
        while observer.stop_reason() != Some(StopReason::Requested) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("caller loss requests stop for the exact physical connection");
    assert_eq!(
        inventory.prune_joined(),
        0,
        "unjoined pumps retain capacity"
    );
    assert!(inventory.observe(&connection_ref).is_some());
    let _ = release_tx.send(());
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("both started pump handles join"),
        CustodianState::Joined
    );
    assert_eq!(inventory.prune_joined(), 1);
    drop(client);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caller_cancellation_after_actor_start_joins_registered_auxiliary_cleanup() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let hooks = ConnectionActorStartHooks {
        after_actor_started: Some(ConnectionActorStartBarrier::new(entered_tx, release_rx)),
        ..ConnectionActorStartHooks::default()
    };
    let (start_task, client, connection_ref_rx) =
        spawn_actor_start_with_hooks(inventory.clone(), hooks).await;

    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("Kameo actor started before input and effect handoff")
        .expect("barrier entry");
    let connection_ref = timeout(TEST_TIMEOUT, connection_ref_rx)
        .await
        .expect("factory ran after reservation")
        .expect("exact connection identity");
    let observer = inventory
        .observe(&connection_ref)
        .expect("active exact connection remains indexed");
    start_task.abort();
    let _ = start_task.await;
    timeout(TEST_TIMEOUT, async {
        while observer.stop_reason() != Some(StopReason::Requested) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("caller loss requests stop while actor startup is paused");
    assert_eq!(
        inventory.prune_joined(),
        0,
        "startup handoff is still incomplete"
    );
    let _ = release_tx.send(());
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("registered cleanup task and both pumps join"),
        CustodianState::Joined
    );
    assert!(observer.auxiliary_join_started());
    assert_eq!(inventory.prune_joined(), 1);
    drop(client);
}
