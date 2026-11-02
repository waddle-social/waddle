use futures::{Sink, SinkExt, StreamExt};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use waddle_server::test_support::connection_actor::{
    start_connection_actor, BindConnection, CompleteHandledInbound, ConnectionActorError,
    ConnectionActorStartError, ConnectionWireFrame, PanicConnectionActor, RequestAcknowledgement,
    ReserveHandledInbound, RetireBinding, SnapshotConnection, StopConnectionActor,
};
use waddle_server::test_support::supervised_io::{
    AdmissionError, CustodianInventoryError, CustodianState, IoCustodianInventory, Limits,
    OutboundTerminal, ReservedIoStartConfig, ShutdownProtocol, StopReason, TransportLimits,
};
use waddle_xmpp::ownership::{BindingRef, NodeLeaseIdentity};
use waddle_xmpp::stream_management::{SmAck, SmRequest, SmStanza};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

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
    let (server, client) = super::pumps::socket_pair().await;
    let (entered_tx, entered_rx) = oneshot::channel();
    let config = ReservedIoStartConfig::new(
        TransportLimits::new(
            Limits::new(8, 2048).expect("inbound bounds"),
            Limits::new(max_outbound_count, max_outbound_bytes).expect("outbound bounds"),
        ),
        ShutdownProtocol::new(ConnectionWireFrame::Close, |error: &Error| {
            matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
        }),
        ConnectionWireFrame::encoded_len,
        ConnectionWireFrame::is_close,
        retirement_deadline,
    );
    let controller = start_connection_actor(
        inventory,
        NodeLeaseIdentity::local_process(),
        config,
        move |_| async move {
            let (writer, reader) = server.split();
            let reader = reader
                .filter_map(|incoming| async move {
                    match incoming {
                        Ok(Message::Text(text)) => match SmStanza::parse(text.as_str()) {
                            Some(SmStanza::Request) => {
                                Some(Ok(ConnectionWireFrame::RequestAcknowledgement(SmRequest)))
                            }
                            _ => None,
                        },
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
    )
    .await
    .expect("reserved connection actor starts");
    (controller, client, entered_rx)
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
        .ask(ReserveHandledInbound)
        .send()
        .await
        .expect("reserve first handled-prefix slot");
    let second = controller
        .actor()
        .ask(ReserveHandledInbound)
        .send()
        .await
        .expect("reserve second handled-prefix slot");
    assert_eq!(
        controller
            .actor()
            .ask(CompleteHandledInbound(second))
            .send()
            .await
            .expect("complete later operation"),
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
            .expect("complete earlier operation"),
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
        .ask(ReserveHandledInbound)
        .send()
        .await
        .expect("reserve first sequence");
    let second = controller
        .actor()
        .ask(ReserveHandledInbound)
        .send()
        .await
        .expect("reserve second sequence");
    controller
        .actor()
        .ask(CompleteHandledInbound(second))
        .send()
        .await
        .expect("complete later sequence");

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
async fn binding_retirement_is_separate_from_physical_connection_and_stale_completion() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let gate = Arc::new(FlushGate::new());
    gate.release();
    let (controller, _peer, _) = start_actor(&inventory, 4, 256, gate, TEST_TIMEOUT).await;
    let connection_ref = controller.connection_ref().clone();
    let binding_a = BindingRef::fresh(connection_ref.clone());
    let binding_b = BindingRef::fresh(connection_ref.clone());

    controller
        .actor()
        .ask(BindConnection(binding_a.clone()))
        .send()
        .await
        .expect("binding A is accepted by the live actor");
    assert!(controller
        .actor()
        .ask(RetireBinding(binding_a.clone()))
        .send()
        .await
        .expect("retire A"));
    controller
        .actor()
        .ask(BindConnection(binding_b.clone()))
        .send()
        .await
        .expect("binding B reuses the same physical socket");
    assert!(
        !controller
            .actor()
            .ask(RetireBinding(binding_a))
            .send()
            .await
            .expect("stale A completion is processed"),
        "stale A completion must not retire B"
    );
    let snapshot = controller
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("snapshot actor-owned state");
    assert_eq!(snapshot.connection_ref, connection_ref);
    assert_eq!(snapshot.binding_ref, Some(binding_b));

    controller.request_stop(StopReason::Requested);
    assert_eq!(
        controller.wait_until_joined().await,
        Some(CustodianState::Joined)
    );
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
