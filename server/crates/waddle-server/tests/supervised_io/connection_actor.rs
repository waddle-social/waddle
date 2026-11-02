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
    ConnectionActorStartError, ConnectionWireFrame, PanicConnectionActor, PauseConnectionActor,
    RequestAcknowledgement, ReserveHandledInbound, RetireBinding, SnapshotConnection,
    StopConnectionActor,
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
    start_actor_with_inbound_limit(
        inventory,
        8,
        max_outbound_count,
        max_outbound_bytes,
        gate,
        retirement_deadline,
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
) -> (
    waddle_server::test_support::connection_actor::ConnectionActorController,
    ClientSocket,
    oneshot::Receiver<()>,
) {
    let (server, client) = super::pumps::socket_pair().await;
    let (entered_tx, entered_rx) = oneshot::channel();
    let config = ReservedIoStartConfig::new(
        TransportLimits::new(
            Limits::new(max_inbound_count, 2048).expect("inbound bounds"),
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
        .expect("reserve first handled-prefix message");
    let second = controller
        .actor()
        .ask(ReserveHandledInbound)
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
        .ask(ReserveHandledInbound)
        .send()
        .await
        .expect("reserve first sequence message");
    let second = controller
        .actor()
        .ask(ReserveHandledInbound)
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
    let binding_a = BindingRef::fresh(connection_ref.clone());
    let binding_b = BindingRef::fresh(connection_ref.clone());

    controller
        .actor()
        .ask(BindConnection(binding_a.clone()))
        .send()
        .await
        .expect("binding A is accepted by the live actor");
    let sequence_a = controller
        .actor()
        .ask(ReserveHandledInbound)
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
    assert_eq!(
        controller
            .actor()
            .ask(BindConnection(binding_b.clone()))
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
        ConnectionActorError::SequenceNotPending
    );
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
        .ask(ReserveHandledInbound)
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
        .ask(ReserveHandledInbound)
        .send()
        .await
        .expect("reserve A message");
    let sequence_a_for_b = sequence_a.clone();
    let sequence_b = controller_b
        .actor()
        .ask(ReserveHandledInbound)
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
                .ask(ReserveHandledInbound)
                .send()
                .await
                .expect("reserve handled-prefix message"),
        );
    }
    assert_eq!(
        controller
            .actor()
            .ask(ReserveHandledInbound)
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
            .ask(ReserveHandledInbound)
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
    let (controller, mut peer, _) =
        start_actor_with_inbound_limit(&inventory, 1, 4, 256, gate, TEST_TIMEOUT).await;

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

    let _ = release_tx.send(());
    controller
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("paused actor resumes and drains its retained item");
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
