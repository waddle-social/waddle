//! Tests for the chunked XEP-0198-aware batch writer (issue #1089).
//!
//! The writer is what stops the unacked-queue eviction storm: it
//! records + writes frame by frame, follows every `ack_threshold`th
//! countable stanza with an `<r/>`, and drains already-arrived inbound
//! frames after each `<r/>` so `<a/>` acks shrink the queue mid-flood.

use super::super::transport_xml::{element_to_xml, websocket_stream_close_xml};
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
/// each time the outstanding count reaches 8, sends an off-cadence `<r/>`,
/// and blocks until the scripted client ack brings the window back down —
/// so the queue never evicts and no replay gap is ever marked.
#[tokio::test]
async fn send_window_pause_awaits_ack_so_a_burst_over_cap_never_evicts() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    // cap=10 → high=8, low=5. ack_threshold=100 so the per-N cadence never
    // fires and the only `<r/>` on the wire are the send-window ones.
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state.enable("paced".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    // The client fully acks at each pause: pauses land at outstanding 8, i.e.
    // outbound counts 8, 16, 24, 32, 40.
    let mut reader = reader_with(vec![
        ack_frame(8),
        ack_frame(16),
        ack_frame(24),
        ack_frame(32),
        ack_frame(40),
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
    assert_eq!(conn.sm_state.last_acked, 40, "final ack landed");
    assert_eq!(
        conn.sm_state.replay_gap_through(),
        None,
        "a paced burst must never evict, so no replay gap is marked"
    );
    assert!(
        conn.sm_state.queue_len() <= 10,
        "the retained queue never exceeds the cap under pacing"
    );
    let r_xml = SmRequest::to_xml();
    let texts = sink_texts(&sink);
    assert_eq!(
        texts.iter().filter(|t| **t == r_xml).count(),
        5,
        "exactly one off-cadence <r/> per pause (at 8/16/24/32/40)"
    );
    assert_eq!(texts.len(), 45, "40 stanzas + 5 pacing <r/>");
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
        let _ = conn.sm_state.record_outbound(countable_message(i));
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

/// When the client floods the socket with non-ack frames while the writer
/// is paused, the awaited `<a/>` cannot be read in order once 64 frames are
/// parked. The writer then degrades to the pre-#1219 evict-oldest behaviour
/// for the rest of the batch (this stream only) rather than wedging.
#[tokio::test]
async fn send_window_deferred_cap_degrades_to_eviction() {
    // This test intentionally evicts, bumping the process-global
    // `waddle_sm_unacked_evicted_total`. Hold the shared metrics lock so it
    // serializes against `write_response_batch_drains_acks_between_chunks_so_nothing_evicts`,
    // which asserts that counter's delta is zero under the same lock —
    // otherwise these two contend under parallel test execution.
    let _metrics_guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    conn.sm_state = StreamManagementState::with_config(10, 100);
    conn.sm_state.enable("degrade".to_string(), true, Some(300));

    let mut sink = CollectSink::default();
    // 64 non-ack frames (the DEFERRED_INBOUND_CAP) buffered ahead of any
    // ack, so the pause fills the deferred buffer and cannot make progress.
    let noise: Vec<Message> = (0..64)
        .map(|i| Message::Text(message_with_id(&format!("noise{i}")).into()))
        .collect();
    let mut reader = reader_with(noise);
    let frames: Vec<String> = (1..=20).map(countable_message).collect();

    let outcome = write_response_batch(
        &mut sink,
        &mut reader,
        state.as_ref(),
        &mut conn,
        frames,
        BatchSmPolicy::Record,
    )
    .await;

    assert!(
        matches!(outcome, BatchWriteOutcome::Continue),
        "the batch still completes under degrade"
    );
    assert_eq!(
        conn.sm_state.outbound_count, 20,
        "all frames were still written"
    );
    assert!(
        conn.sm_state.replay_gap_through().is_some(),
        "degrade falls back to evict-oldest, which marks the replay gap"
    );
    assert_eq!(
        conn.deferred_inbound.len(),
        64,
        "the parked non-ack frames stay for the main loop to process in order"
    );
}
