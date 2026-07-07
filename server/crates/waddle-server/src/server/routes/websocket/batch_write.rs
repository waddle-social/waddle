//! Chunked XEP-0198-aware batch writer (issue #1089).
//!
//! Writing a large response batch used to record every countable
//! frame into the 1000-cap unacked queue before anything reached the
//! socket, emit one coalesced `<r/>` after the whole batch, and give
//! inbound `<a/>` acks no chance to drain the queue mid-batch. A MAM
//! history sync or fan-out burst pinned the queue at capacity and
//! evicted one stanza per subsequent send — permanently breaking
//! `<resume/>` for the stream.
//!
//! This writer records and writes frame by frame and follows every
//! `ack_threshold`th countable stanza with an `<r/>` (XEP-0198 §4
//! permits requesting acks at any time).

use super::send::send_ws_message;
use super::state::WsConnState;
use super::stream_management::{apply_sm_ack, is_countable_stanza};
use super::*;
use futures::FutureExt as _;
use waddle_xmpp::stream_management::SmRequest;

/// How the writer records countable frames into XEP-0198 bookkeeping.
///
/// Note there is deliberately NO replay exemption for XEP-0313 MAM
/// result frames (issue #1089 asked for one; adversarial review
/// killed it): the client's `h` counts every stanza it receives, and
/// a frame that is counted but can never be re-delivered by resume
/// replay permanently desyncs `outbound_count` from `h` the moment a
/// written frame is lost in flight — un-ackable queue entries and
/// duplicate replays forever after. Recording MAM results like any
/// other stanza keeps the counters convergent; a replayed result is
/// protocol-harmless (XEP-0313 §6.1: clients MUST ignore results they
/// did not request), and queue pressure from archive floods is solved
/// by this writer's chunking + mid-batch ack draining instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BatchSmPolicy {
    /// Record every countable frame into the unacked replay queue.
    Record,
    /// Record nothing. Only for SM resume replay batches, whose
    /// stanzas already sit in the restored unacked queue with their
    /// original sequence numbers.
    ReplaySuppressed,
}

/// Outcome of writing a response batch.
#[must_use = "a closed transport must break the connection loop"]
pub(super) enum BatchWriteOutcome {
    /// Every frame was written; the connection loop continues.
    Continue,
    /// The transport went away mid-batch (send failure). The caller
    /// must break the connection loop; the SM unacked queue already
    /// holds every replayable countable frame of the batch.
    TransportClosed,
}

/// Upper bound on frames the mid-batch drain may park in
/// [`WsConnState::deferred_inbound`]. Once reached the drain stops
/// reading, so a flooding client hits TCP backpressure again instead
/// of converting its send rate into unbounded server heap. `<a/>`
/// acks are consumed (never parked), acks only stop draining once 64
/// non-ack frames are already parked ahead of them — reads must stay
/// in order, so an ack behind parked frames cannot be consumed until
/// the connection loop processes the backlog. At the cap, behavior
/// degrades exactly to the pre-#1089 semantics (no mid-batch ack
/// draining: the queue may evict and mark a replay gap), and only
/// this connection's own stream is affected. Note a well-behaved
/// client can reach the cap during a very large batch — e.g. one
/// XEP-0184 receipt or XEP-0085 chat state per delivered message —
/// so this is a graceful-degradation bound, not a misbehavior gate.
const DEFERRED_INBOUND_CAP: usize = 64;

/// Write a response batch to the WebSocket, recording countable
/// stanzas into the XEP-0198 unacked queue one frame at a time and
/// interleaving an `<r/>` ack request after every `ack_threshold`th
/// countable stanza.
///
/// Frames are recorded just before their own write: if the send
/// fails they replay on resume, and the counters re-converge when
/// the client receives them.
pub(super) async fn write_response_batch<S, SE, R, RE>(
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
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    let mut frames = frames.into_iter();
    while let Some(frame) = frames.next() {
        let request_ack = if should_record(conn, &frame, policy) {
            conn.sm_state.record_outbound(frame.clone()).request_ack
        } else {
            false
        };
        if !send_ws_message(
            sender,
            Message::Text(frame.into()),
            "Failed to send WebSocket message",
        )
        .await
        {
            record_remaining_for_replay(conn, frames, policy);
            return BatchWriteOutcome::TransportClosed;
        }
        if request_ack {
            if !send_ws_message(
                sender,
                Message::Text(SmRequest::to_xml().into()),
                "Failed to send SM <r/> request",
            )
            .await
            {
                record_remaining_for_replay(conn, frames, policy);
                return BatchWriteOutcome::TransportClosed;
            }
            // Give already-arrived inbound frames a chance to land:
            // `<a/>` acks shrink the unacked queue mid-flood instead
            // of waiting for the whole batch to finish.
            if matches!(
                drain_ready_inbound(sender, reader, state, conn).await,
                DrainSignal::TransportClosed
            ) {
                record_remaining_for_replay(conn, frames, policy);
                return BatchWriteOutcome::TransportClosed;
            }
        }
    }
    BatchWriteOutcome::Continue
}

fn should_record(conn: &WsConnState, frame: &str, policy: BatchSmPolicy) -> bool {
    conn.sm_state.enabled && matches!(policy, BatchSmPolicy::Record) && is_countable_stanza(frame)
}

/// The transport died mid-batch: record every not-yet-written
/// countable frame so the resume replay window covers the rest of
/// the batch. The cadence signal is moot (no wire), which mirrors the
/// detach-drain contract in `replay.rs`.
///
/// Also used by the connection loop's shutdown path for responses to
/// frames the drain had deferred before the transport went away.
pub(super) fn record_remaining_for_replay(
    conn: &mut WsConnState,
    frames: impl Iterator<Item = String>,
    policy: BatchSmPolicy,
) {
    for frame in frames {
        if should_record(conn, &frame, policy) {
            let _ = conn.sm_state.record_outbound(frame);
        }
    }
}

/// Result of a non-blocking inbound drain pass.
enum DrainSignal {
    /// Socket has no more buffered frames right now; keep writing.
    Idle,
    /// Peer closed (WS close frame, stream end, or read error). The
    /// batch must stop and the connection loop must exit.
    TransportClosed,
}

/// Non-blockingly pull already-buffered inbound frames off the
/// socket. `<a/>` acks are applied immediately (they are what keeps
/// the unacked queue from evicting mid-flood); every other text frame
/// is deferred, in arrival order, for the main frame dispatcher — up
/// to [`DEFERRED_INBOUND_CAP`], past which the drain stops reading so
/// TCP backpressure throttles a flooding client. Pings are answered
/// inline so a mid-flood client keepalive isn't starved;
/// pongs/binary only count as liveness evidence.
async fn drain_ready_inbound<S, SE, R, RE>(
    sender: &mut S,
    reader: &mut R,
    state: &WebSocketState,
    conn: &mut WsConnState,
) -> DrainSignal
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    loop {
        if conn.deferred_inbound.len() >= DEFERRED_INBOUND_CAP {
            return DrainSignal::Idle;
        }
        let Some(next) = reader.next().now_or_never() else {
            // Nothing buffered right now — back to writing.
            return DrainSignal::Idle;
        };
        match next {
            Some(Ok(Message::Text(text))) => {
                conn.note_transport_activity();
                if text.len() > MAX_FRAME_SIZE {
                    // The main dispatcher's MAX_FRAME_SIZE backstop
                    // would drop this frame anyway; dropping it here
                    // keeps up to 64 near-1MiB frames from being
                    // retained in `deferred_inbound` until the loop
                    // gets around to processing the backlog.
                    warn!(
                        len = text.len(),
                        max = MAX_FRAME_SIZE,
                        "Dropping oversized inbound frame during mid-batch drain"
                    );
                } else if let Some(h) = parse_sm_ack_h(text.as_str()) {
                    // Applied ahead of any frames already parked in
                    // `deferred_inbound`. Safe ONLY because ack
                    // application is order-independent here: `h` is
                    // cumulative/monotone, and `delete_acked_in_window`
                    // removes rows keyed on the newly-acked window
                    // (last_acked, h] — outbound stanzas a deferred
                    // frame produces later get sequences past h and are
                    // untouched. If ack handling ever grows a side
                    // effect that is not keyed on the acked window, it
                    // must move to the deferred queue instead of
                    // running inline.
                    // Issue #1099: a handled-count-too-high `h` makes
                    // apply_sm_ack return the stream error + close
                    // frames and flip the phase to Closing instead of
                    // purging the replay queue. Write them and end the
                    // batch — the connection is terminating.
                    let responses =
                        apply_sm_ack(state, &mut conn.sm_state, &mut conn.phase, h).await;
                    for response in responses {
                        if !send_ws_message(
                            sender,
                            Message::Text(response.into()),
                            "Failed to send SM ack stream error",
                        )
                        .await
                        {
                            return DrainSignal::TransportClosed;
                        }
                    }
                    if conn.phase.is_closing() {
                        return DrainSignal::TransportClosed;
                    }
                } else {
                    conn.deferred_inbound.push_back(text);
                }
            }
            Some(Ok(Message::Ping(data))) => {
                conn.note_transport_activity();
                if !send_ws_message(sender, Message::Pong(data), "Failed to send pong").await {
                    return DrainSignal::TransportClosed;
                }
            }
            Some(Ok(Message::Pong(_))) | Some(Ok(Message::Binary(_))) => {
                conn.note_transport_activity();
            }
            Some(Ok(Message::Close(_))) => {
                info!("WebSocket close requested during outbound batch");
                return DrainSignal::TransportClosed;
            }
            Some(Err(error)) => {
                error!(error = %error, "WebSocket error during outbound batch drain");
                return DrainSignal::TransportClosed;
            }
            None => {
                debug!("WebSocket stream ended during outbound batch drain");
                return DrainSignal::TransportClosed;
            }
        }
    }
}

/// Parse a frame as an XEP-0198 `<a h='N'/>` nonza, returning `h`.
/// Anything else returns `None` and is left for the main frame
/// dispatcher. Oversized frames never reach this — the drain drops
/// them before parsing, mirroring the dispatcher's `MAX_FRAME_SIZE`
/// backstop.
fn parse_sm_ack_h(frame: &str) -> Option<u32> {
    if !SmStanza::is_client_nonza_candidate(frame) {
        return None;
    }
    match SmStanza::parse(frame) {
        Some(SmStanza::Ack(ack)) => Some(ack.h),
        _ => None,
    }
}
