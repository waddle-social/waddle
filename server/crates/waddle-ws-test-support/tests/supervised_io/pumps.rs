use futures::{Sink, SinkExt, StreamExt};
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{accept_async, connect_async, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

const NOT_ATTEMPTED: u8 = 0;
const ATTEMPTED: u8 = 1;
const FLUSHED: u8 = 2;
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

struct Queued<T> {
    value: T,
    _count: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

struct QueueSender<T> {
    sender: mpsc::Sender<Queued<T>>,
    count: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    stop: CancellationToken,
}

#[derive(Debug, PartialEq, Eq)]
enum OfferError {
    Overloaded,
    Closed,
}

impl<T> QueueSender<T> {
    fn offer(&self, value: T, bytes: usize) -> Result<(), OfferError> {
        let outcome = (|| {
            let count = self
                .count
                .clone()
                .try_acquire_owned()
                .map_err(|_| OfferError::Overloaded)?;
            let bytes = u32::try_from(bytes).map_err(|_| OfferError::Overloaded)?;
            let bytes = self
                .bytes
                .clone()
                .try_acquire_many_owned(bytes)
                .map_err(|_| OfferError::Overloaded)?;
            self.sender
                .try_send(Queued {
                    value,
                    _count: count,
                    _bytes: bytes,
                })
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => OfferError::Overloaded,
                    mpsc::error::TrySendError::Closed(_) => OfferError::Closed,
                })
        })();
        if outcome.is_err() {
            // The laboratory's explicit full/closed policy is to stop this
            // transport. It makes no claim about durable delivery custody.
            self.stop.cancel();
        }
        outcome
    }
}

fn bounded_queue<T>(
    count: usize,
    bytes: usize,
    stop: CancellationToken,
) -> (QueueSender<T>, mpsc::Receiver<Queued<T>>) {
    let (sender, receiver) = mpsc::channel(count);
    (
        QueueSender {
            sender,
            count: Arc::new(Semaphore::new(count)),
            bytes: Arc::new(Semaphore::new(bytes)),
            stop,
        },
        receiver,
    )
}

struct Write {
    frame: Message,
    milestone: Arc<AtomicU8>,
}

#[derive(Clone, Copy)]
enum Stall {
    Never,
    Ready,
    Flush,
}

struct StallSink<S> {
    inner: S,
    stall: Stall,
    blocked: Option<oneshot::Sender<()>>,
    milestone: Option<Arc<AtomicU8>>,
}

impl<S: Sink<Message, Error = Error> + Unpin> Sink<Message> for StallSink<S> {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if matches!(self.stall, Stall::Ready) {
            if let Some(blocked) = self.blocked.take() {
                let _ = blocked.send(());
            }
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        if let Some(milestone) = &self.milestone {
            // Mark before calling into the real sink: an error or interruption
            // after this point does not prove that no bytes reached the peer.
            milestone.store(ATTEMPTED, Ordering::SeqCst);
        }
        Pin::new(&mut self.inner).start_send(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if matches!(self.stall, Stall::Flush) {
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

#[derive(Debug, PartialEq, Eq)]
enum ReaderExit {
    Stopped,
    PeerClosed,
    Overloaded,
}

#[derive(Debug, PartialEq, Eq)]
enum WriterExit {
    Stopped,
    ShutdownClose,
    SocketError,
    QueueClosed,
}

struct Supervisor {
    stop: CancellationToken,
    shutdown: CancellationToken,
    reader: JoinHandle<ReaderExit>,
    writer: JoinHandle<WriterExit>,
}

struct Joined {
    reader: Result<ReaderExit, JoinError>,
    writer: Result<WriterExit, JoinError>,
}

impl Supervisor {
    async fn retire(self, graceful: bool) -> Joined {
        if graceful {
            self.shutdown.cancel();
        } else {
            self.stop.cancel();
        }
        let (reader, writer) = tokio::join!(self.reader, self.writer);
        Joined { reader, writer }
    }

    async fn shutdown_with_deadline(mut self, deadline: Duration) -> Joined {
        self.shutdown.cancel();
        // Preserve each result even if the other pump exceeds the deadline.
        let deadline = tokio::time::sleep(deadline);
        tokio::pin!(deadline);
        let mut reader = None;
        let mut writer = None;
        while reader.is_none() || writer.is_none() {
            tokio::select! {
                result = &mut self.reader, if reader.is_none() => reader = Some(result),
                result = &mut self.writer, if writer.is_none() => writer = Some(result),
                _ = &mut deadline => break,
            }
        }
        self.stop.cancel();
        if reader.is_none() {
            self.reader.abort();
            reader = Some(self.reader.await);
        }
        if writer.is_none() {
            self.writer.abort();
            writer = Some(self.writer.await);
        }
        Joined {
            reader: reader.expect("reader result retained"),
            writer: writer.expect("writer result retained"),
        }
    }

    async fn retire_after_failure(mut self) -> Joined {
        // Observe a pump result, then interrupt and join the other owner. The
        // first result is retained rather than detached or mistaken for both.
        tokio::select! {
            reader = &mut self.reader => {
                self.stop.cancel();
                Joined { reader, writer: self.writer.await }
            }
            writer = &mut self.writer => {
                self.stop.cancel();
                Joined { reader: self.reader.await, writer }
            }
        }
    }
}

struct Lab {
    supervisor: Supervisor,
    outbound: QueueSender<Write>,
    inbound: mpsc::Receiver<Queued<Message>>,
    blocked: oneshot::Receiver<()>,
    panic_reader: oneshot::Sender<()>,
}

impl Lab {
    fn write(&self, frame: Message) -> Result<Arc<AtomicU8>, OfferError> {
        let milestone = Arc::new(AtomicU8::new(NOT_ATTEMPTED));
        let bytes = frame.len();
        self.outbound.offer(
            Write {
                frame,
                milestone: milestone.clone(),
            },
            bytes,
        )?;
        Ok(milestone)
    }
}

fn start_lab(
    socket: WebSocketStream<TcpStream>,
    stall: Stall,
    outbound_limits: (usize, usize),
    inbound_limits: (usize, usize),
) -> Lab {
    let (writer, mut reader) = socket.split();
    let stop = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let (outbound, mut writes) =
        bounded_queue::<Write>(outbound_limits.0, outbound_limits.1, stop.clone());
    let (input, inbound) = bounded_queue(inbound_limits.0, inbound_limits.1, stop.clone());
    let (blocked_tx, blocked) = oneshot::channel();
    let (panic_reader, mut panic_rx) = oneshot::channel();
    let reader_stop = stop.clone();
    let reader_shutdown = shutdown.clone();
    let reader = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = reader_stop.cancelled() => return ReaderExit::Stopped,
                _ = reader_shutdown.cancelled() => return ReaderExit::Stopped,
                result = &mut panic_rx => {
                    if result.is_ok() {
                        panic!("injected reader panic");
                    }
                    // The test keeps the sender alive until retirement.
                    return ReaderExit::Stopped;
                }
                frame = reader.next() => match frame {
                    Some(Ok(frame)) if !frame.is_close() => {
                        let bytes = frame.len();
                        if input.offer(frame, bytes).is_err() {
                            return ReaderExit::Overloaded;
                        }
                    }
                    _ => return ReaderExit::PeerClosed,
                }
            }
        }
    });
    let writer_stop = stop.clone();
    let writer_shutdown = shutdown.clone();
    let writer = tokio::spawn(async move {
        let mut writer = StallSink {
            inner: writer,
            stall,
            blocked: Some(blocked_tx),
            milestone: None,
        };
        loop {
            let work = tokio::select! {
                biased;
                _ = writer_stop.cancelled() => return WriterExit::Stopped,
                _ = writer_shutdown.cancelled() => {
                    return if writer.send(Message::Close(None)).await.is_ok() {
                        WriterExit::ShutdownClose
                    } else {
                        WriterExit::SocketError
                    };
                }
                work = writes.recv() => match work {
                    Some(work) => work,
                    None => return WriterExit::QueueClosed,
                }
            };
            writer.milestone = Some(work.value.milestone.clone());
            let result = tokio::select! {
                biased;
                _ = writer_stop.cancelled() => return WriterExit::Stopped,
                _ = writer_shutdown.cancelled() => {
                    writer.milestone = None;
                    return if writer.send(Message::Close(None)).await.is_ok() {
                        WriterExit::ShutdownClose
                    } else {
                        WriterExit::SocketError
                    };
                }
                result = writer.send(work.value.frame) => result,
            };
            if result.is_err() {
                return WriterExit::SocketError;
            }
            work.value.milestone.store(FLUSHED, Ordering::SeqCst);
            writer.milestone = None;
        }
    });
    Lab {
        supervisor: Supervisor {
            stop,
            shutdown,
            reader,
            writer,
        },
        outbound,
        inbound,
        blocked,
        panic_reader,
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
    for (stall, expected) in [(Stall::Ready, NOT_ATTEMPTED), (Stall::Flush, ATTEMPTED)] {
        let (socket, mut peer) = socket_pair().await;
        let mut lab = start_lab(socket, stall, (2, 8), (2, 8));
        let first = lab.write(Message::binary(vec![1, 2, 3])).expect("admit");
        timeout(TEST_TIMEOUT, &mut lab.blocked)
            .await
            .expect("stalled")
            .expect("signal");
        peer.send(Message::binary(vec![9]))
            .await
            .expect("peer sends while writer stalls");
        let input = timeout(TEST_TIMEOUT, lab.inbound.recv())
            .await
            .expect("reader remains responsive")
            .expect("input admitted");
        assert_eq!(input.value.into_data().as_ref(), &[9]);
        let tail = lab.write(Message::binary(vec![4, 5])).expect("queue tail");
        let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(false))
            .await
            .expect("join pumps");
        assert_eq!(joined.reader.expect("reader joined"), ReaderExit::Stopped);
        assert_eq!(joined.writer.expect("writer joined"), WriterExit::Stopped);
        assert_eq!(first.load(Ordering::SeqCst), expected);
        assert_eq!(tail.load(Ordering::SeqCst), NOT_ATTEMPTED);
        peer_observes_drop(&mut peer).await;
    }
}

#[tokio::test]
async fn outbound_count_and_byte_overload_stop_the_transport_without_an_attempt() {
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
            Err(OfferError::Overloaded)
        ));
        timeout(TEST_TIMEOUT, lab.supervisor.stop.cancelled())
            .await
            .expect("outbound overload stops transport before retirement");
        let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(false))
            .await
            .expect("join pumps");
        assert_eq!(joined.reader.expect("reader joined"), ReaderExit::Stopped);
        assert_eq!(joined.writer.expect("writer joined"), WriterExit::Stopped);
        assert_eq!(accepted.load(Ordering::SeqCst), NOT_ATTEMPTED);
        peer_observes_drop(&mut peer).await;
    }
}

#[tokio::test]
async fn inbound_count_and_byte_overload_stop_the_transport() {
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
        timeout(TEST_TIMEOUT, lab.supervisor.stop.cancelled())
            .await
            .expect("overload stops transport");
        let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(false))
            .await
            .expect("join pumps");
        assert_eq!(
            joined.reader.expect("reader joined"),
            ReaderExit::Overloaded
        );
        assert_eq!(joined.writer.expect("writer joined"), WriterExit::Stopped);
        drop(retained);
        peer_observes_drop(&mut peer).await;
    }
}

#[tokio::test]
async fn graceful_shutdown_writes_a_close_and_joins_both_owners() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(true))
        .await
        .expect("join pumps");
    assert_eq!(joined.reader.expect("reader joined"), ReaderExit::Stopped);
    assert_eq!(
        joined.writer.expect("writer joined"),
        WriterExit::ShutdownClose
    );
    assert!(matches!(
        timeout(TEST_TIMEOUT, peer.next()).await.expect("close"),
        Some(Ok(Message::Close(_)))
    ));
}

#[tokio::test]
async fn reader_panic_and_writer_abort_are_joined_before_retirement() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    lab.panic_reader.send(()).expect("inject panic");
    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire_after_failure())
        .await
        .expect("join pumps");
    assert!(joined.reader.expect_err("panic is observed").is_panic());
    assert_eq!(joined.writer.expect("writer joined"), WriterExit::Stopped);
    peer_observes_drop(&mut peer).await;

    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::Ready, (2, 8), (2, 8));
    let milestone = lab.write(Message::binary(vec![1])).expect("admit");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("stalled")
        .expect("signal");
    lab.supervisor.writer.abort();
    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire_after_failure())
        .await
        .expect("join pumps");
    assert!(joined
        .writer
        .expect_err("abort observed by join")
        .is_cancelled());
    assert_eq!(joined.reader.expect("reader joined"), ReaderExit::Stopped);
    assert_eq!(milestone.load(Ordering::SeqCst), NOT_ATTEMPTED);
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn stalled_shutdown_aborts_then_joins_before_the_peer_observes_drop() {
    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::Flush, (2, 8), (2, 8));
    let attempted = lab.write(Message::binary(vec![1])).expect("admit");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("flush stalled")
        .expect("signal");
    let joined = timeout(
        TEST_TIMEOUT,
        lab.supervisor
            .shutdown_with_deadline(Duration::from_millis(50)),
    )
    .await
    .expect("both handles joined after deadline");
    assert_eq!(joined.reader.expect("reader joined"), ReaderExit::Stopped);
    assert!(joined
        .writer
        .expect_err("stalled writer aborted")
        .is_cancelled());
    assert_eq!(attempted.load(Ordering::SeqCst), ATTEMPTED);
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn cancelling_a_requester_does_not_cancel_an_admitted_write() {
    let (socket, mut peer) = socket_pair().await;
    let mut lab = start_lab(socket, Stall::Flush, (2, 8), (2, 8));
    let (milestone_tx, milestone_rx) = oneshot::channel();
    let requester = tokio::spawn(async move {
        let milestone = Arc::new(AtomicU8::new(NOT_ATTEMPTED));
        lab.outbound
            .offer(
                Write {
                    frame: Message::binary(vec![1]),
                    milestone: milestone.clone(),
                },
                1,
            )
            .expect("admit");
        milestone_tx.send(milestone).expect("publish tracker");
        std::future::pending::<()>().await;
    });
    let milestone = milestone_rx.await.expect("admitted");
    timeout(TEST_TIMEOUT, &mut lab.blocked)
        .await
        .expect("flush stalled")
        .expect("signal");
    requester.abort();
    assert!(requester
        .await
        .expect_err("requester cancelled")
        .is_cancelled());
    assert_eq!(milestone.load(Ordering::SeqCst), ATTEMPTED);
    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(false))
        .await
        .expect("join pumps");
    assert_eq!(joined.reader.expect("reader joined"), ReaderExit::Stopped);
    assert_eq!(joined.writer.expect("writer joined"), WriterExit::Stopped);
    assert_eq!(milestone.load(Ordering::SeqCst), ATTEMPTED);
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn flushed_frame_is_peer_visible_without_claiming_client_acknowledgement() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    let milestone = lab.write(Message::binary(vec![1, 2])).expect("admit");
    let received = timeout(TEST_TIMEOUT, peer.next())
        .await
        .expect("peer reads frame")
        .expect("frame")
        .expect("valid frame");
    assert_eq!(received.into_data().as_ref(), &[1, 2]);
    timeout(TEST_TIMEOUT, async {
        while milestone.load(Ordering::SeqCst) != FLUSHED {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("local flush completes");
    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire(false))
        .await
        .expect("join pumps");
    assert_eq!(joined.reader.expect("reader joined"), ReaderExit::Stopped);
    assert_eq!(joined.writer.expect("writer joined"), WriterExit::Stopped);
    peer_observes_drop(&mut peer).await;
}

#[tokio::test]
async fn dropping_the_producer_is_not_a_substitute_for_joining_both_pumps() {
    let (socket, mut peer) = socket_pair().await;
    let lab = start_lab(socket, Stall::Never, (2, 8), (2, 8));
    drop(lab.outbound);
    assert!(
        !lab.supervisor.reader.is_finished(),
        "reader still owns its half"
    );
    let joined = timeout(TEST_TIMEOUT, lab.supervisor.retire_after_failure())
        .await
        .expect("join pumps");
    assert_eq!(
        joined.writer.expect("writer joined"),
        WriterExit::QueueClosed
    );
    assert_eq!(joined.reader.expect("reader joined"), ReaderExit::Stopped);
    peer_observes_drop(&mut peer).await;
}
