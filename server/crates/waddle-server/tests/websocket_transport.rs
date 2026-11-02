//! Dark transport boundary tests using controllable I/O halves.

#![cfg(feature = "test-support")]

use axum::extract::ws::{CloseFrame, Message};
use futures::task::AtomicWaker;
use futures::{Sink, SinkExt, Stream, StreamExt};
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use waddle_server::clustering::NodeLifecycle;
use waddle_server::websocket_transport::{
    start_websocket_transport, AdmissionError, CustodianState, InboundTerminal, InputError,
    IoCustodianHandle, IoCustodianInventory, Limits, OutputError, StopReason, TestSocketAdmission,
    TransportLimits, WebSocketInput, WebSocketOutput, WriteProgress,
};
use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct SinkState {
    stall_ready: AtomicBool,
    stall_flush: AtomicBool,
    fail_flush: AtomicBool,
    fail_start_after_close: AtomicBool,
    panic_in_start: AtomicBool,
    block_in_start: AtomicBool,
    start_entered: AtomicBool,
    start_released: Mutex<bool>,
    start_changed: Condvar,
    ready_polls: AtomicUsize,
    block_in_flush: AtomicBool,
    blocking_entered: AtomicBool,
    blocking_released: Mutex<bool>,
    blocking_changed: Condvar,
    starts: AtomicUsize,
    dropped: AtomicBool,
    frames: Mutex<Vec<Message>>,
    waker: AtomicWaker,
    changed: Notify,
}

impl SinkState {
    fn release(&self) {
        self.stall_ready.store(false, Ordering::Release);
        self.stall_flush.store(false, Ordering::Release);
        *self.blocking_released.lock().expect("gate lock") = true;
        self.blocking_changed.notify_all();
        *self.start_released.lock().expect("start gate lock") = true;
        self.start_changed.notify_all();
        self.waker.wake();
    }

    async fn wait_until_blocked(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.blocking_entered.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    async fn wait_for_ready_poll(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.ready_polls.load(Ordering::Acquire) > 0 {
                return;
            }
            notified.await;
        }
    }

    async fn wait_for_starts(&self, count: usize) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.starts.load(Ordering::Acquire) >= count {
                return;
            }
            notified.await;
        }
    }

    async fn wait_for_blocked_start(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.start_entered.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

struct ControlledSink(Arc<SinkState>);

impl Drop for ControlledSink {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::Release);
    }
}

impl Sink<Message> for ControlledSink {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.ready_polls.fetch_add(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
        self.0.waker.register(cx.waker());
        if self.0.stall_ready.load(Ordering::Acquire) {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn start_send(self: Pin<&mut Self>, frame: Message) -> io::Result<()> {
        self.0.frames.lock().expect("frames lock").push(frame);
        self.0.starts.fetch_add(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
        if self.0.panic_in_start.load(Ordering::Acquire) {
            panic!("start_send panicked after accepting a frame");
        }
        if self.0.fail_start_after_close.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "send after close",
            ));
        }
        if self.0.block_in_start.load(Ordering::Acquire) {
            let mut released = self.0.start_released.lock().expect("start gate lock");
            self.0.start_entered.store(true, Ordering::Release);
            self.0.changed.notify_waiters();
            while !*released {
                released = self
                    .0
                    .start_changed
                    .wait(released)
                    .expect("start gate wait");
            }
        }
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.0.block_in_flush.load(Ordering::Acquire) {
            let mut released = self.0.blocking_released.lock().expect("gate lock");
            self.0.blocking_entered.store(true, Ordering::Release);
            self.0.changed.notify_waiters();
            while !*released {
                released = self.0.blocking_changed.wait(released).expect("gate wait");
            }
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.0.waker.register(cx.waker());
        if self.0.fail_flush.load(Ordering::Acquire) {
            return Poll::Ready(Err(io::Error::other("flush broke")));
        }
        if self.0.stall_flush.load(Ordering::Acquire) {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct ControlledReader {
    rx: mpsc::Receiver<io::Result<Message>>,
    dropped: Arc<AtomicBool>,
}

impl Stream for ControlledReader {
    type Item = io::Result<Message>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().rx.poll_recv(cx)
    }
}

impl Drop for ControlledReader {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

struct Harness {
    identity: ConnectionRef,
    inventory: IoCustodianInventory,
    custodian: IoCustodianHandle,
    input: WebSocketInput<io::Error>,
    output: WebSocketOutput<io::Error>,
    peer: mpsc::Sender<io::Result<Message>>,
    sink: Arc<SinkState>,
    reader_dropped: Arc<AtomicBool>,
    lifecycle: NodeLifecycle,
    process_stop: CancellationToken,
    live_interrupt: CancellationToken,
}

async fn start(inbound: (usize, usize), outbound: (usize, usize)) -> Harness {
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let limits = TransportLimits::new(
        Limits::new(inbound.0, inbound.1).expect("inbound limits"),
        Limits::new(outbound.0, outbound.1).expect("outbound limits"),
    );
    let (peer, rx) = mpsc::channel(16);
    let reader_dropped = Arc::new(AtomicBool::new(false));
    let reader = ControlledReader {
        rx,
        dropped: Arc::clone(&reader_dropped),
    };
    let sink = Arc::new(SinkState::default());
    let writer = ControlledSink(Arc::clone(&sink));
    let lifecycle = NodeLifecycle::new();
    let process_stop = CancellationToken::new();
    let live_interrupt = CancellationToken::new();
    let admission = TestSocketAdmission::standalone(
        &lifecycle,
        &inventory,
        process_stop.clone(),
        live_interrupt.clone(),
    )
    .expect("admission");
    let identity = admission.connection_ref().clone();
    let expected = identity.clone();
    let (custodian, input, output) = start_websocket_transport(
        admission,
        limits,
        Duration::from_millis(250),
        |error: &io::Error| error.kind() == io::ErrorKind::BrokenPipe,
        move |connection_ref| {
            assert_eq!(connection_ref, &expected);
            async move { (reader, writer) }
        },
    )
    .await
    .expect("start transport");
    Harness {
        identity,
        inventory,
        custodian,
        input,
        output,
        peer,
        sink,
        reader_dropped,
        lifecycle,
        process_stop,
        live_interrupt,
    }
}

async fn assert_joined(h: &Harness) {
    assert_eq!(
        timeout(TIMEOUT, h.custodian.wait_until_joined())
            .await
            .expect("joined"),
        CustodianState::Joined
    );
    assert!(h.custodian.is_for(&h.identity));
    assert!(h
        .inventory
        .observe(&h.identity)
        .expect("indexed")
        .is_for(&h.identity));
    assert!(h.reader_dropped.load(Ordering::Acquire));
    assert!(h.sink.dropped.load(Ordering::Acquire));
}

#[tokio::test]
async fn ack_progress_and_independent_stop_survive_a_stalled_write() {
    let mut h = start((4, 128), (1, 128)).await;
    h.sink.stall_flush.store(true, Ordering::Release);
    let receipt = h
        .output
        .submit(
            h.output
                .data_frame(Message::text("<message id='first'/>"))
                .expect("data frame"),
        )
        .await
        .expect("admitted");
    timeout(TIMEOUT, h.sink.wait_for_starts(1))
        .await
        .expect("sink attempt");
    assert!(receipt.is_for(&h.identity));
    let successor = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    assert!(!receipt.is_for(&successor));
    assert_eq!(receipt.wait_for_attempt().await, WriteProgress::Attempted);
    let ack = Message::text("<a xmlns='urn:xmpp:sm:3' h='1'/>");
    h.peer.send(Ok(ack.clone())).await.expect("send ack");
    assert_eq!(
        timeout(TIMEOUT, h.input.next())
            .await
            .expect("inbound progress")
            .expect("frame")
            .expect("no error"),
        ack
    );
    {
        let next = h.output.submit(
            h.output
                .data_frame(Message::text("<message id='next'/>"))
                .expect("data frame"),
        );
        tokio::pin!(next);
        assert!(timeout(Duration::from_millis(40), &mut next).await.is_err());
        assert_eq!(
            h.custodian.stop_reason(),
            None,
            "full data queue is backpressure"
        );
        h.custodian.request_stop(StopReason::Requested);
        assert!(matches!(
            next.await,
            Err(OutputError::Admission(AdmissionError::Stopped))
        ));
    }
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Uncertain
    );
    assert_joined(&h).await;
}

#[tokio::test]
async fn count_and_byte_saturation_backpressure_then_preserve_exact_frame_bytes() {
    let mut h = start((2, 64), (2, 5)).await;
    h.sink.stall_flush.store(true, Ordering::Release);
    let first = h
        .output
        .submit(
            h.output
                .data_frame(Message::text("éx"))
                .expect("data frame"),
        )
        .await
        .expect("first");
    timeout(TIMEOUT, h.sink.wait_for_starts(1))
        .await
        .expect("first attempt");
    let second = h
        .output
        .submit(
            h.output
                .data_frame(Message::text("yz"))
                .expect("data frame"),
        )
        .await
        .expect("second");
    {
        let third = h
            .output
            .submit(h.output.data_frame(Message::text("!")).expect("data frame"));
        tokio::pin!(third);
        assert!(timeout(Duration::from_millis(40), &mut third)
            .await
            .is_err());
        assert_eq!(h.custodian.stop_reason(), None);
        h.sink.release();
        third.await.expect("third after capacity");
    }
    timeout(TIMEOUT, h.output.flush())
        .await
        .expect("flush timed out")
        .expect("flush");
    assert_eq!(first.progress(), WriteProgress::Completed);
    assert_eq!(second.progress(), WriteProgress::Completed);
    assert_eq!(
        *h.sink.frames.lock().expect("frames"),
        vec![Message::text("éx"), Message::text("yz"), Message::text("!")]
    );
    h.custodian.request_stop(StopReason::Requested);
    assert_joined(&h).await;
}

#[tokio::test]
async fn close_and_control_frames_are_ordered_even_with_small_inbound_capacity() {
    let mut h = start((1, 64), (2, 64)).await;
    let close = Message::Close(Some(CloseFrame {
        code: 1001,
        reason: "away".into(),
    }));
    let frames = [
        Message::text("first"),
        Message::Ping(vec![1, 2].into()),
        Message::Pong(vec![3].into()),
        close.clone(),
    ];
    for frame in &frames {
        h.peer.send(Ok(frame.clone())).await.expect("peer frame");
    }
    for expected in frames {
        assert_eq!(
            timeout(TIMEOUT, h.input.next())
                .await
                .expect("input")
                .expect("frame")
                .expect("no error"),
            expected
        );
    }
    assert!(h.input.next().await.is_none());
    assert_joined(&h).await;
    assert_eq!(h.custodian.stop_reason(), Some(StopReason::PeerClosed));
}

#[tokio::test]
async fn input_errors_and_oversized_close_are_preserved() {
    let mut h = start((1, 64), (2, 64)).await;
    h.peer
        .send(Ok(Message::text("before ping")))
        .await
        .expect("text");
    h.peer
        .send(Ok(Message::Ping(vec![9].into())))
        .await
        .expect("ping");
    h.peer
        .send(Err(io::Error::other("read broke")))
        .await
        .expect("read error");
    assert_eq!(
        h.input.next().await.expect("text").expect("frame"),
        Message::text("before ping")
    );
    assert_eq!(
        h.input.next().await.expect("ping").expect("frame"),
        Message::Ping(vec![9].into())
    );
    match h.input.next().await.expect("error") {
        Err(InputError::Transport(error)) => assert_eq!(error.to_string(), "read broke"),
        other => panic!("expected exact transport error, got {other:?}"),
    }
    assert_joined(&h).await;
    assert_eq!(
        h.custodian.terminal_summary().expect("summary").inbound,
        InboundTerminal::IoFailure
    );

    let mut h = start((2, 4), (2, 64)).await;
    h.peer
        .send(Ok(Message::Close(Some(CloseFrame {
            code: 1000,
            reason: "abc".into(),
        }))))
        .await
        .expect("close");
    assert!(matches!(
        h.input.next().await,
        Some(Err(InputError::FrameTooLarge {
            size: 5,
            max_bytes: 4
        }))
    ));
    assert_joined(&h).await;
}

#[tokio::test]
async fn graceful_close_sends_once_and_waits_for_both_halves() {
    let mut h = start((2, 64), (2, 64)).await;
    let close = h.output.transport_close_frame(None).expect("close frame");
    let close = h.output.submit(close).await.expect("local close");
    assert_eq!(close.wait_for_terminal().await, WriteProgress::Completed);
    assert_eq!(h.sink.starts.load(Ordering::Acquire), 1);
    {
        let closing = h.output.close();
        tokio::pin!(closing);
        assert!(
            timeout(Duration::from_millis(40), &mut closing)
                .await
                .is_err(),
            "must await peer close"
        );
        h.peer
            .send(Ok(Message::Close(None)))
            .await
            .expect("peer close");
        timeout(TIMEOUT, &mut closing)
            .await
            .expect("close wait")
            .expect("close success");
    }
    assert_joined(&h).await;
    assert_eq!(
        *h.sink.frames.lock().expect("frames"),
        vec![Message::Close(None)]
    );
}

#[tokio::test]
async fn graceful_shutdown_unblocks_capacity_waiters_without_overload() {
    let mut h = start((2, 64), (1, 64)).await;
    h.sink.stall_flush.store(true, Ordering::Release);
    let receipt = h
        .output
        .submit(
            h.output
                .data_frame(Message::text("first"))
                .expect("data frame"),
        )
        .await
        .expect("first");
    timeout(TIMEOUT, h.sink.wait_for_starts(1))
        .await
        .expect("attempt");
    {
        let waiting = h.output.submit(
            h.output
                .data_frame(Message::text("second"))
                .expect("data frame"),
        );
        tokio::pin!(waiting);
        assert!(timeout(Duration::from_millis(40), &mut waiting)
            .await
            .is_err());
        h.custodian.request_graceful_shutdown();
        assert!(matches!(
            waiting.await,
            Err(OutputError::Admission(AdmissionError::Shutdown))
        ));
    }
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Uncertain
    );
    assert_joined(&h).await;
}

struct ReleaseBlockingSinkOnDrop(Arc<SinkState>);

impl Drop for ReleaseBlockingSinkOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_deadline_returns_while_noncooperative_pump_remains_indexed() {
    let mut h = start((2, 64), (1, 64)).await;
    let _release = ReleaseBlockingSinkOnDrop(Arc::clone(&h.sink));
    h.sink.block_in_flush.store(true, Ordering::Release);
    let receipt = h
        .output
        .submit(
            h.output
                .data_frame(Message::text("in flight"))
                .expect("data frame"),
        )
        .await
        .expect("admit");
    timeout(TIMEOUT, h.sink.wait_until_blocked())
        .await
        .expect("writer blocked");
    assert_eq!(receipt.progress(), WriteProgress::Attempted);

    assert!(matches!(
        timeout(TIMEOUT, h.output.close()).await,
        Ok(Err(OutputError::RetirementFailed))
    ));
    assert_ne!(h.custodian.state(), CustodianState::Joined);
    assert!(!h.sink.dropped.load(Ordering::Acquire));
    assert!(h
        .inventory
        .observe(&h.identity)
        .expect("still indexed")
        .is_for(&h.identity));
    timeout(TIMEOUT, async {
        while h.custodian.state() != CustodianState::CleanupPending {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cleanup pending is observable");
    assert!(!h.sink.dropped.load(Ordering::Acquire));

    h.sink.release();
    assert_joined(&h).await;
    assert_eq!(receipt.wait_for_terminal().await, WriteProgress::Uncertain);
}

#[tokio::test]
async fn in_memory_websocket_halves_preserve_raw_text_and_peer_close() {
    use futures::future;
    use tokio::io::duplex;
    use tokio_tungstenite::tungstenite::error::ProtocolError;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::protocol::{CloseFrame as PeerCloseFrame, Role};
    use tokio_tungstenite::tungstenite::{Error as PeerError, Message as PeerMessage};
    use tokio_tungstenite::WebSocketStream;

    fn inbound(frame: PeerMessage) -> Message {
        match frame {
            PeerMessage::Text(text) => Message::text(text.to_string()),
            PeerMessage::Binary(bytes) => Message::Binary(bytes),
            PeerMessage::Ping(bytes) => Message::Ping(bytes),
            PeerMessage::Pong(bytes) => Message::Pong(bytes),
            PeerMessage::Close(frame) => Message::Close(frame.map(|frame| CloseFrame {
                code: frame.code.into(),
                reason: frame.reason.to_string().into(),
            })),
            PeerMessage::Frame(_) => panic!("raw control frame is not a complete message"),
        }
    }
    fn outbound(frame: Message) -> PeerMessage {
        match frame {
            Message::Text(text) => PeerMessage::Text(text.to_string().into()),
            Message::Binary(bytes) => PeerMessage::Binary(bytes),
            Message::Ping(bytes) => PeerMessage::Ping(bytes),
            Message::Pong(bytes) => PeerMessage::Pong(bytes),
            Message::Close(frame) => PeerMessage::Close(frame.map(|frame| PeerCloseFrame {
                code: CloseCode::from(frame.code),
                reason: frame.reason.to_string().into(),
            })),
        }
    }

    let (server_io, peer_io) = duplex(4096);
    let server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
    let mut peer = WebSocketStream::from_raw_socket(peer_io, Role::Client, None).await;
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let admission = TestSocketAdmission::standalone(
        &NodeLifecycle::new(),
        &inventory,
        CancellationToken::new(),
        CancellationToken::new(),
    )
    .expect("admission");
    let identity = admission.connection_ref().clone();
    let limits = TransportLimits::new(
        Limits::new(4, 256).expect("inbound"),
        Limits::new(4, 256).expect("outbound"),
    );
    let (custodian, mut input, mut output) = start_websocket_transport(
        admission,
        limits,
        Duration::from_secs(1),
        |error: &PeerError| matches!(error, PeerError::Protocol(ProtocolError::SendAfterClosing)),
        move |_| async move {
            let (writer, reader) = server.split();
            let reader = reader.map(|result| result.map(inbound));
            let writer = writer.with(|message: Message| future::ready(Ok(outbound(message))));
            (reader, writer)
        },
    )
    .await
    .expect("start");

    let inbound = "<a xmlns='urn:xmpp:sm:3' h='42'/>";
    peer.send(PeerMessage::Text(inbound.into()))
        .await
        .expect("peer text");
    assert_eq!(
        input.next().await.expect("input").expect("frame"),
        Message::text(inbound)
    );
    let exact = "<message xml:lang='en'>héllo &amp; bye</message>";
    let frame = output.data_frame(Message::text(exact)).expect("data frame");
    let receipt = output.submit(frame).await.expect("submit");
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Completed
    );
    assert_eq!(
        timeout(TIMEOUT, peer.next())
            .await
            .expect("peer read")
            .expect("frame")
            .expect("valid"),
        PeerMessage::Text(exact.into())
    );
    peer.send(PeerMessage::Close(Some(PeerCloseFrame {
        code: CloseCode::Away,
        reason: "bye".into(),
    })))
    .await
    .expect("peer close");
    assert_eq!(
        timeout(TIMEOUT, input.next())
            .await
            .expect("input close")
            .expect("close")
            .expect("valid"),
        Message::Close(Some(CloseFrame {
            code: 1001,
            reason: "bye".into()
        }))
    );
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("reply"),
        Some(Ok(PeerMessage::Close(_)))
    ));
    assert_eq!(
        timeout(TIMEOUT, custodian.wait_until_joined())
            .await
            .expect("joined"),
        CustodianState::Joined
    );
    assert!(inventory
        .observe(&identity)
        .expect("indexed")
        .is_for(&identity));
}

#[tokio::test]
async fn cancelled_before_sink_attempt_is_not_attempted() {
    let mut h = start((2, 64), (2, 64)).await;
    h.sink.stall_ready.store(true, Ordering::Release);
    let receipt = h
        .output
        .submit(
            h.output
                .data_frame(Message::text("queued"))
                .expect("data frame"),
        )
        .await
        .expect("admit");
    timeout(TIMEOUT, h.sink.wait_for_ready_poll())
        .await
        .expect("readiness polled");
    assert_eq!(receipt.progress(), WriteProgress::Admitted);
    h.custodian.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::NotAttempted
    );
    assert_joined(&h).await;
    assert_eq!(h.sink.starts.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn write_failure_keeps_original_error_and_uncertain_receipt() {
    let mut h = start((2, 64), (2, 64)).await;
    h.sink.fail_flush.store(true, Ordering::Release);
    let receipt = h
        .output
        .submit(
            h.output
                .data_frame(Message::text("written partially"))
                .expect("data frame"),
        )
        .await
        .expect("admit");
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Uncertain
    );
    match h.output.flush().await {
        Err(OutputError::Transport(error)) => assert_eq!(error.to_string(), "flush broke"),
        other => panic!("expected original write error, got {other:?}"),
    }
    assert_joined(&h).await;
}

#[tokio::test]
async fn sink_close_without_prior_frame_sends_one_close_and_observes_peer() {
    let mut h = start((2, 64), (2, 64)).await;
    {
        let closing = h.output.close();
        tokio::pin!(closing);
        tokio::select! {
            result = &mut closing => panic!("closed before peer reply: {result:?}"),
            () = h.sink.wait_for_starts(1) => {}
        }
        h.peer
            .send(Ok(Message::Close(None)))
            .await
            .expect("peer close");
        timeout(TIMEOUT, &mut closing)
            .await
            .expect("close wait")
            .expect("close success");
    }
    assert_joined(&h).await;
    assert_eq!(
        *h.sink.frames.lock().expect("frames"),
        vec![Message::Close(None)]
    );
}

#[tokio::test]
async fn generation_loss_and_recovery_cannot_reauthorize_a_parked_frame() {
    let mut h = start((2, 512), (2, 512)).await;
    h.sink.stall_ready.store(true, Ordering::Release);
    let frame = h
        .output
        .data_frame(Message::text("old generation"))
        .expect("frame");
    let receipt = h.output.submit(frame).await.expect("admitted");
    timeout(TIMEOUT, h.sink.wait_for_ready_poll())
        .await
        .expect("readiness polled");
    h.lifecycle.begin_fenced_recovery();
    h.lifecycle.serve();
    h.sink.release();
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::NotAttempted
    );
    assert_eq!(h.sink.starts.load(Ordering::Acquire), 0);
    h.custodian.request_stop(StopReason::Requested);
    assert_joined(&h).await;
}

#[tokio::test]
async fn start_send_close_state_error_is_still_uncertain() {
    let mut h = start((2, 512), (2, 512)).await;
    h.sink.fail_start_after_close.store(true, Ordering::Release);
    let frame = h
        .output
        .data_frame(Message::text("attempted"))
        .expect("frame");
    let receipt = h.output.submit(frame).await.expect("admitted");
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Uncertain
    );
    h.peer
        .send(Ok(Message::Close(None)))
        .await
        .expect("peer close");
    assert_joined(&h).await;
}

#[tokio::test]
async fn process_stop_and_live_interruption_cancel_parked_readiness() {
    for live_detach in [false, true] {
        let mut h = start((2, 512), (2, 512)).await;
        h.sink.stall_ready.store(true, Ordering::Release);
        let frame = h.output.data_frame(Message::text("parked")).expect("frame");
        let receipt = h.output.submit(frame).await.expect("admitted");
        timeout(TIMEOUT, h.sink.wait_for_ready_poll())
            .await
            .expect("readiness polled");
        if live_detach {
            h.live_interrupt.cancel();
        } else {
            h.process_stop.cancel();
        }
        assert_eq!(
            timeout(TIMEOUT, receipt.wait_for_terminal())
                .await
                .expect("receipt"),
            WriteProgress::NotAttempted
        );
        assert_eq!(h.sink.starts.load(Ordering::Acquire), 0);
        h.custodian.request_stop(StopReason::Requested);
        assert_joined(&h).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn lifecycle_transition_waits_for_the_synchronous_sink_enqueue() {
    let mut h = start((2, 512), (2, 512)).await;
    let _release = ReleaseBlockingSinkOnDrop(Arc::clone(&h.sink));
    h.sink.block_in_start.store(true, Ordering::Release);
    h.sink.stall_flush.store(true, Ordering::Release);
    let frame = h
        .output
        .data_frame(Message::text("in start_send"))
        .expect("frame");
    let receipt = h.output.submit(frame).await.expect("admitted");
    timeout(TIMEOUT, h.sink.wait_for_blocked_start())
        .await
        .expect("start entered");
    let lifecycle = h.lifecycle.clone();
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let mut transition = tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        lifecycle.begin_drain();
    });
    let mut output = h.output;
    let (phase_started, phase_started_rx) = tokio::sync::oneshot::channel();
    let mut terminal = tokio::task::spawn_blocking(move || {
        let _ = phase_started.send(());
        let result = output.begin_shutdown();
        (output, result)
    });
    started_rx.await.expect("transition started");
    phase_started_rx.await.expect("terminal transition started");
    assert!(timeout(Duration::from_millis(40), &mut transition)
        .await
        .is_err());
    assert!(timeout(Duration::from_millis(40), &mut terminal)
        .await
        .is_err());
    *h.sink.start_released.lock().expect("start gate lock") = true;
    h.sink.start_changed.notify_all();
    timeout(TIMEOUT, &mut transition)
        .await
        .expect("transition finished")
        .expect("join");
    let (output, result) = timeout(TIMEOUT, &mut terminal)
        .await
        .expect("terminal transition finished")
        .expect("join terminal");
    result.expect("terminal mode");
    h.output = output;
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Uncertain
    );
    h.custodian.request_stop(StopReason::Requested);
    assert_joined(&h).await;
}

#[tokio::test]
async fn sink_rejects_terminal_frames_but_submit_can_retire_them() {
    let mut h = start((2, 512), (2, 512)).await;
    let close = h.output.transport_close_frame(None).expect("close frame");
    assert!(matches!(
        h.output.send(close).await,
        Err(OutputError::InvalidFrame)
    ));
    let close = h.output.transport_close_frame(None).expect("close frame");
    let receipt = h.output.submit(close).await.expect("admit close");
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Completed
    );
    h.custodian.request_stop(StopReason::Requested);
    assert_joined(&h).await;
}

#[tokio::test]
async fn revocation_during_flush_and_panic_during_start_are_uncertain() {
    let mut h = start((2, 512), (2, 512)).await;
    h.sink.stall_flush.store(true, Ordering::Release);
    let frame = h
        .output
        .data_frame(Message::text("enqueued"))
        .expect("frame");
    let receipt = h.output.submit(frame).await.expect("admitted");
    timeout(TIMEOUT, h.sink.wait_for_starts(1))
        .await
        .expect("start");
    h.lifecycle.begin_drain();
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Uncertain
    );
    assert_eq!(h.sink.starts.load(Ordering::Acquire), 1);
    h.custodian.request_stop(StopReason::Requested);
    assert_joined(&h).await;
    let mut h = start((2, 512), (2, 512)).await;
    h.sink.panic_in_start.store(true, Ordering::Release);
    let frame = h.output.data_frame(Message::text("panic")).expect("frame");
    let receipt = h.output.submit(frame).await.expect("admitted");
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Uncertain
    );
    assert_joined(&h).await;
}

#[tokio::test]
async fn fixed_shutdown_frames_survive_revocation_but_open_data_is_rejected() {
    let mut h = start((2, 512), (4, 1024)).await;
    let stale = h.output.data_frame(Message::text("stale")).expect("frame");
    assert!(matches!(
        h.output.data_frame(Message::Close(None)),
        Err(OutputError::InvalidFrame)
    ));
    h.lifecycle.begin_drain();
    h.process_stop.cancel();
    h.output.begin_shutdown().expect("terminal");
    let stale = h.output.submit(stale).await.expect("stale admitted");
    assert_eq!(
        timeout(TIMEOUT, stale.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::NotAttempted
    );
    assert!(matches!(
        h.output.data_frame(Message::text("forbidden")),
        Err(OutputError::InvalidFrame)
    ));
    assert!(matches!(
        h.output.conflict_error_frame(),
        Err(OutputError::InvalidFrame)
    ));
    let shutdown = h.output.shutdown_error_frame().expect("shutdown frame");
    let shutdown = h.output.submit(shutdown).await.expect("submit shutdown");
    let framing = h.output.framing_close_frame().expect("framing frame");
    let framing = h.output.submit(framing).await.expect("submit framing");
    assert_eq!(
        timeout(TIMEOUT, shutdown.wait_for_terminal())
            .await
            .expect("shutdown receipt"),
        WriteProgress::Completed
    );
    assert_eq!(
        timeout(TIMEOUT, framing.wait_for_terminal())
            .await
            .expect("framing receipt"),
        WriteProgress::Completed
    );
    let frames = h.sink.frames.lock().expect("frames").clone();
    assert_eq!(frames, vec![
        Message::text("<stream:error xmlns:stream=\"http://etherx.jabber.org/streams\"><system-shutdown xmlns=\"urn:ietf:params:xml:ns:xmpp-streams\"/></stream:error>"),
        Message::text("<close xmlns='urn:ietf:params:xml:ns:xmpp-framing'/>"),
    ]);
    let close = h.output.transport_close_frame(None).expect("close frame");
    let close = h.output.submit(close).await.expect("submit close");
    assert_eq!(
        timeout(TIMEOUT, close.wait_for_terminal())
            .await
            .expect("close receipt"),
        WriteProgress::Completed
    );
    h.peer
        .send(Ok(Message::Close(None)))
        .await
        .expect("peer close");
    timeout(TIMEOUT, h.output.close())
        .await
        .expect("retire")
        .expect("closed");
    assert_joined(&h).await;
}

#[tokio::test]
async fn conflict_bypasses_only_live_interruption_and_foreign_frames_are_rejected() {
    let mut first = start((2, 512), (4, 1024)).await;
    let mut second = start((2, 512), (4, 1024)).await;
    let foreign = first
        .output
        .data_frame(Message::text("foreign"))
        .expect("frame");
    assert!(matches!(
        second.output.submit(foreign).await,
        Err(OutputError::WrongConnection)
    ));
    first.live_interrupt.cancel();
    first.output.begin_live_detach().expect("detach");
    assert!(matches!(
        first.output.shutdown_error_frame(),
        Err(OutputError::InvalidFrame)
    ));
    let conflict = first.output.conflict_error_frame().expect("conflict");
    let receipt = first.output.submit(conflict).await.expect("admitted");
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Completed
    );
    let framing = first.output.framing_close_frame().expect("framing");
    let receipt = first.output.submit(framing).await.expect("admitted");
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::Completed
    );
    assert_eq!(*first.sink.frames.lock().expect("frames"), vec![
        Message::text("<stream:error xmlns:stream=\"http://etherx.jabber.org/streams\"><conflict xmlns=\"urn:ietf:params:xml:ns:xmpp-streams\"/></stream:error>"),
        Message::text("<close xmlns='urn:ietf:params:xml:ns:xmpp-framing'/>"),
    ]);
    first.custodian.request_stop(StopReason::Requested);
    assert_joined(&first).await;
    second.output.begin_live_detach().expect("detach");
    second.process_stop.cancel();
    let conflict = second.output.conflict_error_frame().expect("conflict");
    let receipt = second.output.submit(conflict).await.expect("admitted");
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::NotAttempted
    );
    assert_eq!(second.sink.starts.load(Ordering::Acquire), 0);
    let close = second
        .output
        .transport_close_frame(None)
        .expect("transport close");
    let close = second
        .output
        .submit(close)
        .await
        .expect("admit transport close");
    assert_eq!(
        timeout(TIMEOUT, close.wait_for_terminal())
            .await
            .expect("close receipt"),
        WriteProgress::Completed
    );
    second.custodian.request_stop(StopReason::Requested);
    assert_joined(&second).await;

    let mut third = start((2, 512), (4, 1024)).await;
    third.output.begin_live_detach().expect("detach");
    third.lifecycle.begin_fenced_recovery();
    third.lifecycle.serve();
    let conflict = third.output.conflict_error_frame().expect("conflict");
    let receipt = third.output.submit(conflict).await.expect("admitted");
    assert_eq!(
        timeout(TIMEOUT, receipt.wait_for_terminal())
            .await
            .expect("receipt"),
        WriteProgress::NotAttempted
    );
    assert_eq!(third.sink.starts.load(Ordering::Acquire), 0);
    third.custodian.request_stop(StopReason::Requested);
    assert_joined(&third).await;
}
