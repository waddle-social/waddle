use futures::{Sink, SinkExt, StreamExt};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use waddle_server::test_support::connection_actor::{
    start_connection_actor, start_connection_actor_with_effect, CompleteHandledInbound,
    CompleteHandledInboundAtLifecycleGate, CompleteHandledInboundBeforeLifecycleGate,
    ConfirmBindingInstall, ConnectionActorError, ConnectionActorGateBarrier,
    ConnectionActorOperationId, ConnectionActorStartError, ConnectionWireFrame,
    CountableEffectDisposition, CountableInboundEffect, InstallBinding, PanicConnectionActor,
    PauseConnectionActor, PrepareBinding, RequestAcknowledgement, ReserveHandledInbound,
    RetireBinding, SeedInboundCounterForTest, SnapshotConnection, StopConnectionActor,
};
use waddle_server::test_support::supervised_io::{
    AdmissionError, CustodianInventoryError, CustodianState, IoCustodianInventory, Limits,
    OutboundTerminal, ReservedIoStartConfig, ShutdownProtocol, StopReason, TransportLimits,
};
use waddle_xmpp::ownership::NodeLeaseIdentity;
use waddle_xmpp::protocol::frame::{parse_frame, InboundFrame, ParseError};
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
            ConnectionWireFrame::RequestAcknowledgement(_) => {
                Message::Text(SmRequest::to_xml().into())
            }
            ConnectionWireFrame::Acknowledgement(ack) => Message::Text(ack.to_xml().into()),
            ConnectionWireFrame::CountableStanza { .. }
            | ConnectionWireFrame::RejectedInbound { .. } => {
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
    match parse_frame(text) {
        Ok(InboundFrame::Stanza(stanza)) => ConnectionWireFrame::CountableStanza {
            stanza: Arc::new(*stanza),
            wire_bytes,
        },
        Err(ParseError::UnknownRoot(root)) if root == "r" => {
            if matches!(SmStanza::parse(text), Some(SmStanza::Request)) {
                ConnectionWireFrame::RequestAcknowledgement(SmRequest)
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
            actor_ack(&controller).await,
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
                                    .then_some(Ok(ConnectionWireFrame::RequestAcknowledgement(
                                        SmRequest,
                                    )))
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
async fn failed_reservation_does_not_call_the_connection_pump_factory() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let first = start_actor(
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
    .await
    .0;
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
                                .then_some(Ok(ConnectionWireFrame::RequestAcknowledgement(
                                    SmRequest,
                                )))
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
