use futures::{Sink, SinkExt, Stream, StreamExt};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{accept_async, connect_async, MaybeTlsStream, WebSocketStream};
use waddle_server::test_support::supervised_io::{
    spawn_io_custodian, spawn_supervised_io, AdmissionError, CustodianState, InboundExit,
    InboundTerminal, IoCustodianInventory, Limits, OutboundExit, OutboundTerminal, QueueItem,
    RetirementMode, ShutdownProtocol, StopReason, TransportLimits, WriteProgress, WriteReceipt,
    WriteSender,
};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum Stall {
    Never,
    Ready,
    ReadyOnce,
    Flush,
    FlushOnce,
}

struct StallSink<S> {
    inner: S,
    stall: Stall,
    blocked: Option<oneshot::Sender<()>>,
}

struct FailingStartSend;

impl Sink<Message> for FailingStartSend {
    type Error = std::io::Error;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), Self::Error> {
        Err(std::io::Error::other("injected outbound failure"))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl<S: Sink<Message, Error = Error> + Unpin> Sink<Message> for StallSink<S> {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if matches!(self.stall, Stall::Ready)
            || matches!(self.stall, Stall::ReadyOnce) && self.blocked.is_some()
        {
            if let Some(blocked) = self.blocked.take() {
                let _ = blocked.send(());
            }
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        Pin::new(&mut self.inner).start_send(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if matches!(self.stall, Stall::Flush)
            || matches!(self.stall, Stall::FlushOnce) && self.blocked.is_some()
        {
            if let Some(blocked) = self.blocked.take() {
                let _ = blocked.send(());
            }
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

struct CloseRaceReader<S> {
    inner: S,
    close_entered: Option<oneshot::Sender<()>>,
    gate: Arc<PollGate>,
}

impl<S: Stream<Item = Result<Message, Error>> + Unpin> Stream for CloseRaceReader<S> {
    type Item = Result<Message, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = Pin::new(&mut self.inner).poll_next(cx);
        match result {
            Poll::Ready(Some(Ok(Message::Close(frame)))) => {
                if let Some(close_entered) = self.close_entered.take() {
                    let _ = close_entered.send(());
                    self.gate.wait();
                }
                Poll::Ready(Some(Ok(Message::Close(frame))))
            }
            other => other,
        }
    }
}

struct CloseRaceWriter<S> {
    inner: S,
    flush_entered: Option<oneshot::Sender<()>>,
    send_after_close: Option<oneshot::Sender<()>>,
    gate: Arc<PollGate>,
}

impl<S: Sink<Message, Error = Error> + Unpin> Sink<Message> for CloseRaceWriter<S> {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.inner).poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        Pin::new(&mut self.inner).start_send(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if let Some(flush_entered) = self.flush_entered.take() {
            let _ = flush_entered.send(());
            self.gate.wait();
        }
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        if matches!(
            &result,
            Poll::Ready(Err(Error::Protocol(ProtocolError::SendAfterClosing)))
        ) {
            if let Some(send_after_close) = self.send_after_close.take() {
                let _ = send_after_close.send(());
            }
        }
        result
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

struct BarrierFlushWriter<S> {
    inner: S,
    flush_entered: Option<oneshot::Sender<()>>,
    gate: Arc<PollGate>,
}

impl<S: Sink<Message, Error = Error> + Unpin> Sink<Message> for BarrierFlushWriter<S> {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.inner).poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        Pin::new(&mut self.inner).start_send(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if let Some(flush_entered) = self.flush_entered.take() {
            let _ = flush_entered.send(());
            self.gate.wait();
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

#[derive(Default)]
struct PollGate {
    released: Mutex<bool>,
    changed: Condvar,
}

impl PollGate {
    fn wait(&self) {
        let mut released = self.released.lock().expect("gate lock");
        while !*released {
            released = self.changed.wait(released).expect("gate wait");
        }
    }

    fn release(&self) {
        *self.released.lock().expect("gate lock") = true;
        self.changed.notify_all();
    }
}

struct ReleaseGatesOnDrop([Arc<PollGate>; 2]);

impl Drop for ReleaseGatesOnDrop {
    fn drop(&mut self) {
        for gate in &self.0 {
            gate.release();
        }
    }
}

struct Lab {
    supervisor: waddle_server::test_support::supervised_io::IoSupervisor<Error, Error>,
    outbound: WriteSender<Message>,
    inbound: mpsc::Receiver<QueueItem<Message>>,
    blocked: oneshot::Receiver<()>,
}

impl Lab {
    fn write(
        &self,
        frame: Message,
    ) -> Result<WriteReceipt, waddle_server::test_support::supervised_io::AdmissionError> {
        let bytes = frame.len();
        self.outbound.try_send(frame, bytes)
    }
}

fn start_lab(
    socket: WebSocketStream<TcpStream>,
    stall: Stall,
    outbound_limits: (usize, usize),
    inbound_limits: (usize, usize),
) -> Lab {
    let (writer, reader) = socket.split();
    let (blocked_tx, blocked) = oneshot::channel();
    let writer = StallSink {
        inner: writer,
        stall,
        blocked: Some(blocked_tx),
    };
    let (supervisor, outbound, inbound) = spawn_supervised_io(
        ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        reader,
        writer,
        TransportLimits::new(
            Limits::new(inbound_limits.0, inbound_limits.1).expect("inbound limits"),
            Limits::new(outbound_limits.0, outbound_limits.1).expect("outbound limits"),
        ),
        ShutdownProtocol::new(Message::Close(None), |error: &Error| {
            matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
        }),
        Message::len,
        Message::is_close,
    );
    Lab {
        supervisor,
        outbound,
        inbound,
        blocked,
    }
}

async fn socket_pair() -> (
    WebSocketStream<TcpStream>,
    WebSocketStream<MaybeTlsStream<TcpStream>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let accept = async {
        let (stream, _) = listener.accept().await.expect("accept");
        accept_async(stream).await.expect("server handshake")
    };
    let connect = connect_async(format!("ws://{address}"));
    let (server, client) = tokio::join!(accept, connect);
    (server, client.expect("client handshake").0)
}

async fn peer_observes_drop(peer: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) {
    let next = timeout(TEST_TIMEOUT, peer.next())
        .await
        .expect("peer observes termination");
    match next {
        None | Some(Ok(Message::Close(_))) | Some(Err(Error::ConnectionClosed)) => {}
        Some(Err(Error::Protocol(ProtocolError::ResetWithoutClosingHandshake))) => {}
        Some(Err(Error::Io(error)))
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            ) => {}
        other => panic!("expected peer-visible close or drop, got {other:?}"),
    }
}

#[tokio::test]
async fn force_detach_interrupts_stalls_and_joins_both_owners() {
    for (stall, expected_before_retire, expected_after_retire) in [
        (
            Stall::Ready,
            WriteProgress::Admitted,
            WriteProgress::NotAttempted,
        ),
        (
            Stall::Flush,
            WriteProgress::Attempted,
            WriteProgress::Uncertain,
        ),
    ] {
        let (socket, mut peer) = socket_pair().await;
        let mut lab = start_lab(socket, stall, (2, 8), (2, 8));
        let first = lab.write(Message::binary(vec![1, 2, 3])).expect("admit");
        timeout(TEST_TIMEOUT, &mut lab.blocked)
            .await
            .expect("stalled")
            .expect("signal");
        assert_eq!(first.progress(), expected_before_retire);

        peer.send(Message::binary(vec![9]))
            .await
            .expect("peer sends while writer stalls");
        let input = timeout(TEST_TIMEOUT, lab.inbound.recv())
            .await
            .expect("reader remains responsive")
            .expect("input admitted");
        assert_eq!(input.value.into_data().as_ref(), &[9]);
        let tail = lab.write(Message::binary(vec![4, 5])).expect("queue tail");

        let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(RetirementMode::Forced))
            .await
            .expect("join pumps");
        assert!(matches!(
            joined.inbound.expect("reader joined"),
            InboundExit::Stopped
        ));
        assert!(matches!(
            joined.outbound.expect("writer joined"),
            OutboundExit::Stopped
        ));
        assert_eq!(first.progress(), expected_after_retire);
        assert_eq!(tail.progress(), WriteProgress::NotAttempted);
        peer_observes_drop(&mut peer).await;
    }
}

#[tokio::test]
async fn outbound_count_and_byte_overload_stops_before_retirement() {
    for (limits, first, rejected) in [((1, 8), 2, 1), ((2, 4), 3, 2)] {
        let (socket, mut peer) = socket_pair().await;
        let mut lab = start_lab(socket, Stall::Ready, limits, (2, 8));
        let accepted = lab
            .write(Message::binary(vec![1; first]))
            .expect("first admitted");
        timeout(TEST_TIMEOUT, &mut lab.blocked)
            .await
            .expect("stalled")
            .expect("signal");
        assert!(matches!(
            lab.write(Message::binary(vec![2; rejected])),
            Err(waddle_server::test_support::supervised_io::AdmissionError::Overloaded)
        ));
        assert!(lab.supervisor.is_stopped());
        assert_eq!(
            lab.supervisor.stop_reason(),
            Some(StopReason::OutboundOverload)
        );
        assert_eq!(accepted.progress(), WriteProgress::Admitted);
        assert!(matches!(
            lab.write(Message::binary(vec![3])),
            Err(AdmissionError::Stopped)
        ));

        let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(RetirementMode::Forced))
            .await
            .expect("join pumps");
        assert!(matches!(
            joined.inbound.expect("reader joined"),
            InboundExit::Stopped
        ));
        assert!(matches!(
            joined.outbound.expect("writer joined"),
            OutboundExit::Stopped
        ));
        peer_observes_drop(&mut peer).await;
    }
}

#[tokio::test]
async fn inbound_count_and_byte_overload_stops_before_retirement() {
    for (limits, first, rejected) in [((1, 8), 2, 1), ((2, 4), 3, 2)] {
        let (socket, mut peer) = socket_pair().await;
        let mut lab = start_lab(socket, Stall::Never, (2, 8), limits);
        peer.send(Message::binary(vec![1; first]))
            .await
            .expect("first frame");
        let retained = timeout(TEST_TIMEOUT, lab.inbound.recv())
            .await
            .expect("first read")
            .expect("first admitted");
        peer.send(Message::binary(vec![2; rejected]))
            .await
            .expect("second frame");
        timeout(TEST_TIMEOUT, lab.supervisor.stopped())
            .await
            .expect("overload stops transport");
        assert_eq!(
            lab.supervisor.stop_reason(),
            Some(StopReason::InboundOverload)
        );
        let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(RetirementMode::Forced))
            .await
            .expect("join pumps");
        assert!(matches!(
            joined.inbound.expect("reader joined"),
            InboundExit::Overloaded
        ));
        assert!(matches!(
            joined.outbound.expect("writer joined"),
            OutboundExit::Stopped
        ));
        drop(retained);
        peer_observes_drop(&mut peer).await;
    }
}

#[tokio::test]
async fn graceful_shutdown_writes_a_close_and_joins_both_owners() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    lab.supervisor.begin_graceful_shutdown();
    assert!(matches!(
        timeout(TEST_TIMEOUT, peer.next())
            .await
            .expect("server close"),
        Some(Ok(Message::Close(_)))
    ));
    peer.flush().await.expect("flush automatic Close reply");
    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor.shutdown_with_deadline(TEST_TIMEOUT),
    )
    .await
    .expect("join pumps after close handshake");
    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::PeerClosed
    ));
    assert!(matches!(
        joined.outbound.expect("writer joined"),
        OutboundExit::ShutdownClose | OutboundExit::PeerClosedReplyFlushed
    ));
}

#[tokio::test]
async fn graceful_shutdown_seals_admission_and_marks_abandoned_receipts_not_attempted() {
    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::ReadyOnce, (3, 16), (2, 8));
    let first = lab.write(Message::binary(vec![1])).expect("first admitted");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("first write waits before attempt")
        .expect("stalled signal");
    let tail = lab
        .write(Message::binary(vec![2]))
        .expect("second admitted behind first");

    lab.supervisor.begin_graceful_shutdown();
    assert!(matches!(
        lab.write(Message::binary(vec![3])),
        Err(AdmissionError::Shutdown)
    ));
    assert!(matches!(
        timeout(TEST_TIMEOUT, peer.next())
            .await
            .expect("server close"),
        Some(Ok(Message::Close(_)))
    ));
    peer.flush().await.expect("flush automatic Close reply");
    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor.shutdown_with_deadline(TEST_TIMEOUT),
    )
    .await
    .expect("close handshake joins pumps");

    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::PeerClosed
    ));
    assert!(matches!(
        joined.outbound.expect("writer joined"),
        OutboundExit::ShutdownClose | OutboundExit::PeerClosedReplyFlushed
    ));
    assert_eq!(first.progress(), WriteProgress::NotAttempted);
    assert_eq!(tail.progress(), WriteProgress::NotAttempted);
}

#[tokio::test]
async fn peer_close_flushes_the_queued_reply_before_bounded_retirement() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    peer.send(Message::Close(None))
        .await
        .expect("peer sends close");

    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor.retire_after_failure(TEST_TIMEOUT),
    )
    .await
    .expect("both pumps retire within deadline");
    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::PeerClosed
    ));
    assert!(matches!(
        joined.outbound.expect("writer joined"),
        OutboundExit::PeerClosedReplyFlushed
    ));
    assert!(matches!(
        timeout(TEST_TIMEOUT, peer.next())
            .await
            .expect("server close"),
        Some(Ok(Message::Close(_)))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn send_after_closing_waits_for_peer_close_then_flushes_reply() {
    for local_shutdown in [false, true] {
        let (socket, mut peer) = socket_pair().await;
        let (writer, reader) = socket.split();
        let writer_gate = Arc::new(PollGate::default());
        let reader_gate = Arc::new(PollGate::default());
        let _release_gates_on_drop = ReleaseGatesOnDrop([writer_gate.clone(), reader_gate.clone()]);
        let (flush_entered_tx, flush_entered) = oneshot::channel();
        let (close_entered_tx, close_entered) = oneshot::channel();
        let (send_after_close_tx, send_after_close) = oneshot::channel();
        let writer = CloseRaceWriter {
            inner: writer,
            flush_entered: Some(flush_entered_tx),
            send_after_close: Some(send_after_close_tx),
            gate: writer_gate.clone(),
        };
        let reader = CloseRaceReader {
            inner: reader,
            close_entered: Some(close_entered_tx),
            gate: reader_gate.clone(),
        };
        let (supervisor, outbound, _inbound) = spawn_supervised_io(
            ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
            reader,
            writer,
            TransportLimits::new(
                Limits::new(2, 8).expect("inbound limits"),
                Limits::new(2, 8).expect("outbound limits"),
            ),
            ShutdownProtocol::new(Message::Close(None), |error: &Error| {
                matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
            }),
            Message::len,
            Message::is_close,
        );
        let receipt = if local_shutdown {
            supervisor.begin_graceful_shutdown();
            None
        } else {
            Some(
                outbound
                    .try_send(Message::binary(vec![1, 2, 3]), 3)
                    .expect("admit outbound frame"),
            )
        };

        timeout(TEST_TIMEOUT, flush_entered)
            .await
            .expect("writer reaches its first flush")
            .expect("flush-entry signal");
        peer.send(Message::Close(None))
            .await
            .expect("peer initiates close");
        timeout(TEST_TIMEOUT, close_entered)
            .await
            .expect("reader consumes peer close before signaling the pump")
            .expect("close-entry signal");

        // Let SplitSink poll Tungstenite after the reader has moved its protocol
        // state to ClosedByPeer, while the reader is still holding back the
        // peer-close cancellation token.
        writer_gate.release();
        timeout(TEST_TIMEOUT, send_after_close)
            .await
            .expect("writer observes exact SendAfterClosing")
            .expect("send-after-close signal");
        let tail = if let Some(receipt) = &receipt {
            assert_eq!(
                receipt.progress(),
                WriteProgress::Attempted,
                "a successful split-sink start_send remains monotonic until retirement"
            );
            let tail = outbound
                .try_send(Message::binary(vec![4]), 1)
                .expect("one queued frame fits behind the in-flight frame");
            assert!(matches!(
                outbound.try_send(Message::binary(vec![5]), 1),
                Err(AdmissionError::Overloaded)
            ));
            assert!(supervisor.is_stopped(), "overload is signaled immediately");
            assert_eq!(supervisor.stop_reason(), Some(StopReason::OutboundOverload));
            Some(tail)
        } else {
            assert!(
                !supervisor.is_stopped(),
                "local close waits for the reader close signal"
            );
            None
        };

        reader_gate.release();
        let joined = timeout(TEST_TIMEOUT, supervisor.retire_after_failure(TEST_TIMEOUT))
            .await
            .expect("both pumps retire after peer-close reply flush");
        assert!(matches!(
            joined.inbound.expect("reader joined"),
            InboundExit::PeerClosed
        ));
        assert!(matches!(
            joined.outbound.expect("writer joined"),
            OutboundExit::PeerClosedReplyFlushed
        ));
        if let Some(receipt) = receipt {
            assert_eq!(receipt.progress(), WriteProgress::Uncertain);
        }
        if let Some(tail) = tail {
            assert_eq!(tail.progress(), WriteProgress::NotAttempted);
        }
        assert!(matches!(
            timeout(TEST_TIMEOUT, peer.next())
                .await
                .expect("server close reply"),
            Some(Ok(Message::Close(_)))
        ));
    }
}

#[tokio::test]
async fn peer_close_during_local_close_flush_still_gets_a_server_close() {
    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::FlushOnce, (2, 8), (2, 8));
    lab.supervisor.begin_graceful_shutdown();
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("local close flush is stalled")
        .expect("stalled signal");
    peer.send(Message::Close(None))
        .await
        .expect("peer initiates simultaneous close");

    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor.shutdown_with_deadline(TEST_TIMEOUT),
    )
    .await
    .expect("bounded graceful retirement joins both pumps");
    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::PeerClosed
    ));
    let outbound = joined.outbound.expect("writer joined");
    assert!(
        matches!(
            outbound,
            OutboundExit::PeerClosedReplyFlushed | OutboundExit::ShutdownClose
        ),
        "unexpected simultaneous-close writer exit: {outbound:?}"
    );
    assert!(matches!(
        timeout(TEST_TIMEOUT, peer.next())
            .await
            .expect("server close reply"),
        Some(Ok(Message::Close(_)))
    ));
}

#[tokio::test]
async fn stalled_peer_close_reply_is_aborted_and_joined_at_the_deadline() {
    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::Flush, (2, 8), (2, 8));
    peer.send(Message::Close(None))
        .await
        .expect("peer sends close");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("peer-close reply flush blocks")
        .expect("stalled signal");

    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor
            .retire_after_failure(Duration::from_millis(50)),
    )
    .await
    .expect("deadline aborts and joins stalled writer");
    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::PeerClosed
    ));
    match joined.outbound {
        Err(error) => assert!(error.is_cancelled()),
        Ok(OutboundExit::Stopped) => {}
        other => panic!("stalled reply must stop at the deadline, got {other:?}"),
    }
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn forced_retirement_interrupts_a_stalled_peer_close_reply() {
    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::Flush, (2, 8), (2, 8));
    peer.send(Message::Close(None))
        .await
        .expect("peer sends close");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("peer-close reply flush blocks")
        .expect("stalled signal");

    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(RetirementMode::Forced))
        .await
        .expect("forced retirement interrupts reply flush and joins pumps");
    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::PeerClosed
    ));
    assert!(matches!(
        joined.outbound.expect("writer joined"),
        OutboundExit::Stopped
    ));
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn inbound_and_outbound_aborts_are_joined_before_retirement() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    lab.supervisor.abort_inbound_for_test();
    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor.retire_after_failure(TEST_TIMEOUT),
    )
    .await
    .expect("join both tasks after inbound abort");
    assert!(joined
        .inbound
        .expect_err("inbound abort is observed")
        .is_cancelled());
    assert!(matches!(
        joined.outbound.expect("writer joined"),
        OutboundExit::Stopped
    ));
    peer_observes_drop(&mut peer).await;

    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::Ready, (2, 8), (2, 8));
    let admitted = lab.write(Message::binary(vec![1])).expect("admit");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("stalled")
        .expect("signal");
    lab.supervisor.abort_outbound_for_test();
    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor.retire_after_failure(TEST_TIMEOUT),
    )
    .await
    .expect("join both tasks after outbound abort");
    assert!(joined
        .outbound
        .expect_err("outbound abort is observed")
        .is_cancelled());
    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::Stopped
    ));
    assert_eq!(admitted.progress(), WriteProgress::NotAttempted);
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn stalled_shutdown_aborts_then_joins_before_peer_observes_drop() {
    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::Flush, (2, 8), (2, 8));
    let attempted = lab.write(Message::binary(vec![1])).expect("admit");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("flush stalled")
        .expect("signal");
    assert_eq!(attempted.progress(), WriteProgress::Attempted);
    lab.supervisor.begin_graceful_shutdown();
    tokio::task::yield_now().await;
    assert!(
        !lab.supervisor.inbound_finished(),
        "reader remains available for peer Close during graceful shutdown"
    );
    assert!(!lab.supervisor.outbound_finished());
    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor
            .shutdown_with_deadline(Duration::from_millis(50)),
    )
    .await
    .expect("both handles joined after deadline");
    match joined.inbound {
        Err(error) => assert!(error.is_cancelled()),
        Ok(InboundExit::Stopped) => {}
        other => panic!("reader must stop or be aborted at deadline, got {other:?}"),
    }
    match joined.outbound {
        Err(error) => assert!(error.is_cancelled()),
        Ok(OutboundExit::Stopped) => {}
        other => panic!("stalled writer must stop or be aborted, got {other:?}"),
    }
    assert_eq!(attempted.progress(), WriteProgress::Uncertain);
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn cancelling_a_requester_does_not_cancel_an_admitted_write() {
    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::Flush, (2, 8), (2, 8));
    let (receipt_tx, receipt_rx) = oneshot::channel();
    let requester = tokio::spawn(async move {
        let receipt = lab
            .outbound
            .try_send(Message::binary(vec![1]), 1)
            .expect("admit");
        receipt_tx.send(receipt).expect("publish receipt");
        std::future::pending::<()>().await;
    });
    let receipt = receipt_rx.await.expect("admitted");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("flush stalled")
        .expect("signal");
    requester.abort();
    assert!(requester
        .await
        .expect_err("requester cancelled")
        .is_cancelled());
    assert_eq!(receipt.progress(), WriteProgress::Attempted);
    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(RetirementMode::Forced))
        .await
        .expect("join pumps");
    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::Stopped
    ));
    assert!(matches!(
        joined.outbound.expect("writer joined"),
        OutboundExit::Stopped
    ));
    assert_eq!(receipt.progress(), WriteProgress::Uncertain);
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn flushed_frame_is_peer_visible_without_claiming_client_acknowledgement() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    let receipt = lab.write(Message::binary(vec![1, 2])).expect("admit");
    let received = timeout(TEST_TIMEOUT, peer.next())
        .await
        .expect("peer reads frame")
        .expect("frame")
        .expect("valid frame");
    assert_eq!(received.into_data().as_ref(), &[1, 2]);
    timeout(TEST_TIMEOUT, async {
        while receipt.progress() != WriteProgress::Completed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("local flush completes");
    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(RetirementMode::Forced))
        .await
        .expect("join pumps");
    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::Stopped
    ));
    assert!(matches!(
        joined.outbound.expect("writer joined"),
        OutboundExit::Stopped
    ));
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn dropping_the_producer_is_not_a_substitute_for_joining_both_pumps() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    drop(lab.outbound);
    assert!(
        !lab.supervisor.inbound_finished(),
        "reader still owns its half"
    );
    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor.retire_after_failure(TEST_TIMEOUT),
    )
    .await
    .expect("join pumps");
    assert!(matches!(
        joined.outbound.expect("writer joined"),
        OutboundExit::QueueClosed
    ));
    assert!(matches!(
        joined.inbound.expect("reader joined"),
        InboundExit::Stopped
    ));
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn completion_keeps_its_exact_connection_reference_for_late_fencing() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    let old_connection_ref = lab.supervisor.connection_ref().clone();
    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(RetirementMode::Forced))
        .await
        .expect("join pumps");
    let replacement = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    assert!(joined.is_for(&old_connection_ref));
    assert!(!joined.is_for(&replacement));
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn independent_custodian_joins_after_cancelled_waiter_and_concurrent_stop() {
    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::Flush, (2, 8), (2, 8));
    let attempted = lab.write(Message::binary(vec![1, 2])).expect("admit");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("writer reaches stalled flush")
        .expect("stalled signal");
    assert_eq!(attempted.progress(), WriteProgress::Attempted);

    let connection_ref = lab.supervisor.connection_ref().clone();
    let successor = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let (supervisor, _outbound, _inbound, _blocked) =
        (lab.supervisor, lab.outbound, lab.inbound, lab.blocked);
    let custodian = std::sync::Arc::new(spawn_io_custodian(supervisor, Duration::from_millis(250)));
    let observer = custodian.observer();
    let waiting_observer = observer.clone();
    let waiter = tokio::spawn(async move { waiting_observer.wait_until_joined().await });
    waiter.abort();
    assert!(waiter.await.expect_err("waiter is canceled").is_cancelled());

    let first_stop = std::sync::Arc::clone(&custodian);
    let second_stop = std::sync::Arc::clone(&custodian);
    let (first, second) = tokio::join!(
        tokio::spawn(async move { first_stop.request_stop(StopReason::LeaseLost) }),
        tokio::spawn(async move { second_stop.request_stop(StopReason::Requested) }),
    );
    first.expect("first stop request");
    second.expect("concurrent stop request");

    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("independent custodian completes joins"),
        CustodianState::Joined
    );
    assert!(matches!(
        custodian.stop_reason(),
        Some(StopReason::LeaseLost | StopReason::Requested)
    ));
    assert!(observer.is_for(&connection_ref));
    assert!(!observer.is_for(&successor));
    assert_eq!(attempted.progress(), WriteProgress::Uncertain);

    let first_binding = BindingRef::fresh(connection_ref.clone());
    let second_binding = BindingRef::fresh(connection_ref.clone());
    assert_eq!(
        first_binding.connection_ref(),
        second_binding.connection_ref()
    );
    assert_ne!(first_binding, second_binding);
    peer_observes_drop(&mut peer).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn cleanup_pending_remains_visible_until_a_stalled_pump_is_joined() {
    let (socket, mut peer) = socket_pair().await;
    let (writer, reader) = socket.split();
    let gate = Arc::new(PollGate::default());
    let _release_gate_on_drop = ReleaseGatesOnDrop([gate.clone(), Arc::new(PollGate::default())]);
    let (flush_entered_tx, flush_entered) = oneshot::channel();
    let writer = BarrierFlushWriter {
        inner: writer,
        flush_entered: Some(flush_entered_tx),
        gate: gate.clone(),
    };
    let (supervisor, outbound, _inbound) = spawn_supervised_io(
        ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        reader,
        writer,
        TransportLimits::new(
            Limits::new(2, 8).expect("inbound limits"),
            Limits::new(2, 8).expect("outbound limits"),
        ),
        ShutdownProtocol::new(Message::Close(None), |error: &Error| {
            matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
        }),
        Message::len,
        Message::is_close,
    );
    let attempted = outbound
        .try_send(Message::binary(vec![1]), 1)
        .expect("admit outbound frame");
    timeout(TEST_TIMEOUT, flush_entered)
        .await
        .expect("writer is blocked in its poll")
        .expect("flush-entry signal");

    let connection_ref = supervisor.connection_ref().clone();
    let custodian = spawn_io_custodian(supervisor, Duration::from_millis(25));
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    inventory
        .try_retain(&custodian)
        .expect("retain stalled connection independently");
    let observer = inventory
        .observe(&connection_ref)
        .expect("reacquire exact physical connection");
    custodian.request_stop(StopReason::LeaseLost);
    timeout(TEST_TIMEOUT, async {
        while observer.state() != CustodianState::CleanupPending {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("deadline publishes cleanup pending while the task has not yielded");
    assert_eq!(observer.state(), CustodianState::CleanupPending);
    assert_eq!(inventory.prune_joined(), 0);
    assert!(inventory.observe(&connection_ref).is_some());
    assert_eq!(attempted.progress(), WriteProgress::Attempted);

    gate.release();
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("join follows the actual pump completion"),
        CustodianState::Joined
    );
    assert_eq!(attempted.progress(), WriteProgress::Uncertain);
    assert_eq!(inventory.prune_joined(), 1);
    assert!(inventory.observe(&connection_ref).is_none());
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn independent_custodian_joins_peer_after_a_pump_panic() {
    let reader = futures::stream::poll_fn(|_| -> Poll<Option<Result<Message, Error>>> {
        panic!("injected reader panic")
    });
    let (supervisor, _outbound, _inbound) = spawn_supervised_io(
        ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        reader,
        futures::sink::drain::<Message>(),
        TransportLimits::new(
            Limits::new(2, 8).expect("inbound limits"),
            Limits::new(2, 8).expect("outbound limits"),
        ),
        ShutdownProtocol::new(Message::Close(None), |_: &std::convert::Infallible| false),
        Message::len,
        Message::is_close,
    );
    let custodian = spawn_io_custodian(supervisor, TEST_TIMEOUT);
    let observer = custodian.observer();
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("custodian joins both handles after panic"),
        CustodianState::Joined
    );
    let summary = observer
        .terminal_summary()
        .expect("terminal outcomes retained");
    assert_eq!(summary.inbound, InboundTerminal::Panicked);
    assert_eq!(summary.outbound, OutboundTerminal::Stopped);
}

#[tokio::test]
async fn independent_custodian_joins_peer_after_a_pump_abort() {
    let (socket, mut peer) = socket_pair().await;
    let Lab {
        supervisor,
        outbound: _outbound,
        inbound: _inbound,
        blocked: _blocked,
    } = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    supervisor.abort_inbound_for_test();

    let custodian = spawn_io_custodian(supervisor, TEST_TIMEOUT);
    let observer = custodian.observer();
    assert_eq!(
        timeout(TEST_TIMEOUT, observer.wait_until_joined())
            .await
            .expect("custodian joins both handles after abort"),
        CustodianState::Joined
    );
    let summary = observer
        .terminal_summary()
        .expect("terminal outcomes retained");
    assert_eq!(summary.inbound, InboundTerminal::Aborted);
    assert_eq!(summary.outbound, OutboundTerminal::Stopped);
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn independent_custodian_retains_typed_inbound_and_outbound_io_failures() {
    let reader = futures::stream::iter([Err(std::io::Error::other("injected inbound failure"))]);
    let (supervisor, _outbound, _inbound) = spawn_supervised_io(
        ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        reader,
        futures::sink::drain::<Message>(),
        TransportLimits::new(
            Limits::new(2, 8).expect("inbound limits"),
            Limits::new(2, 8).expect("outbound limits"),
        ),
        ShutdownProtocol::new(Message::Close(None), |_: &std::convert::Infallible| false),
        Message::len,
        Message::is_close,
    );
    let inbound_custodian = spawn_io_custodian(supervisor, TEST_TIMEOUT);
    let inbound_observer = inbound_custodian.observer();
    assert_eq!(
        timeout(TEST_TIMEOUT, inbound_observer.wait_until_joined())
            .await
            .expect("join after inbound I/O failure"),
        CustodianState::Joined
    );
    let inbound_summary = inbound_observer
        .terminal_summary()
        .expect("inbound failure summary");
    assert_eq!(inbound_summary.inbound, InboundTerminal::IoFailure);
    assert_eq!(inbound_summary.outbound, OutboundTerminal::Stopped);

    let (supervisor, outbound, _inbound) = spawn_supervised_io(
        ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        futures::stream::pending::<Result<Message, std::io::Error>>(),
        FailingStartSend,
        TransportLimits::new(
            Limits::new(2, 8).expect("inbound limits"),
            Limits::new(2, 8).expect("outbound limits"),
        ),
        ShutdownProtocol::new(Message::Close(None), |_: &std::io::Error| false),
        Message::len,
        Message::is_close,
    );
    outbound
        .try_send(Message::binary(vec![1]), 1)
        .expect("admit failing write");
    let outbound_custodian = spawn_io_custodian(supervisor, TEST_TIMEOUT);
    let outbound_observer = outbound_custodian.observer();
    assert_eq!(
        timeout(TEST_TIMEOUT, outbound_observer.wait_until_joined())
            .await
            .expect("join after outbound I/O failure"),
        CustodianState::Joined
    );
    let outbound_summary = outbound_observer
        .terminal_summary()
        .expect("outbound failure summary");
    assert_eq!(outbound_summary.inbound, InboundTerminal::Stopped);
    assert_eq!(outbound_summary.outbound, OutboundTerminal::IoFailure);
}

#[tokio::test]
async fn reader_panic_is_preserved_while_its_writer_is_stopped_and_joined() {
    let reader = futures::stream::poll_fn(|_| -> Poll<Option<Result<Message, Error>>> {
        panic!("injected reader panic")
    });
    let writer = futures::sink::drain::<Message>();
    let (supervisor, _outbound, _inbound) = spawn_supervised_io(
        ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        reader,
        writer,
        TransportLimits::new(
            Limits::new(2, 8).expect("inbound limits"),
            Limits::new(2, 8).expect("outbound limits"),
        ),
        ShutdownProtocol::new(Message::Close(None), |_: &std::convert::Infallible| false),
        Message::len,
        Message::is_close,
    );
    let joined = timeout(TEST_TIMEOUT, supervisor.retire_after_failure(TEST_TIMEOUT))
        .await
        .expect("join after task panic");
    assert!(joined.inbound.expect_err("panic is observed").is_panic());
    assert!(matches!(
        joined.outbound.expect("writer joined"),
        OutboundExit::Stopped
    ));
}
