//! Tests for the chunked XEP-0198-aware batch writer (issue #1089).
//!
//! The writer is what stops the unacked-queue eviction storm: it
//! records + writes frame by frame, follows every `ack_threshold`th
//! countable stanza with an `<r/>`, and drains already-arrived inbound
//! frames after each `<r/>` so `<a/>` acks shrink the queue mid-flood.

use super::super::transport_xml::{element_to_xml, websocket_stream_close_xml};
use super::super::{
    batch_write::{
        write_response_batch_with_admission, BatchAuthority, BatchSmPolicy, BatchWriteOutcome,
    },
    state::{WebSocketState, WsConnState, TERMINAL_RECOVERY_QUEUE_CAP},
};
use super::create_test_websocket_state;
use futures::{stream, Sink, Stream, StreamExt as _};
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use waddle_xmpp::stream_management::{SmAck, SmRequest, StreamManagementState};
use waddle_xmpp::telemetry::attributes::SmEvictionPath;

use axum::extract::ws::Message;
use xmpp_parsers::minidom::Element;

/// Reader driven by a script: `Some(msg)` yields the message,
/// `None` yields one `Poll::Pending` (— "nothing more buffered right
/// now"). Exhausted scripts stay Pending forever, like a live socket.
struct ScriptedReader {
    script: std::collections::VecDeque<Option<Message>>,
}

impl ScriptedReader {
    fn new(script: Vec<Option<Message>>) -> Self {
        Self {
            script: script.into_iter().collect(),
        }
    }
}

impl Stream for ScriptedReader {
    type Item = Result<Message, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.script.pop_front() {
            Some(Some(message)) => Poll::Ready(Some(Ok(message))),
            Some(None) | None => Poll::Pending,
        }
    }
}

fn ack_frame(h: u32) -> Message {
    Message::Text(SmAck::new(h).to_xml().into())
}

/// Sink that records every message written to it.
#[derive(Default)]
struct CollectSink {
    sent: Vec<Message>,
}

struct RevokeAfterFirstSink {
    sent: Vec<Message>,
    lifecycle: crate::clustering::NodeLifecycle,
}

/// Returns Ready only after it revokes the admitted generation. This models
/// the critical readiness race: the task was parked in `poll_ready`, the
/// lifecycle changed, and a later poll would otherwise call `start_send` in
/// the same `SinkExt::send` poll before its caller observes cancellation.
struct RevokeOnReadySink {
    sent: Vec<Message>,
    lifecycle: crate::clustering::NodeLifecycle,
    revoke_on_ready_call: usize,
    ready_calls: usize,
}

#[derive(Default)]
struct BackpressuredAfterEightSink {
    committed: Vec<Message>,
}

impl Sink<Message> for BackpressuredAfterEightSink {
    type Error = Infallible;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.committed.len() < 8 {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.committed.push(item);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Sink<Message> for RevokeAfterFirstSink {
    type Error = Infallible;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.sent.push(item);
        if self.sent.len() == 1 {
            self.lifecycle.begin_fenced_recovery();
        }
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Sink<Message> for RevokeOnReadySink {
    type Error = Infallible;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.ready_calls += 1;
        if self.ready_calls == self.revoke_on_ready_call {
            self.lifecycle.begin_fenced_recovery();
        }
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
async fn authority_revocation_stops_batch_before_next_record_or_write() {
    let state = create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut conn = WsConnState::new();
    conn.sm_state
        .enable("authority-batch".to_string(), true, Some(300));
    let mut sink = RevokeAfterFirstSink {
        sent: Vec::new(),
        lifecycle,
    };
    let mut reader = reader_with(vec![]);

    let outcome = write_response_batch_with_admission(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        vec![countable_message(1), countable_message(2)],
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::AuthorityRevoked));
    assert_eq!(sink.sent.len(), 1);
    assert_eq!(conn.sm_state.outbound_count, 2);
    assert_eq!(conn.sm_state.queue_len(), 2);
    let replay = conn.sm_state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].stanza_xml, countable_message(1));
    assert_eq!(replay[1].stanza_xml, countable_message(2));
}

#[tokio::test]
async fn pre_current_revocation_records_the_entire_countable_batch_for_replay() {
    let state = create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    lifecycle.begin_fenced_recovery();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut conn = WsConnState::new();
    conn.sm_state
        .enable("pre-current-revoked".to_string(), true, Some(300));
    let first = mam_result_frame(1);
    let second = mam_result_frame(2);
    let fin = mam_fin_frame();
    let mut sink = CollectSink::default();
    let mut reader = reader_with(vec![]);

    let outcome = write_response_batch_with_admission(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        vec![first.clone(), second.clone(), fin.clone()],
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::AuthorityRevoked));
    assert!(
        sink.sent.is_empty(),
        "revocation must fence every wire write"
    );
    let replay = conn.sm_state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 3);
    assert_eq!(replay[0].stanza_xml, first);
    assert_eq!(replay[1].stanza_xml, second);
    assert_eq!(replay[2].stanza_xml, fin);
}

/// A MAM batch can contain result messages followed by the query's final IQ.
/// Once an admitted generation revokes before the next `poll_ready`, the
/// written result is acknowledged and every unwritten countable response must
/// remain exactly once, in wire order, for the XEP-0198 resume window.
#[tokio::test]
async fn revoked_next_readiness_keeps_mam_result_tail_and_fin_for_resume() {
    let state = create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(100, 1);
    conn.sm_state
        .enable("revoked-mam-tail".to_string(), true, Some(300));
    let first = mam_result_frame(1);
    let second = mam_result_frame(2);
    let fin = mam_fin_frame();
    let mut sink = RevokeOnReadySink {
        sent: Vec::new(),
        lifecycle,
        // First result and its threshold `<r/>` commit. The next result's
        // readiness check observes revocation before `start_send`.
        revoke_on_ready_call: 3,
        ready_calls: 0,
    };
    let mut reader = reader_with(vec![ack_frame(1)]);

    let outcome = write_response_batch_with_admission(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        vec![first.clone(), second.clone(), fin.clone()],
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::AuthorityRevoked));
    let sent: Vec<String> = sink
        .sent
        .iter()
        .filter_map(|message| match message {
            Message::Text(text) => Some(text.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(sent, vec![first, SmRequest::to_xml()]);
    assert_eq!(
        conn.sm_state.last_acked, 1,
        "the inbound h settled the first result"
    );
    let replay = conn.sm_state.get_stanzas_to_resend(1);
    assert_eq!(replay.len(), 2, "the tail must be retained exactly once");
    assert_eq!(replay[0].stanza_xml, second);
    assert_eq!(replay[1].stanza_xml, fin);
}

#[tokio::test]
async fn ready_revocation_suppresses_the_normal_batch_frame_before_start_send() {
    let state = create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut conn = WsConnState::new();
    conn.sm_state
        .enable("ready-revoked-frame".to_string(), true, Some(300));
    let mut sink = RevokeOnReadySink {
        sent: Vec::new(),
        lifecycle,
        revoke_on_ready_call: 1,
        ready_calls: 0,
    };
    let mut reader = reader_with(vec![]);

    let outcome = write_response_batch_with_admission(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        vec![countable_message(1)],
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::AuthorityRevoked));
    assert!(
        sink.sent.is_empty(),
        "revoked frame must not reach start_send"
    );
    assert_eq!(
        conn.sm_state.outbound_count, 1,
        "the frame stays replayable"
    );
}

#[tokio::test]
async fn ready_revocation_suppresses_the_cadence_request_before_start_send() {
    let state = create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut conn = WsConnState::new();
    conn.sm_state
        .enable("ready-revoked-cadence".to_string(), true, Some(300));
    let mut sink = RevokeOnReadySink {
        sent: Vec::new(),
        lifecycle,
        // Five countable stanzas commit first; the sixth send is the XEP-0198
        // cadence `<r/>` that must be suppressed.
        revoke_on_ready_call: 6,
        ready_calls: 0,
    };
    let mut reader = reader_with(vec![]);
    let frames: Vec<String> = (1..=5).map(countable_message).collect();

    let outcome = write_response_batch_with_admission(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::AuthorityRevoked));
    assert_eq!(sink.sent.len(), 5, "the stale `<r/>` must not commit");
    assert!(sink
        .sent
        .iter()
        .filter_map(|message| match message {
            Message::Text(text) => Some(text),
            _ => None,
        })
        .all(|text| text.as_str() != SmRequest::to_xml()));
}

#[tokio::test]
async fn ready_revocation_suppresses_the_mid_batch_drain_response_before_start_send() {
    let state = create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut conn = WsConnState::new();
    conn.sm_state
        .enable("ready-revoked-drain".to_string(), true, Some(300));
    let mut sink = RevokeOnReadySink {
        sent: Vec::new(),
        lifecycle,
        // Five stanzas plus their `<r/>` are valid. The bogus ack produces a
        // handled-count-too-high error in the mid-batch drain; suppress that
        // response if its readiness poll observes the new generation.
        revoke_on_ready_call: 7,
        ready_calls: 0,
    };
    let mut reader = ScriptedReader::new(vec![Some(ack_frame(999))]);
    let frames: Vec<String> = (1..=5).map(countable_message).collect();

    let outcome = write_response_batch_with_admission(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::AuthorityRevoked));
    assert_eq!(
        sink.sent.len(),
        6,
        "the stale drain response must not commit"
    );
    assert!(sink.sent.iter().all(|message| match message {
        Message::Text(text) => !text.to_string().contains("handled-count-too-high"),
        _ => true,
    }));
}

impl Sink<Message> for CollectSink {
    type Error = Infallible;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
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

fn sink_texts(sink: &CollectSink) -> Vec<String> {
    sink.sent
        .iter()
        .filter_map(|m| match m {
            Message::Text(t) => Some(t.to_string()),
            _ => None,
        })
        .collect()
}

/// Reader with the given frames already buffered, then Pending forever
/// (like a live socket with nothing more to read — NOT end-of-stream).
fn reader_with(frames: Vec<Message>) -> impl Stream<Item = Result<Message, Infallible>> + Unpin {
    stream::iter(frames.into_iter().map(Ok)).chain(stream::pending())
}

async fn write_response_batch<S, SE, R, RE>(
    sender: &mut S,
    reader: &mut R,
    state: &WebSocketState,
    conn: &mut WsConnState,
    frames: Vec<String>,
    policy: BatchSmPolicy,
) -> BatchWriteOutcome
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving test permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    write_response_batch_with_admission(
        sender,
        reader,
        state,
        conn,
        frames,
        policy,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await
}

fn message_with_id(id: &str) -> String {
    element_to_xml(
        Element::builder("message", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .build(),
    )
}

fn countable_message(i: usize) -> String {
    message_with_id(&format!("m{i}"))
}

/// XEP-0198 §4: the server may request acks at any time. A large
/// response batch must be followed by an `<r/>` every `ack_threshold`
/// countable stanzas — not by a single coalesced `<r/>` after the
/// whole batch (which is what let a MAM flood pin the unacked queue
/// at capacity before the client ever got a chance to ack).
#[tokio::test]
async fn write_response_batch_requests_ack_every_threshold_not_once_per_batch() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    // Default ack_threshold is 5.
    conn.sm_state.enable("chunk".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    let mut reader = reader_with(vec![]);
    let frames: Vec<String> = (1..=12).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    let texts = sink_texts(&sink);
    let r_xml = SmRequest::to_xml();
    // 12 stanzas + 2 interleaved <r/> (after the 5th and 10th).
    assert_eq!(texts.len(), 14, "wire: {texts:?}");
    assert_eq!(texts[5], r_xml, "<r/> must follow the 5th countable stanza");
    assert_eq!(
        texts[11], r_xml,
        "<r/> must follow the 10th countable stanza"
    );
    assert_eq!(
        texts.iter().filter(|t| **t == r_xml).count(),
        2,
        "no coalesced trailing <r/>, no per-frame spam"
    );
    // Stanzas stay in order around the interleaved requests.
    assert_eq!(texts[0], countable_message(1));
    assert_eq!(texts[4], countable_message(5));
    assert_eq!(texts[6], countable_message(6));
    assert_eq!(texts[13], countable_message(12));
    assert_eq!(conn.sm_state.outbound_count, 12);
    assert_eq!(conn.sm_state.queue_len(), 12);
}

/// Issue #1089 acceptance: inbound `<a/>` acks are processed between
/// chunks of a large outbound batch, so a healthy client's acks drain
/// the unacked queue mid-flood and `waddle_sm_unacked_evicted_total`
/// stays flat — instead of the whole batch being recorded up front
/// and evicting one stanza per send once the cap is hit.
///
/// The scripted reader releases exactly one ack per drain window
/// (each followed by a Pending), so this only passes if the writer
/// drains after EVERY `<r/>`, not just once at the end.
#[tokio::test]
async fn write_response_batch_drains_acks_between_chunks_so_nothing_evicts() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;

    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    // Queue cap 10 with 50 countable frames: without mid-batch ack
    // draining this MUST evict (50 > 10); with draining it never does.
    conn.sm_state = StreamManagementState::with_config(10, 5);
    conn.sm_state.enable("drain".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    // One ack per <r/> window: after the k-th <r/> the client has
    // acked through h = 5k. Nine <r/>s fire for 50 frames (45th is
    // the last threshold multiple before the batch ends).
    let mut script: Vec<Option<Message>> = Vec::new();
    for k in 1..=9 {
        script.push(Some(ack_frame(5 * k)));
        script.push(None); // drain window ends: socket has nothing more
    }
    let mut reader = ScriptedReader::new(script);
    let frames: Vec<String> = (1..=50).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(
        metrics
            .counter_sum("xmpp.sm.unacked_evicted", &[])
            .unwrap_or(0),
        0,
        "healthy acking client must not evict anything from the replay window"
    );
    assert_eq!(
        conn.sm_state.replay_gap_through(),
        None,
        "no eviction means no replay gap — resume stays possible"
    );
    assert_eq!(conn.sm_state.last_acked, 45, "all scripted acks applied");
    assert_eq!(
        conn.sm_state.queue_len(),
        5,
        "only the final unacked chunk (46..=50) remains queued"
    );
}

/// Frames drained mid-batch that are NOT `<a/>` acks must reach the
/// main dispatcher in arrival order — the drain may consume acks out
/// of band, but must never reorder or drop anything else.
#[tokio::test]
async fn drained_non_ack_frames_are_deferred_in_arrival_order() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(100, 5);
    conn.sm_state.enable("defer".to_string(), true, Some(300));

    let chat = element_to_xml(
        Element::builder("message", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "live-1")
            .append(
                Element::builder("body", "jabber:client")
                    .append("hi")
                    .build(),
            )
            .build(),
    );
    let client_r = SmRequest::to_xml();
    let mut sink = CollectSink::default();
    let mut reader = ScriptedReader::new(vec![
        Some(Message::Text(chat.clone().into())),
        Some(ack_frame(5)),
        Some(Message::Text(client_r.clone().into())),
        None,
    ]);
    let frames: Vec<String> = (1..=5).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    // The interleaved ack was applied out of band...
    assert_eq!(conn.sm_state.last_acked, 5);
    assert_eq!(conn.sm_state.queue_len(), 0);
    // ...while the live message and the client's own <r/> are queued
    // for the main dispatcher, in arrival order.
    let deferred: Vec<String> = conn
        .deferred_inbound
        .iter()
        .map(|t| t.to_string())
        .collect();
    assert_eq!(deferred, vec![chat, client_r]);
}

/// Deliberate design decision (issue #1089 adversarial review): MAM
/// result messages are recorded into the replay window like ANY other
/// stanza. An exemption ("count toward h, never replay") permanently
/// desyncs `outbound_count` from the client's `h` the moment a
/// written result is lost in flight — XEP-0198 offers no legal way to
/// count a stanza the client will never receive. A replayed result is
/// harmless (XEP-0313 §6.1: clients MUST ignore results they did not
/// request); queue pressure from archive floods is handled by the
/// chunked writer's mid-batch ack draining instead. This also means a
/// message spoofing the MAM shape gets no special treatment anywhere.
#[tokio::test]
async fn mam_results_are_recorded_like_any_stanza_for_counter_convergence() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(100, 5);
    conn.sm_state.enable("mam".to_string(), true, Some(300));

    let live = countable_message(1);
    let mut frames = vec![live.clone()];
    frames.extend((1..=3).map(mam_result_frame));
    frames.push(element_to_xml(
        Element::builder("iq", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "q1")
            .append(
                Element::builder("fin", "urn:xmpp:mam:2")
                    .attr(minidom::rxml::xml_ncname!("complete").to_owned(), "true")
                    .build(),
            )
            .build(),
    ));

    let mut sink = CollectSink::default();
    let mut reader = ScriptedReader::new(vec![]);
    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    // Live message + 3 results + fin: all counted AND all replayable,
    // so the server can always retransmit what the client's h lacks.
    assert_eq!(conn.sm_state.outbound_count, 5);
    assert_eq!(conn.sm_state.queue_len(), 5);
    let replay = conn.sm_state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 5);
    assert_eq!(replay[0].stanza_xml, live);
}

/// If the peer closes (or the socket dies) mid-batch, every countable
/// frame not yet written must still be recorded into the unacked
/// queue — the batch is no longer recorded up front, so an early exit
/// that skipped recording would silently drop the tail of the batch
/// from the resume replay window.
#[tokio::test]
async fn transport_close_mid_batch_still_records_unwritten_frames_for_replay() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(100, 5);
    conn.sm_state
        .enable("close-mid".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    // The drain after the first <r/> (frame 5) sees a Close frame.
    let mut reader = ScriptedReader::new(vec![Some(Message::Close(None))]);
    let frames: Vec<String> = (1..=12).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::TransportClosed));
    // Frames 1..=5 were written; 6..=12 were not — but ALL 12 must be
    // in the replay window so a resume delivers the tail.
    assert_eq!(conn.sm_state.outbound_count, 12);
    assert_eq!(conn.sm_state.queue_len(), 12);
    let replay = conn.sm_state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 12);
    assert_eq!(replay[11].stanza_xml, countable_message(12));
    // Only 5 stanzas + 1 <r/> actually hit the wire.
    assert_eq!(sink_texts(&sink).len(), 6);
}

/// `record=false` is the SM resume replay batch: those stanzas
/// already sit in the restored unacked queue under their original
/// sequence numbers. Re-recording would double-count `outbound_count`
/// and duplicate queue entries; emitting `<r/>` is the enabled-path's
/// job once new traffic flows.
#[tokio::test]
async fn resume_replay_batch_is_written_without_recording_or_ack_requests() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(100, 5);
    conn.sm_state.enable("replay".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    let mut reader = ScriptedReader::new(vec![]);
    let frames: Vec<String> = (1..=12).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::ReplaySuppressed,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(conn.sm_state.outbound_count, 0);
    assert_eq!(conn.sm_state.queue_len(), 0);
    let texts = sink_texts(&sink);
    assert_eq!(texts.len(), 12, "all frames written, no <r/> interleaved");
    assert!(!texts.iter().any(|t| *t == SmRequest::to_xml()));
}

fn mam_result_frame(i: usize) -> String {
    element_to_xml(
        Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                "u@example.com/r",
            )
            .append(
                Element::builder("result", "urn:xmpp:mam:2")
                    .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q1")
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), i.to_string())
                    .append(Element::builder("forwarded", "urn:xmpp:forward:0").build())
                    .build(),
            )
            .build(),
    )
}

fn mam_fin_frame() -> String {
    element_to_xml(
        Element::builder("iq", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "mam-query")
            .append(
                Element::builder("fin", "urn:xmpp:mam:2")
                    .attr(minidom::rxml::xml_ncname!("complete").to_owned(), "true")
                    .build(),
            )
            .build(),
    )
}

/// Oversized inbound frames are dropped by the drain immediately —
/// the main dispatcher's MAX_FRAME_SIZE backstop would discard them
/// anyway, so parking them would only retain up to 64 near-1MiB
/// payloads in `deferred_inbound` until the loop chews through the
/// backlog. Well-sized frames around them are still drained normally.
#[tokio::test]
async fn drain_drops_oversized_frames_instead_of_parking_them() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(100, 5);
    conn.sm_state
        .enable("oversize".to_string(), true, Some(300));

    let oversized = "a".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE + 1);
    let kept = message_with_id("kept-1");
    let mut reader = ScriptedReader::new(vec![
        Some(Message::Text(oversized.into())),
        Some(ack_frame(5)),
        Some(Message::Text(kept.clone().into())),
        None,
    ]);
    let mut sink = CollectSink::default();
    let frames: Vec<String> = (1..=5).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    // The ack behind the oversized frame was still applied...
    assert_eq!(conn.sm_state.last_acked, 5);
    // ...and only the well-sized frame was parked.
    let deferred: Vec<String> = conn
        .deferred_inbound
        .iter()
        .map(|t| t.to_string())
        .collect();
    assert_eq!(deferred, vec![kept]);
}

/// Normal drain parking leaves reserved headroom for an active
/// send-window pause, then stops reading so a flooding client hits TCP
/// backpressure instead of growing server heap without bound.
#[tokio::test]
async fn drain_stops_parking_before_the_reserved_ack_headroom() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10_000, 5);
    conn.sm_state.enable("cap".to_string(), true, Some(300));

    // 200 buffered non-ack frames, far more than the cap.
    let script: Vec<Option<Message>> = (0..200)
        .map(|i| Some(Message::Text(message_with_id(&format!("flood-{i}")).into())))
        .collect();
    let mut reader = ScriptedReader::new(script);
    let mut sink = CollectSink::default();
    // 10 countable frames → two <r/> drains.
    let frames: Vec<String> = (1..=10).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(
        conn.deferred_inbound.len(),
        56,
        "normal drain must retain the eight-frame ack headroom"
    );
}

/// Issue #1099, mid-batch path: a bogus-high `<a h/>` (a handled count
/// ahead of anything the server ever sent) drained between chunks must
/// terminate the stream with the handled-count-too-high stream error
/// followed by `<close/>`, signal transport-closed to the batch writer —
/// and must NOT purge the replay queue: XEP-0198 gives no legal way for
/// `h` to exceed the send count, so acknowledging it would destroy
/// stanzas the client provably never received.
#[tokio::test]
async fn bogus_high_ack_drained_mid_batch_closes_stream_without_purging_replay() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(100, 5);
    conn.sm_state
        .enable("bogus-high".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    // The drain after the first <r/> (frame 5) reads an ack claiming
    // the client handled 999 stanzas; the server has only sent 5.
    let mut reader = ScriptedReader::new(vec![Some(ack_frame(999))]);
    let frames: Vec<String> = (1..=12).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::TransportClosed));
    let texts = sink_texts(&sink);
    // 5 stanzas + <r/> + stream error + <close/> reached the wire.
    assert_eq!(texts.len(), 8, "wire: {texts:?}");
    assert!(
        texts[6].contains("handled-count-too-high") && texts[6].contains("999"),
        "the drained bogus ack must produce the stream error mid-batch: {}",
        texts[6]
    );
    assert_eq!(
        texts[7],
        websocket_stream_close_xml(),
        "the stream error must be followed by the framing <close/>"
    );
    assert!(
        conn.phase.is_closing(),
        "the connection phase must flip to Closing"
    );
    // The replay queue was NOT purged: the 5 written frames stay
    // unacked and the 7 unwritten ones were recorded for replay.
    assert_eq!(
        conn.sm_state.last_acked, 0,
        "bogus h must never be acknowledged"
    );
    assert_eq!(conn.sm_state.outbound_count, 12);
    assert_eq!(
        conn.sm_state.queue_len(),
        12,
        "every countable frame must stay replayable for a later resume"
    );
}

/// Round-2 #1099 review: an `<a h/>` mod-2^32 BEHIND `last_acked` (a
/// stale duplicate or garbage, e.g. h=0xC0000000 against last_acked=0)
/// sits in the half-space the wrap-aware too-high guard classifies as
/// "valid" — only the regression guard stops the numeric `<= h`
/// range-delete from wiping every pending row. It must be ignored
/// wholesale: no frames, no purge, and the batch keeps writing.
#[tokio::test]
async fn wrap_behind_stale_ack_drained_mid_batch_is_ignored_and_batch_continues() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(100, 5);
    conn.sm_state
        .enable("wrap-behind".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    let mut reader = ScriptedReader::new(vec![Some(ack_frame(0xC000_0000)), None]);
    let frames: Vec<String> = (1..=12).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    let texts = sink_texts(&sink);
    // The full batch was written: 12 stanzas + 2 interleaved <r/>,
    // and nothing else — no stream error, no <close/>.
    assert_eq!(texts.len(), 14, "wire: {texts:?}");
    assert!(
        !texts.iter().any(|t| t.contains("handled-count-too-high")),
        "a stale wrap-behind ack must not raise a stream error: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| *t == websocket_stream_close_xml()),
        "a stale wrap-behind ack must not close the stream: {texts:?}"
    );
    assert!(
        !conn.phase.is_closing(),
        "the connection phase must stay put — the stream keeps serving"
    );
    assert_eq!(
        conn.sm_state.last_acked, 0,
        "a regressed h must never advance last_acked"
    );
    assert_eq!(
        conn.sm_state.queue_len(),
        12,
        "the stale ack must not purge a single replayable frame"
    );
}

// ── XEP-0198 send-window pacing (issue #1219) ───────────────────────────

/// The core fix: a burst far larger than the unacked-queue cap paces on
/// the send window instead of overflowing it. With a cap of 10 (high
/// watermark 8, low watermark 5) and a 40-frame batch, the writer pauses
/// before each next countable frame once the outstanding count has reached 8,
/// sends an off-cadence `<r/>`, and blocks until the scripted client ack
/// brings the window back down. If the batch ends exactly on the high
/// watermark, the final `<r/>` is left to the connection loop's paused-window
/// gate; the batch writer itself still preserves the no-eviction invariant.
#[tokio::test]
async fn send_window_pause_awaits_ack_so_a_burst_over_cap_never_evicts() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    // cap=10 → high=8, low=5. ack_threshold=100 so the per-N cadence never
    // fires and the only `<r/>` on the wire are the send-window ones.
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state.enable("paced".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    // The client fully acks at each inline pause. With pre-countable pacing,
    // those pauses happen before frames 9/17/25/33, i.e. after counts
    // 8/16/24/32. The final eight frames are left outstanding for the
    // connection loop's paused-window gate to prompt.
    let mut reader = reader_with(vec![
        ack_frame(8),
        ack_frame(16),
        ack_frame(24),
        ack_frame(32),
    ]);
    let frames: Vec<String> = (1..=40).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(conn.sm_state.outbound_count, 40, "every frame was written");
    assert_eq!(
        conn.sm_state.last_acked, 32,
        "the batch writer applies only the four inline pause acks"
    );
    assert_eq!(
        conn.sm_state.replay_gap_through(),
        None,
        "a paced burst must never evict, so no replay gap is marked"
    );
    assert!(
        conn.sm_state.queue_len() <= 10,
        "the retained queue never exceeds the cap under pacing"
    );
    assert_eq!(
        conn.sm_state.queue_len(),
        8,
        "the final eight frames remain queued for the loop-level paused-window gate"
    );
    let r_xml = SmRequest::to_xml();
    let texts = sink_texts(&sink);
    assert_eq!(
        texts.iter().filter(|t| **t == r_xml).count(),
        4,
        "exactly one off-cadence <r/> per inline pause (at 8/16/24/32)"
    );
    assert_eq!(texts.len(), 44, "40 stanzas + 4 inline pacing <r/>");
}

#[tokio::test]
async fn backpressured_send_window_write_stops_promptly_on_authority_revocation() {
    let state = create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("authority-pause".to_string(), true, Some(300));
    let mut sink = BackpressuredAfterEightSink::default();
    let mut reader = reader_with(vec![]);
    let frames: Vec<String> = (1..=9).map(countable_message).collect();
    let mut writer = Box::pin(write_response_batch_with_admission(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    ));

    assert!(futures::poll!(writer.as_mut()).is_pending());
    lifecycle.begin_fenced_recovery();
    let outcome = tokio::time::timeout(std::time::Duration::from_millis(50), writer)
        .await
        .expect("authority revocation must preempt the 15 second pause deadline");

    assert!(matches!(outcome, BatchWriteOutcome::AuthorityRevoked));
    assert_eq!(
        sink.committed,
        (1..=8)
            .map(countable_message)
            .map(|frame| Message::Text(frame.into()))
            .collect::<Vec<_>>(),
        "the first eight stanzas committed before the backpressured ninth send"
    );
    assert_eq!(
        conn.sm_state.outbound_count, 9,
        "the blocked ninth stanza must join the replay window on revocation"
    );
    assert_eq!(conn.sm_state.queue_len(), 9);
    let replay = conn.sm_state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 9);
    for (index, stanza) in replay.iter().enumerate() {
        assert_eq!(
            stanza.stanza_xml,
            countable_message(index + 1),
            "the replay window contains each committed prefix frame and the unsent tail exactly once, in order"
        );
    }
    assert_eq!(
        sink.committed.len(),
        8,
        "the backpressured <r/> must never commit after revocation"
    );
}

/// A dead/stalled peer never acks the window down. The pause deadline fires
/// and the untransmitted tail is recorded for replay via `record_outbound` —
/// which, once the queue is over capacity, evicts the oldest entry AND marks
/// the replay gap. That is deliberate (Codex P1 review on PR #1234): the tail
/// must NOT be silently dropped, or a later resume against the client's old
/// `h` would succeed while omitting never-written stanzas. Marking the gap
/// makes such a resume fail loud so the client fresh-binds and recovers via
/// MAM. This no-wire path only runs once the transport is already gone, so it
/// does not reintroduce the #1219 routine-burst poison loop.
#[tokio::test]
async fn send_window_pause_timeout_records_tail_and_marks_replay_gap() {
    // The over-capacity tail eviction bumps the process-global
    // `waddle_sm_unacked_evicted_total`; hold the shared metrics lock so this
    // serializes against `write_response_batch_drains_acks_between_chunks_so_nothing_evicts`,
    // which asserts that counter's delta is zero under the same lock.
    let _metrics_guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state.enable("dead".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    // No acks ever — the reader is Pending forever.
    let mut reader = reader_with(vec![]);
    let frames: Vec<String> = (1..=30).map(countable_message).collect();

    // Paused clock: the 15 s pause deadline auto-advances once the runtime
    // is idle on the timeout future, so the test is instant.
    tokio::time::pause();
    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::TransportClosed));
    assert_eq!(
        conn.sm_state.queue_len(),
        10,
        "the retained queue stays at the cap"
    );
    assert_eq!(
        conn.sm_state.outbound_count, 30,
        "every frame is recorded (its sequence advances the counter), not silently dropped"
    );
    assert!(
        conn.sm_state.replay_gap_through().is_some(),
        "the untransmitted tail that no longer fits marks a replay gap so resume fails loud"
    );
    assert!(
        !conn.sm_state.can_resume_from(0),
        "a resume that needs the evicted prefix must be rejected, not silently short-replayed"
    );
}

/// Issue #1219 review regression: a `ReplaySuppressed` resume-replay batch
/// must NEVER send-window pause, even when the restored backlog already sits
/// at/above the high watermark. Pausing the replay would block waiting for
/// acks of frames it has not sent yet — a permanent resume livelock. The
/// replay re-sends already-queued stanzas without growing the window, so the
/// post-replay connection-loop gate is what paces subsequent new traffic.
#[tokio::test]
async fn replay_suppressed_batch_never_send_window_pauses_even_above_high_watermark() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    // cap=10 → high=8. Simulate a resumed stream whose restored backlog is
    // already above the high watermark: record 9 unacked stanzas so the pause
    // latch is set, exactly as restore_from_session would leave it.
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state.enable("resumed".to_string(), true, Some(300));
    for i in 0..9 {
        let _ = conn
            .sm_state
            .record_outbound(countable_message(i), SmEvictionPath::Batch);
    }
    assert!(
        conn.sm_state.needs_send_pause(),
        "precondition: the restored backlog latched the send-window pause"
    );
    let outbound_before = conn.sm_state.outbound_count;

    let mut sink = CollectSink::default();
    // Pending reader with NO acks: if the replay wrongly paused, it would
    // block here forever. The timeout turns a regression into a fast failure.
    let mut reader = reader_with(vec![]);
    let replay: Vec<String> = (100..103).map(countable_message).collect();

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        write_response_batch(
            &mut sink,
            &mut reader,
            state.as_ref(),
            &mut conn,
            replay,
            BatchSmPolicy::ReplaySuppressed,
        ),
    )
    .await
    .expect("ReplaySuppressed batch must not stall on the send-window pause");

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    let texts = sink_texts(&sink);
    assert_eq!(texts.len(), 3, "all 3 replay frames written, no <r/> pause");
    assert!(
        !texts.iter().any(|t| *t == SmRequest::to_xml()),
        "replay must not emit a send-window <r/>"
    );
    assert_eq!(
        conn.sm_state.outbound_count, outbound_before,
        "ReplaySuppressed must not record (grow) the window"
    );
}

/// Issue #1219 / #1234 review regression: a `Record` batch whose next frame
/// is uncountable control traffic must not trip send-window pacing just
/// because the pause latch is already set from earlier countable backlog.
/// The lone outbound `<a/>` cannot grow or evict the replay queue, so it must
/// write through even if the paused window would otherwise exhaust its
/// deferred-headroom reserve while waiting for a fresh ack.
#[tokio::test]
async fn control_only_record_batch_writes_through_a_latched_send_window() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("control-only-latched".to_string(), true, Some(300));
    for i in 0..8 {
        let _ = conn
            .sm_state
            .record_outbound(countable_message(i), SmEvictionPath::Batch);
    }
    assert!(
        conn.sm_state.needs_send_pause(),
        "precondition: existing countable backlog latched the send-window pause"
    );
    conn.deferred_inbound.extend(
        (0..64).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("noise{i}")))),
    );

    let control = SmAck::new(17).to_xml();
    let mut sink = CollectSink::default();
    let mut reader = reader_with(
        (0..8)
            .map(|i| Message::Text(message_with_id(&format!("paused-noise{i}")).into()))
            .collect(),
    );

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        vec![control.clone()],
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(
        sink_texts(&sink),
        vec![control],
        "the control frame must write immediately"
    );
    assert_eq!(
        conn.sm_state.queue_len(),
        8,
        "uncountable control traffic must not grow the replay queue"
    );
    assert!(
        !conn.sm_recovery_required,
        "the control-only batch must not terminalize the connection"
    );
    assert_eq!(
        conn.deferred_inbound.len(),
        64,
        "without a pause, the writer must not consume deferred-headroom reserve"
    );
}

/// Mixed batches still honor send-window pacing, but only once they reach the
/// first countable frame that would grow the replay queue. Any leading
/// uncountable control frame must preserve FIFO and write through before the
/// pause loop runs.
#[tokio::test]
async fn mixed_batch_pauses_only_when_it_reaches_the_first_countable_frame() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("mixed-latched".to_string(), true, Some(300));
    for i in 0..8 {
        let _ = conn
            .sm_state
            .record_outbound(countable_message(i), SmEvictionPath::Batch);
    }
    assert!(
        conn.sm_state.needs_send_pause(),
        "precondition: existing countable backlog latched the send-window pause"
    );

    let control = SmAck::new(17).to_xml();
    let countable = countable_message(99);
    let mut sink = CollectSink::default();
    let mut reader = reader_with(vec![ack_frame(8)]);

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        vec![control.clone(), countable.clone()],
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(
        sink_texts(&sink),
        vec![control, SmRequest::to_xml(), countable],
        "the uncountable control must write first; the pause runs only before the first countable"
    );
    assert_eq!(
        conn.sm_state.last_acked, 8,
        "the inline pause ack must settle the pre-existing backlog before the new countable frame records"
    );
    assert_eq!(
        conn.sm_state.queue_len(),
        1,
        "only the newly-recorded countable frame remains replayable"
    );
}

/// A full deferred buffer is terminal for a paused send window. The writer
/// must not abandon pacing and record the batch tail into the capped unacked
/// queue: that is the eviction path which previously poisoned resume.
#[tokio::test]
async fn send_window_deferred_cap_exhaustion_closes_without_eviction_or_silent_tail() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("cap-exhausted".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    // Model a full deferred queue whose freshly entered pause consumes its
    // replenished eight-frame ack-search allowance without finding an ack.
    conn.deferred_inbound.extend(
        (0..64).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("noise{i}")))),
    );
    let mut reader = reader_with(
        (0..8)
            .map(|i| Message::Text(message_with_id(&format!("terminal-noise{i}")).into()))
            .collect(),
    );
    let frames: Vec<String> = (1..=1200).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::DeferredCapExhausted));
    assert_eq!(
        conn.sm_state.outbound_count, 8,
        "only the paced prefix is accepted into SM ownership"
    );
    assert_eq!(
        conn.sm_state.replay_gap_through(),
        None,
        "cap exhaustion must not evict the recorded prefix"
    );
    assert_eq!(
        conn.sm_state.queue_len(),
        8,
        "the capped queue retains the complete recorded prefix"
    );
    assert_eq!(
        conn.deferred_inbound.len(),
        72,
        "the terminal batch consumes only its replenished bounded allowance"
    );
    assert!(
        conn.sm_recovery_required,
        "connection cleanup must promote the recorded prefix and reject resume"
    );
    assert_eq!(
        sink_texts(&sink)
            .iter()
            .filter(|text| text.contains("<message"))
            .count(),
        8,
        "the unrecorded tail was neither written nor silently accepted"
    );
}

/// Normal non-ack parking leaves the final eight slots for a paused writer.
/// An `<a/>` behind seven ordinary frames therefore still reaches the inline
/// ack handler and releases the window without closing the stream.
#[tokio::test]
async fn send_window_reserved_headroom_recovers_from_ack_behind_ordinary_frames() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("reserved-headroom".to_string(), true, Some(300));
    conn.deferred_inbound.extend(
        (0..56).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("parked{i}")))),
    );

    let mut sink = CollectSink::default();
    let mut reader = reader_with(
        (0..7)
            .map(|i| Message::Text(message_with_id(&format!("during-pause{i}")).into()))
            .chain(std::iter::once(ack_frame(8)))
            .collect(),
    );
    let frames: Vec<String> = (1..=9).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(conn.sm_state.last_acked, 8);
    assert_eq!(conn.deferred_inbound.len(), 63);
    assert!(!conn.sm_recovery_required);
}

/// Every successful ack roundtrip replenishes the next in-batch pause's
/// bounded ordinary-frame allowance. The second pause begins after the first
/// already parked seven frames, so a static 64-slot check would terminalize a
/// healthy client before it could read the second ack.
#[tokio::test]
async fn send_window_headroom_replenishes_for_second_pause_in_one_batch() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("two-pauses".to_string(), true, Some(300));
    conn.deferred_inbound.extend(
        (0..56).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("parked{i}")))),
    );

    let mut sink = CollectSink::default();
    let mut reader = reader_with(
        (0..7)
            .map(|i| Message::Text(message_with_id(&format!("first-pause{i}")).into()))
            .chain(std::iter::once(ack_frame(8)))
            .chain(
                (0..7).map(|i| Message::Text(message_with_id(&format!("second-pause{i}")).into())),
            )
            .chain(std::iter::once(ack_frame(16)))
            .collect(),
    );

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        (1..=18).map(countable_message).collect(),
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(conn.sm_state.last_acked, 16);
    assert_eq!(conn.deferred_inbound.len(), 70);
    assert!(!conn.sm_recovery_required);
}

/// A paused batch must service a parked client `<r/>` at the front of the
/// deferred queue before charging the last reserved slots. Otherwise the
/// pause can terminalize with the recovering `<a/>` still unread behind a
/// request we already had enough information to answer inline.
#[tokio::test]
async fn send_window_pause_answers_parked_client_request_before_cap_exhaustion() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("parked-client-request".to_string(), true, Some(300));
    conn.deferred_inbound
        .push_back(axum::extract::ws::Utf8Bytes::from(SmRequest::to_xml()));
    conn.deferred_inbound.extend(
        (0..63).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("parked{i}")))),
    );

    let mut sink = CollectSink::default();
    let mut reader = reader_with(
        (0..8)
            .map(|i| Message::Text(message_with_id(&format!("during-pause{i}")).into()))
            .chain(std::iter::once(ack_frame(8)))
            .collect(),
    );

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        (1..=9).map(countable_message).collect(),
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(conn.sm_state.last_acked, 8);
    assert_eq!(conn.deferred_inbound.len(), 71);
    assert!(!conn.sm_recovery_required);
    assert!(
        sink_texts(&sink).contains(&SmAck::new(0).to_xml()),
        "the paused batch must answer the parked client <r/> inline"
    );
}

/// Codex 1669 round 9: a parked client `<r/>` must be serviced even when
/// ordinary frames precede it in the deferred queue — leaving it charged
/// against deferred capacity lets a near-full queue terminalize before the
/// client's recovering `<a/>` can be read.
#[tokio::test]
async fn send_window_pause_answers_parked_client_request_behind_ordinary_frames() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("parked-request-behind".to_string(), true, Some(300));
    conn.deferred_inbound.extend(
        (0..30).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("ahead{i}")))),
    );
    conn.deferred_inbound
        .push_back(axum::extract::ws::Utf8Bytes::from(SmRequest::to_xml()));
    conn.deferred_inbound.extend(
        (0..33).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("behind{i}")))),
    );

    let mut sink = CollectSink::default();
    let mut reader = reader_with(
        (0..8)
            .map(|i| Message::Text(message_with_id(&format!("during-pause{i}")).into()))
            .chain(std::iter::once(ack_frame(8)))
            .collect(),
    );

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        (1..=9).map(countable_message).collect(),
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    assert_eq!(conn.sm_state.last_acked, 8);
    assert!(!conn.sm_recovery_required);
    assert!(
        sink_texts(&sink).contains(&SmAck::new(0).to_xml()),
        "the paused batch must answer a parked client <r/> that is NOT at the queue front"
    );
    assert!(
        !conn
            .deferred_inbound
            .iter()
            .any(|text| text.as_str().contains("urn:xmpp:sm:3") && text.as_str().contains("<r")),
        "the serviced request must no longer occupy deferred capacity"
    );
}

/// Codex 1669 round 9: cap exhaustion mid-batch must not drop the current
/// countable frame and the batch tail — the client's inbound `h` already
/// advanced for the stanzas these responses answer, so they are accepted
/// work and belong in the terminal recovery inventory.
#[tokio::test]
async fn deferred_cap_exhaustion_retains_current_frame_and_batch_tail() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("exhaustion-retains-tail".to_string(), true, Some(300));
    conn.deferred_inbound.extend(
        (0..56).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("parked{i}")))),
    );

    let mut sink = CollectSink::default();
    // The reader never produces the recovering ack: the pause exhausts the
    // reserved headroom and terminalizes on an early frame of the batch.
    let mut reader = reader_with(
        (0..64)
            .map(|i| Message::Text(message_with_id(&format!("no-ack{i}")).into()))
            .collect(),
    );

    let batch: Vec<String> = (1..=12).map(countable_message).collect();
    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        batch.clone(),
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::DeferredCapExhausted));
    assert!(conn.sm_recovery_required);
    let recorded_total = conn.sm_state.queue_len() + conn.terminal_sm_recovery.queue_len();
    assert_eq!(
        recorded_total,
        batch.len(),
        "every countable frame of the batch must be recorded (live prefix + terminal \
         inventory), not dropped at exhaustion"
    );
}

/// Recovered pauses replenish their local ack-search allowance, but a giant
/// batch cannot use repeated seven-frame interleaving to retain more than the
/// absolute pause-time ceiling. The sixth ack remains behind seven ordinary
/// frames and is deliberately not consumed once the ceiling binds.
#[tokio::test]
async fn send_window_absolute_deferred_ceiling_preserves_recorded_prefix() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("absolute-deferred-ceiling".to_string(), true, Some(300));
    conn.deferred_inbound.extend(
        (0..56).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("parked{i}")))),
    );

    let mut sink = CollectSink::default();
    let mut reader = reader_with(
        (1..=6)
            .flat_map(|pause| {
                (0..7)
                    .map(move |i| {
                        Message::Text(message_with_id(&format!("pause{pause}-ordinary{i}")).into())
                    })
                    .chain(std::iter::once(ack_frame(pause * 8)))
            })
            .collect(),
    );

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        (1..=1200).map(countable_message).collect(),
        BatchSmPolicy::Record,
    )
    .await;

    assert!(matches!(outcome, BatchWriteOutcome::DeferredCapExhausted));
    assert_eq!(
        conn.deferred_inbound.len(),
        96,
        "the queue never exceeds the absolute ceiling"
    );
    let deferred: Vec<String> = conn
        .deferred_inbound
        .iter()
        .map(|text| text.to_string())
        .collect();
    let expected_last_deferred = message_with_id("pause6-ordinary4");
    assert_eq!(
        deferred.last().map(String::as_str),
        Some(expected_last_deferred.as_str()),
        "ceiling binding stops before a sixth recovering ack can be consumed"
    );
    assert_eq!(
        conn.sm_state.last_acked, 40,
        "the five recovering acks remain applied"
    );
    assert_eq!(
        conn.sm_state.outbound_count, 48,
        "only the prefix through the terminal sixth pause enters SM ownership"
    );
    assert_eq!(
        conn.sm_state.replay_gap_through(),
        None,
        "the recorded prefix is retained without eviction"
    );
    assert_eq!(
        conn.sm_state.queue_len(),
        8,
        "the unacked recorded prefix remains intact"
    );
    assert!(conn.sm_recovery_required);
    assert_eq!(
        sink_texts(&sink)
            .iter()
            .filter(|text| text.contains("<message"))
            .count(),
        48,
        "the unwritten tail is not sent or silently accepted"
    );
}

/// After terminal recovery begins, one oversized fan-out must still obey the
/// terminal cap: preserve the live prefix, retain exactly the terminal cap,
/// drop the excess without eviction, and remain promotable.
#[test]
fn terminal_recovery_caps_a_single_fanout_without_eviction_and_stays_promotable() {
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(8, 100);
    conn.sm_state
        .enable("terminal-recovery".to_string(), true, Some(300));
    for sequence in 1..=8 {
        let _ = conn
            .sm_state
            .record_outbound(countable_message(sequence), SmEvictionPath::Batch);
    }
    conn.begin_terminal_sm_recovery();

    super::super::batch_write::record_remaining_for_replay(
        &mut conn,
        (9..=u32::try_from(TERMINAL_RECOVERY_QUEUE_CAP + 24).expect("range fits"))
            .map(|sequence| countable_message(sequence as usize)),
        BatchSmPolicy::Record,
    );

    assert_eq!(conn.sm_state.queue_len(), 8);
    assert_eq!(conn.sm_state.replay_gap_through(), None);
    assert_eq!(
        conn.terminal_sm_recovery.queue_len(),
        TERMINAL_RECOVERY_QUEUE_CAP
    );
    assert_eq!(conn.terminal_sm_recovery.replay_gap_through(), None);
    assert_eq!(
        conn.terminal_sm_recovery.outbound_count as usize, TERMINAL_RECOVERY_QUEUE_CAP,
        "dropped terminal tail must not invent replay sequence numbers"
    );
    assert_eq!(conn.terminal_sm_recovery_dropped, 16);
    assert!(conn.terminal_sm_recovery_drop_warned);

    let promoted = conn
        .terminal_sm_recovery
        .to_detached_session(waddle_xmpp::stream_management::DetachedSessionSnapshot {
            user_id: "user-1".to_string(),
            jid: "alice@example.com/terminal-recovery".parse().expect("jid"),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .expect("terminal queue remains promotable");
    assert_eq!(promoted.unacked_stanzas.len(), TERMINAL_RECOVERY_QUEUE_CAP);
}

/// A normal deferred backlog is deliberately kept below the physical cap, so
/// consecutive paced batches still have ack-read room. This is the regression
/// for the old sticky-cap behavior where every later batch immediately
/// degraded after the first flood.
#[tokio::test]
async fn send_window_near_full_deferred_backlog_stays_paced_across_batches() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state
        .enable("cross-batch-headroom".to_string(), true, Some(300));
    conn.deferred_inbound.extend(
        (0..56).map(|i| axum::extract::ws::Utf8Bytes::from(message_with_id(&format!("queued{i}")))),
    );
    let mut sink = CollectSink::default();
    let mut reader = reader_with(vec![ack_frame(8), ack_frame(16)]);

    for start in [1, 10] {
        let outcome = write_response_batch(
            &mut sink,
            &mut reader,
            state.as_ref(),
            &mut conn,
            (start..start + 9).map(countable_message).collect(),
            BatchSmPolicy::Record,
        )
        .await;
        assert!(matches!(outcome, BatchWriteOutcome::Continue));
    }

    assert_eq!(conn.sm_state.outbound_count, 18);
    assert_eq!(conn.sm_state.last_acked, 16);
    assert_eq!(conn.sm_state.replay_gap_through(), None);
    assert_eq!(conn.deferred_inbound.len(), 56);
    assert!(!conn.sm_recovery_required);
}
