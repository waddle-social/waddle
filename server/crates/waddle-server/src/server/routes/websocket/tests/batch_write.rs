//! Tests for the chunked XEP-0198-aware batch writer (issue #1089).
//!
//! The writer is what stops the unacked-queue eviction storm: it
//! records + writes frame by frame, follows every `ack_threshold`th
//! countable stanza with an `<r/>`, and drains already-arrived inbound
//! frames after each `<r/>` so `<a/>` acks shrink the queue mid-flood.

use super::super::transport_xml::element_to_xml;
use super::super::{
    batch_write::{write_response_batch, BatchSmPolicy, BatchWriteOutcome},
    state::WsConnState,
};
use super::create_test_websocket_state;
use futures::{stream, Sink, Stream, StreamExt as _};
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use waddle_xmpp::stream_management::{SmAck, SmRequest, StreamManagementState};

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
    let _metrics_guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
    let evicted_before = sm_evicted_total();

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
        sm_evicted_total() - evicted_before,
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
    assert_eq!(replay[0], live);
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
    assert_eq!(replay[11], countable_message(12));
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

/// The drain parks non-ack frames in `deferred_inbound` — but only up
/// to a cap, past which it stops reading so a flooding client hits
/// TCP backpressure instead of growing server heap without bound.
#[tokio::test]
async fn drain_stops_parking_deferred_frames_at_the_cap() {
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
        64,
        "deferred queue must stop growing at the cap"
    );
}

fn sm_evicted_total() -> u64 {
    waddle_xmpp::prometheus::render_metrics()
        .lines()
        .find(|line| line.starts_with("waddle_sm_unacked_evicted_total "))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}
