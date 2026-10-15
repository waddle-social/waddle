use super::super::send::{
    close_ws_connection, send_ws_message, send_ws_message_with_authority, send_ws_text_frames,
    send_ws_text_frames_with_authority, AuthoritySendOutcome, InterruptibleSink,
};
use axum::extract::ws::Message;
use futures::Sink;
use std::pin::Pin;
use std::task::{Context, Poll};

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
struct PendingReadySink {
    sent: Vec<Message>,
}

struct PendingFlushSink {
    sent: Vec<Message>,
}

struct DetachOnReadySink {
    interrupt: tokio_util::sync::CancellationToken,
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
    let interrupt = tokio_util::sync::CancellationToken::new();
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
    let interrupt = tokio_util::sync::CancellationToken::new();
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
    let interrupt = tokio_util::sync::CancellationToken::new();
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
    let replay = conn.sm_state.get_stanzas_to_resend(0);
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
