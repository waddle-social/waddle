use super::super::send::{
    close_ws_connection, send_ws_message, send_ws_message_with_authority, send_ws_text_frames,
    send_ws_text_frames_with_authority, AuthoritySendOutcome, InterruptibleSink,
};
use axum::extract::ws::Message;
use futures::Sink;
use std::pin::Pin;
use std::task::{Context, Poll};
use waddle_xmpp::registry::SocketWriteInterruption;

#[derive(Default)]
struct TestSink {
    fail_after: Option<usize>,
    closed: bool,
    sent: Vec<Message>,
}

impl TestSink {
    fn succeeds() -> Self {
        Self::default()
    }

    fn fails_after(sent_before_failure: usize) -> Self {
        Self {
            fail_after: Some(sent_before_failure),
            closed: false,
            sent: Vec::new(),
        }
    }
}

impl Sink<Message> for TestSink {
    type Error = &'static str;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        if matches!(self.fail_after, Some(limit) if self.sent.len() >= limit) {
            return Err("synthetic websocket sink failure");
        }

        self.sent.push(item);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.closed = true;
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn send_ws_text_frames_stops_after_first_send_failure() {
    let mut sink = TestSink::fails_after(1);

    let sent = send_ws_text_frames(
        &mut sink,
        vec!["<open/>".to_string(), "<features/>".to_string()],
        "synthetic failure",
    )
    .await;

    assert!(!sent);
    assert_eq!(sink.sent.len(), 1);
}

#[tokio::test]
async fn send_ws_message_returns_true_on_success() {
    let mut sink = TestSink::succeeds();

    let sent = send_ws_message(
        &mut sink,
        Message::Text("<pong/>".into()),
        "unexpected failure",
    )
    .await;

    assert!(sent);
    assert_eq!(sink.sent.len(), 1);
}

#[tokio::test]
async fn close_ws_connection_closes_sink() {
    let mut sink = TestSink::succeeds();

    let closed = close_ws_connection(&mut sink, "unexpected failure").await;

    assert!(closed);
    assert!(sink.closed);
}

#[tokio::test(flavor = "current_thread")]
async fn forced_detach_logs_debug_while_transport_errors_stay_errors() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    struct FailWithDetachTextSink;

    impl Sink<Message> for FailWithDetachTextSink {
        type Error = &'static str;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            // A real transport error must remain ERROR even if its text happens
            // to match the detach error.
            Err("connection force-detached")
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    let writer = CaptureWriter::default();
    let buffer = writer.0.clone();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(writer)
            .finish(),
    );
    let interrupt = SocketWriteInterruption::new();
    interrupt.cancel();
    let mut detached = InterruptibleSink::new(TestSink::succeeds(), interrupt);
    assert!(
        !send_ws_message(
            &mut detached,
            Message::Ping(Vec::new().into()),
            "detached-send"
        )
        .await
    );
    assert!(matches!(
        send_ws_message_with_authority(
            &mut detached,
            Message::Ping(Vec::new().into()),
            "detached-authority-ready",
            None
        )
        .await,
        AuthoritySendOutcome::TransportClosed
    ));
    assert!(!close_ws_connection(&mut detached, "detached-close").await);

    let interrupt = SocketWriteInterruption::new();
    let mut detach_on_ready = InterruptibleSink::new(
        DetachOnReadySink {
            interrupt: interrupt.clone(),
            sent: Vec::new(),
        },
        interrupt,
    );
    assert!(matches!(
        send_ws_message_with_authority(
            &mut detach_on_ready,
            Message::Ping(Vec::new().into()),
            "detached-authority-start",
            None
        )
        .await,
        AuthoritySendOutcome::TransportClosed
    ));

    let interrupt = SocketWriteInterruption::new();
    let mut flushing =
        InterruptibleSink::new(PendingFlushSink { sent: Vec::new() }, interrupt.clone());
    let send = send_ws_message_with_authority(
        &mut flushing,
        Message::Ping(Vec::new().into()),
        "detached-authority-flush",
        None,
    );
    tokio::pin!(send);
    assert!(futures::poll!(send.as_mut()).is_pending());
    interrupt.cancel();
    assert!(matches!(send.await, AuthoritySendOutcome::TransportClosed));

    let interrupt = SocketWriteInterruption::new();
    let mut failing = InterruptibleSink::new(FailWithDetachTextSink, interrupt);
    assert!(
        !send_ws_message(
            &mut failing,
            Message::Ping(Vec::new().into()),
            "wrapped-transport-error"
        )
        .await
    );
    let mut raw = TestSink::fails_after(0);
    assert!(
        !send_ws_message(
            &mut raw,
            Message::Ping(Vec::new().into()),
            "raw-transport-error"
        )
        .await
    );

    let logs =
        String::from_utf8(buffer.lock().expect("capture buffer lock").clone()).expect("UTF-8 logs");
    for message in [
        "detached-send",
        "detached-authority-ready",
        "detached-authority-start",
        "detached-authority-flush",
        "detached-close",
    ] {
        let line = logs
            .lines()
            .find(|line| line.contains(message))
            .expect("detach log");
        assert!(line.contains("DEBUG"), "expected debug detach log: {line}");
    }
    for message in ["wrapped-transport-error", "raw-transport-error"] {
        let line = logs
            .lines()
            .find(|line| line.contains(message))
            .expect("transport error log");
        assert!(
            line.contains("ERROR"),
            "expected error transport log: {line}"
        );
    }
}

/// A sink whose peer never drains: `poll_ready` pends forever, the
/// shape of a black-holed TCP path with a full send buffer.
struct StalledSink;

impl Sink<Message> for StalledSink {
    type Error = &'static str;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }

    fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
        unreachable!("poll_ready never completes")
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }
}

/// A ready transition that is concurrent with a lifecycle fence. It returns
/// `Ready` only after it has revoked the permit, reproducing the gap hidden
/// inside `SinkExt::send` between `poll_ready` and `start_send`.
struct RevokeBeforeStartSendSink {
    lifecycle: crate::clustering::NodeLifecycle,
    sent: Vec<Message>,
}

/// A transport write that has parked in `poll_ready`. Tests explicitly fence
/// the serving generation while the future is pending, then poll it again to
/// prove no stale frame reaches the synchronous `start_send` commit point.
pub(super) struct PendingReadySink {
    pub(super) sent: Vec<Message>,
}

pub(super) struct PendingFlushSink {
    pub(super) sent: Vec<Message>,
}

struct DetachOnReadySink {
    interrupt: SocketWriteInterruption,
    sent: Vec<Message>,
}

impl Sink<Message> for DetachOnReadySink {
    type Error = &'static str;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.interrupt.cancel();
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.sent.push(item);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn forced_detach_between_ready_and_start_send_suppresses_the_frame() {
    let interrupt = SocketWriteInterruption::new();
    let mut sink = InterruptibleSink::new(
        DetachOnReadySink {
            interrupt: interrupt.clone(),
            sent: Vec::new(),
        },
        interrupt,
    );
    assert!(!send_ws_message(&mut sink, Message::Text("<stale/>".into()), "detach").await);
    assert!(sink.uninterrupted().sent.is_empty());
}

impl Sink<Message> for PendingFlushSink {
    type Error = &'static str;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.sent.push(item);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn forced_detach_interrupts_ordinary_write_waiting_for_readiness() {
    let interrupt = SocketWriteInterruption::new();
    let mut sink = InterruptibleSink::new(PendingReadySink { sent: Vec::new() }, interrupt.clone());
    let send = send_ws_message(&mut sink, Message::Text("<message/>".into()), "detach");
    tokio::pin!(send);
    assert!(futures::poll!(send.as_mut()).is_pending());
    interrupt.cancel();
    assert!(
        !tokio::time::timeout(std::time::Duration::from_millis(100), send)
            .await
            .expect("forced detach must wake the parked readiness wait")
    );
}

#[tokio::test]
async fn forced_detach_interrupts_ordinary_write_waiting_for_flush() {
    let interrupt = SocketWriteInterruption::new();
    let mut sink = InterruptibleSink::new(PendingFlushSink { sent: Vec::new() }, interrupt.clone());
    {
        let send = send_ws_message(&mut sink, Message::Text("<message/>".into()), "detach");
        tokio::pin!(send);
        assert!(futures::poll!(send.as_mut()).is_pending());
        interrupt.cancel();
        assert!(
            !tokio::time::timeout(std::time::Duration::from_millis(100), send)
                .await
                .expect("forced detach must wake the parked flush wait")
        );
    }
    assert_eq!(
        sink.uninterrupted().sent.len(),
        1,
        "flush interruption follows transport acceptance"
    );
}

fn force_detach_request() -> waddle_xmpp::registry::ForceDetachRequest {
    let (ack, _receiver) = tokio::sync::oneshot::channel();
    waddle_xmpp::registry::ForceDetachRequest {
        origin: waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement,
        target_binding: None,
        requester_bare_jid: "alice@example.test".parse().expect("bare JID"),
        ack,
    }
}

fn force_detach_entry(
    endpoint: impl Into<waddle_xmpp::registry::SocketOutboundEndpoint>,
) -> waddle_xmpp::registry::ConnectionEntry {
    let connection_ref = waddle_xmpp::ownership::ConnectionRef::fresh(
        waddle_xmpp::ownership::NodeLeaseIdentity::local_process(),
    );
    waddle_xmpp::registry::ConnectionEntry::new_for_binding(
        endpoint,
        waddle_xmpp::ownership::BindingRef::fresh(connection_ref),
    )
}

#[tokio::test]
async fn interrupting_detach_offer_is_not_a_stop_until_its_gate_transition() {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let entry = force_detach_entry(tx);
    let mut requests = entry.take_force_detach_rx().expect("detach receiver");
    let (offered_tx, offered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let target = entry.clone();
    let stop = tokio::task::spawn_blocking(move || {
        target.try_force_detach_and_interrupt_after_offer_for_test(force_detach_request(), || {
            let _ = offered_tx.send(());
            release_rx.recv().expect("release post-offer stop");
        })
    });
    offered_rx.await.expect("detach offer completed");
    requests.try_recv().expect("detach request was offered");

    let mut sink = InterruptibleSink::new(TestSink::succeeds(), entry.write_interruption.clone());
    assert!(
        send_ws_message(
            &mut sink,
            Message::Text("<before-stop/>".into()),
            "before stop"
        )
        .await
    );
    assert_eq!(sink.uninterrupted().sent.len(), 1);

    release_tx.send(()).expect("allow stop transition");
    stop.await.expect("stop task").expect("stop succeeds");
    assert!(entry.write_interruption.is_cancelled());
    assert!(
        !send_ws_message(
            &mut sink,
            Message::Text("<after-stop/>".into()),
            "after stop"
        )
        .await
    );
    assert_eq!(sink.uninterrupted().sent.len(), 1);

    let (successor_tx, _successor_rx) = tokio::sync::mpsc::channel(1);
    let successor = force_detach_entry(successor_tx);
    assert_ne!(entry.connection_ref(), successor.connection_ref());
    let mut successor_requests = successor
        .take_force_detach_rx()
        .expect("successor receiver");
    let mut successor_sink =
        InterruptibleSink::new(TestSink::succeeds(), successor.write_interruption.clone());
    assert!(
        send_ws_message(
            &mut successor_sink,
            Message::Text("<successor/>".into()),
            "successor"
        )
        .await
    );
    assert_eq!(successor_sink.uninterrupted().sent.len(), 1);
    successor
        .try_force_detach_and_interrupt(force_detach_request())
        .expect("production interrupting detach succeeds");
    successor_requests
        .try_recv()
        .expect("successor detach offered");
    assert!(successor.write_interruption.is_cancelled());
    assert!(
        !send_ws_message(
            &mut successor_sink,
            Message::Text("<successor-after-stop/>".into()),
            "successor after stop",
        )
        .await
    );
    assert_eq!(successor_sink.uninterrupted().sent.len(), 1);
}

#[tokio::test]
async fn failed_interrupting_detach_offer_keeps_ordinary_writes_open() {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let entry = force_detach_entry(tx);
    let requests = entry.take_force_detach_rx().expect("detach receiver");
    let sender = entry.force_detach_sender();
    for _ in 0..sender.max_capacity() {
        sender
            .try_send(force_detach_request())
            .expect("fill detach queue");
    }
    assert!(matches!(
        entry.try_force_detach_and_interrupt(force_detach_request()),
        Err(waddle_xmpp::registry::ForceDetachOfferError::Full(_))
    ));
    assert!(!entry.write_interruption.is_cancelled());
    drop(requests);
    assert!(matches!(
        entry.try_force_detach_and_interrupt(force_detach_request()),
        Err(waddle_xmpp::registry::ForceDetachOfferError::Closed(_))
    ));
    assert!(!entry.write_interruption.is_cancelled());
    let mut sink = InterruptibleSink::new(TestSink::succeeds(), entry.write_interruption.clone());
    assert!(send_ws_message(&mut sink, Message::Text("<still-open/>".into()), "open").await);
    assert_eq!(sink.uninterrupted().sent.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_write_started_before_interrupting_stop_finishes_admission_first() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

    struct BlockingStart {
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        release: mpsc::Receiver<()>,
        admitted: Arc<AtomicUsize>,
    }

    impl Sink<Message> for BlockingStart {
        type Error = &'static str;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            self.admitted.fetch_add(1, Ordering::Relaxed);
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            self.release
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "start was not released")
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    let lifecycle = crate::clustering::NodeLifecycle::new();
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "first"));
    let inventory = crate::websocket_transport::IoCustodianInventory::new(1).expect("inventory");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let admission = crate::websocket_transport::SocketAdmission::clustered(
        &lifecycle,
        &identity,
        &inventory,
        shutdown.clone(),
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("socket admission")
    .into_legacy();
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let entry = force_detach_entry(tx);
    let mut requests = entry.take_force_detach_rx().expect("detach receiver");
    let admitted = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let send = tokio::spawn({
        let interrupt = entry.write_interruption.clone();
        let admitted = Arc::clone(&admitted);
        async move {
            let mut sink = InterruptibleSink::new(
                BlockingStart {
                    entered: Some(entered_tx),
                    release: release_rx,
                    admitted,
                },
                interrupt,
            );
            send_ws_message_with_authority(
                &mut sink,
                Message::Text("<in-flight/>".into()),
                "in-flight write",
                Some((admission.permit(), &shutdown)),
            )
            .await
        }
    });
    entered_rx.await.expect("ordinary start_send entered");
    let (offered_tx, offered_rx) = tokio::sync::oneshot::channel();
    let target = entry.clone();
    let mut stop = tokio::task::spawn_blocking(move || {
        target.try_force_detach_and_interrupt_after_offer_for_test(force_detach_request(), || {
            let _ = offered_tx.send(());
        })
    });
    offered_rx.await.expect("detach offer completed");
    requests.try_recv().expect("detach request was offered");
    let premature = tokio::time::timeout(Duration::from_millis(100), &mut stop).await;
    let stopped_during_write = entry.write_interruption.is_cancelled();
    release_tx.send(()).expect("release ordinary start_send");
    assert!(premature.is_err(), "stop waits for entered start_send");
    assert!(!stopped_during_write);
    stop.await.expect("stop task").expect("stop succeeds");
    assert!(matches!(
        send.await.expect("send task"),
        AuthoritySendOutcome::Sent | AuthoritySendOutcome::TransportClosed
    ));
    assert_eq!(admitted.load(Ordering::Relaxed), 1);
    let mut after_stop =
        InterruptibleSink::new(TestSink::succeeds(), entry.write_interruption.clone());
    assert!(!send_ws_message(&mut after_stop, Message::Text("<late/>".into()), "late").await);
    assert!(after_stop.uninterrupted().sent.is_empty());
}

async fn invalid_live_resume_preserves_blocked_write<S>(inner: S)
where
    S: Sink<Message, Error = &'static str> + Unpin,
{
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::registry::{ConnectionEntry, ForceDetachOutcome, LocalResumeReply};
    use waddle_xmpp::stream_management::SmResumeRejection;

    let mut conn = super::super::state::WsConnState::new();
    conn.sm_state
        .enable("owner-serviced-resume".to_owned(), true, Some(300));
    let stream_id = SmSessionId::try_from_wire("owner-serviced-resume").expect("valid stream ID");
    let owner: jid::FullJid = "alice@example.test/old".parse().expect("full JID");
    let (outbound, _outbound_rx) = tokio::sync::mpsc::channel(1);
    let binding_ref =
        waddle_xmpp::ownership::BindingRef::fresh(waddle_xmpp::ownership::ConnectionRef::fresh(
            waddle_xmpp::ownership::NodeLeaseIdentity::local_process(),
        ));
    let entry = ConnectionEntry::new_for_binding(outbound, binding_ref.clone());
    conn.write_interruption = entry.write_interruption.clone();
    conn.live_resume_control = waddle_xmpp::registry::LiveResumeControl::new(
        conn.sm_state.resume_checkpoint(),
        conn.write_interruption.clone(),
    );
    entry.set_sm_stream_id(Some(stream_id.clone()));
    assert!(entry.wire_resume_control(owner.clone(), conn.live_resume_control.clone()));
    let mut force_detach_rx = entry.take_force_detach_rx().expect("owner receiver");
    let mut sink = InterruptibleSink::new(inner, conn.write_interruption.clone())
        .with_resume_control(conn.live_resume_control.clone());
    let send = send_ws_message(&mut sink, Message::Text("<message/>".into()), "detach");
    tokio::pin!(send);
    assert!(futures::poll!(send.as_mut()).is_pending());

    // Only the sink's owner-side polling can inspect the current SM frontier;
    // the request itself must neither cancel I/O nor enqueue a destructive close.
    let invalid = conn.live_resume_control.request(
        binding_ref.clone(),
        stream_id.clone(),
        owner.to_bare(),
        1,
    );
    tokio::pin!(invalid);
    assert!(futures::poll!(invalid.as_mut()).is_pending());
    assert!(!conn.write_interruption.is_cancelled());
    assert!(futures::poll!(send.as_mut()).is_pending());
    assert_eq!(
        invalid.await,
        LocalResumeReply::Rejected(SmResumeRejection::HandledTooHigh { outbound_count: 0 })
    );
    assert!(!conn.write_interruption.is_cancelled());
    assert!(matches!(
        force_detach_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    // Rejecting a bad h must leave a later correct takeover usable, including
    // waking an owner whose socket never becomes ready or finishes flushing.
    let valid = conn
        .live_resume_control
        .request(binding_ref, stream_id, owner.to_bare(), 0);
    tokio::pin!(valid);
    assert!(futures::poll!(valid.as_mut()).is_pending());
    assert!(
        !tokio::time::timeout(std::time::Duration::from_millis(100), send)
            .await
            .expect("validated resume interrupts blocked write")
    );
    let request = force_detach_rx.try_recv().expect("validated force detach");
    assert!(conn.write_interruption.is_cancelled());
    assert_eq!(request.requester_bare_jid, owner.to_bare());
    request
        .ack
        .send(ForceDetachOutcome::Detached)
        .expect("resume caller still waiting");
    assert_eq!(valid.await, LocalResumeReply::Detached);
}

#[tokio::test]
async fn invalid_live_resume_does_not_interrupt_blocked_ready_before_valid_takeover() {
    invalid_live_resume_preserves_blocked_write(PendingReadySink { sent: Vec::new() }).await;
}

#[tokio::test]
async fn invalid_live_resume_does_not_interrupt_blocked_flush_before_valid_takeover() {
    invalid_live_resume_preserves_blocked_write(PendingFlushSink { sent: Vec::new() }).await;
}

async fn interrupted_batch_retains_replay<S>(sink: S)
where
    S: Sink<Message, Error = &'static str> + Unpin,
{
    use super::super::batch_write::{
        write_response_batch_with_admission, BatchAuthority, BatchSmPolicy, BatchWriteOutcome,
    };

    let state = super::create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut conn = super::super::state::WsConnState::new();
    conn.sm_state
        .enable("interrupted-batch".to_owned(), true, Some(300));
    let interrupt = conn.write_interruption.clone();
    let mut sink = InterruptibleSink::new(sink, interrupt.clone());
    let mut reader = futures::stream::pending::<Result<Message, std::convert::Infallible>>();
    let frames: Vec<String> = ["first", "second"]
        .into_iter()
        .map(|id| {
            super::super::transport_xml::element_to_xml(
                xmpp_parsers::minidom::Element::builder("message", waddle_xmpp::ns::JABBER_CLIENT)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                    .build(),
            )
        })
        .collect();
    let outcome = {
        let write = write_response_batch_with_admission(
            &mut sink,
            &mut reader,
            state.as_ref(),
            &mut conn,
            frames.clone(),
            BatchSmPolicy::Record,
            BatchAuthority {
                permit: &permit,
                shutdown: &shutdown,
            },
        );
        tokio::pin!(write);
        assert!(futures::poll!(write.as_mut()).is_pending());
        interrupt.cancel();
        tokio::time::timeout(std::time::Duration::from_millis(100), write)
            .await
            .expect("force detach must promptly return the batch to cleanup")
    };
    assert!(matches!(outcome, BatchWriteOutcome::TransportClosed));
    let replay = conn.sm_state.stanzas_to_resend(0);
    assert_eq!(conn.sm_state.outbound_count, 2);
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].stanza_xml, frames[0]);
    assert_eq!(replay[1].stanza_xml, frames[1]);
}

#[tokio::test]
async fn forced_detach_retains_batch_replay_while_ordinary_readiness_is_blocked() {
    interrupted_batch_retains_replay(PendingReadySink { sent: Vec::new() }).await;
}

#[tokio::test]
async fn forced_detach_retains_batch_replay_while_ordinary_flush_is_blocked() {
    interrupted_batch_retains_replay(PendingFlushSink { sent: Vec::new() }).await;
}

impl Sink<Message> for PendingReadySink {
    type Error = &'static str;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.sent.push(item);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Sink<Message> for RevokeBeforeStartSendSink {
    type Error = &'static str;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.lifecycle.begin_fenced_recovery();
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.sent.push(item);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn authority_bound_send_does_not_start_after_ready_revokes_its_generation() {
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut sink = RevokeBeforeStartSendSink {
        lifecycle,
        sent: Vec::new(),
    };

    let outcome = send_ws_message_with_authority(
        &mut sink,
        Message::Text("<stale/>".into()),
        "ready revocation",
        Some((&permit, &shutdown)),
    )
    .await;

    assert!(matches!(outcome, AuthoritySendOutcome::AuthorityRevoked));
    assert!(
        sink.sent.is_empty(),
        "a revoked frame must not reach start_send"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_admission_rejects_identity_aba_between_ready_and_start_send() {
    use std::sync::mpsc;
    use std::time::Duration;
    use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

    struct RotateOnReady {
        trigger: Option<tokio::sync::oneshot::Sender<()>>,
        completed: mpsc::Receiver<()>,
        sent: Vec<Message>,
    }

    impl Sink<Message> for RotateOnReady {
        type Error = &'static str;

        fn poll_ready(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            if let Some(trigger) = self.trigger.take() {
                trigger.send(()).expect("rotation task is waiting");
                self.completed
                    .recv_timeout(Duration::from_secs(5))
                    .expect("identity rotation completes while readiness is polled");
            }
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.sent.push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    let lifecycle = crate::clustering::NodeLifecycle::new();
    let initial = NodeIdentity::new("node", "first");
    let identity = SharedNodeIdentity::new(initial.clone());
    let inventory = crate::websocket_transport::IoCustodianInventory::new(1).expect("inventory");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let admission = crate::websocket_transport::SocketAdmission::clustered(
        &lifecycle,
        &identity,
        &inventory,
        shutdown.clone(),
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("socket admission")
    .into_legacy();
    let (trigger_tx, trigger_rx) = tokio::sync::oneshot::channel();
    let (completed_tx, completed_rx) = mpsc::channel();
    let rotate = tokio::spawn(async move {
        trigger_rx.await.expect("readiness was polled");
        identity.rotate(NodeIdentity::new("node", "second")).await;
        identity.rotate(initial).await;
        completed_tx.send(()).expect("sink still waiting");
    });
    let mut sink = RotateOnReady {
        trigger: Some(trigger_tx),
        completed: completed_rx,
        sent: Vec::new(),
    };

    let outcome = send_ws_message_with_authority(
        &mut sink,
        Message::Text("<stale/>".into()),
        "identity rotated during readiness",
        Some((admission.permit(), &shutdown)),
    )
    .await;
    rotate.await.expect("rotation task completes");
    assert!(matches!(outcome, AuthoritySendOutcome::AuthorityRevoked));
    assert!(sink.sent.is_empty(), "the stale frame must not be enqueued");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_admission_holds_identity_revision_through_start_send() {
    use std::sync::mpsc;
    use std::time::Duration;
    use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

    struct BlockingStart {
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl Sink<Message> for BlockingStart {
        type Error = &'static str;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            self.release
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "start_send was not released")
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    let lifecycle = crate::clustering::NodeLifecycle::new();
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "first"));
    let stamp = identity.current_stamp().expect("active identity");
    let inventory = crate::websocket_transport::IoCustodianInventory::new(1).expect("inventory");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let admission = crate::websocket_transport::SocketAdmission::clustered(
        &lifecycle,
        &identity,
        &inventory,
        shutdown.clone(),
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("socket admission")
    .into_legacy();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let send = tokio::spawn(async move {
        let mut sink = BlockingStart {
            entered: Some(entered_tx),
            release: release_rx,
        };
        send_ws_message_with_authority(
            &mut sink,
            Message::Text("<message/>".into()),
            "blocked start",
            Some((admission.permit(), &shutdown)),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .expect("start_send entered")
        .expect("sender remains alive");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let mut rotation = tokio::spawn(async move {
        let _ = started_tx.send(());
        identity.rotate(NodeIdentity::new("node", "second")).await;
    });
    started_rx.await.expect("rotation started");
    let premature = tokio::time::timeout(Duration::from_millis(100), &mut rotation).await;
    let still_current = !stamp.is_revoked();
    release_tx.send(()).expect("release start_send");
    assert!(premature.is_err(), "rotation must wait for the enqueue");
    assert!(
        still_current,
        "the old revision remains valid during enqueue"
    );
    assert!(matches!(
        send.await.expect("send task"),
        AuthoritySendOutcome::Sent
    ));
    rotation.await.expect("rotation completes after enqueue");
    assert!(stamp.is_revoked());
}

#[tokio::test]
async fn force_detach_conflict_frames_are_suppressed_when_revoked_while_ready_is_pending() {
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut sink = PendingReadySink { sent: Vec::new() };
    let outcome = {
        let send = send_ws_text_frames_with_authority(
            &mut sink,
            vec!["<conflict/>".to_owned(), "<close/>".to_owned()],
            "force-detach conflict",
            (&permit, &shutdown),
        );
        tokio::pin!(send);

        assert!(futures::poll!(send.as_mut()).is_pending());
        lifecycle.begin_fenced_recovery();
        send.await
    };

    assert!(matches!(outcome, AuthoritySendOutcome::AuthorityRevoked));
    assert!(
        sink.sent.is_empty(),
        "stale conflict frames must not commit"
    );
}

#[tokio::test]
async fn session_init_error_frames_are_suppressed_when_revoked_while_ready_is_pending() {
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut sink = PendingReadySink { sent: Vec::new() };
    let outcome = {
        let send = send_ws_text_frames_with_authority(
            &mut sink,
            vec!["<internal-server-error/>".to_owned(), "<close/>".to_owned()],
            "session initialization error",
            (&permit, &shutdown),
        );
        tokio::pin!(send);

        assert!(futures::poll!(send.as_mut()).is_pending());
        lifecycle.begin_fenced_recovery();
        send.await
    };

    assert!(matches!(outcome, AuthoritySendOutcome::AuthorityRevoked));
    assert!(
        sink.sent.is_empty(),
        "stale session-init error frames must not commit"
    );
}

/// Issue #1090 write-stall budget: a send that cannot make progress
/// must report failure within the 60s budget instead of parking the
/// connection task forever (which would freeze the keepalive clock —
/// the adversarial-review finding the budget exists to close). Paused
/// tokio time auto-advances past the timeout, so this pins the bound
/// without a real 60s wait.
#[tokio::test(start_paused = true)]
async fn send_ws_message_fails_when_the_sink_stalls_past_the_budget() {
    let mut sink = StalledSink;

    let sent = send_ws_message(&mut sink, Message::Text("<ping/>".into()), "expected stall").await;

    assert!(!sent, "a stalled send must report failure, not hang");
}

#[tokio::test(start_paused = true)]
async fn close_ws_connection_fails_when_the_sink_stalls_past_the_budget() {
    let mut sink = StalledSink;

    let closed = close_ws_connection(&mut sink, "expected stall").await;

    assert!(!closed, "a stalled close must report failure, not hang");
}
