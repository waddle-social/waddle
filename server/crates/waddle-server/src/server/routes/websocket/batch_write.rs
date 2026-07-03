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
use super::stream_management::{apply_sm_ack, is_countable_stanza, is_mam_response_frame};
use super::*;
use futures::FutureExt as _;
use waddle_xmpp::stream_management::SmRequest;

/// How the writer records countable frames into XEP-0198 bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BatchSmPolicy {
    /// Record countable frames; XEP-0313 MAM response frames
    /// (`<result/>` carriers and the closing `<fin/>` IQ) are
    /// replay-exempt. ONLY legal for the requester's own connection
    /// batch, where MAM responses are server-generated — the MAM
    /// shape is spoofable, so relayed peer content must never get
    /// this policy.
    RecordWithMamExemption,
    /// Record every countable frame. Used for peer-relayed content
    /// (recipient-pass output), where a forged `{urn:xmpp:mam:2}`
    /// child must not opt a message out of the reliability layer.
    RecordAll,
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
/// acks are consumed (never parked), so ack draining keeps working on
/// later passes even at the cap.
const DEFERRED_INBOUND_CAP: usize = 64;

/// Write a response batch to the WebSocket, recording countable
/// stanzas into the XEP-0198 unacked queue one frame at a time and
/// interleaving an `<r/>` ack request after every `ack_threshold`th
/// countable stanza.
///
/// XEP-0198 counter discipline for replay-exempt frames: the client
/// counts a stanza in `h` only when it actually receives it, so an
/// exempt frame — which can never be re-delivered by replay — must
/// advance `outbound_count` only after its write succeeded. Queued
/// (non-exempt) frames are recorded before the write: if the send
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
        let class = if conn.sm_state.enabled {
            classify_frame(&frame, policy)
        } else {
            FrameSmClass::Untracked
        };
        // Queued frames are recorded pre-send (resume replays them if
        // the send fails). Replay-exempt frames must not be counted
        // until the write succeeds — see the function doc.
        let mut request_ack = match class {
            FrameSmClass::Queue => conn.sm_state.record_outbound(frame.clone()).request_ack,
            FrameSmClass::ReplayExempt | FrameSmClass::Untracked => false,
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
        if matches!(class, FrameSmClass::ReplayExempt) {
            request_ack = conn.sm_state.record_outbound_replay_exempt().request_ack;
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

/// XEP-0198 handling class of one outbound frame under a policy.
#[derive(Debug, Clone, Copy)]
enum FrameSmClass {
    /// Countable; recorded into the unacked replay queue.
    Queue,
    /// Countable MAM response frame; counts toward `h` only once
    /// actually written, never enters the replay queue.
    ReplayExempt,
    /// Not countable (nonza / stream frame), or recording suppressed.
    Untracked,
}

fn classify_frame(frame: &str, policy: BatchSmPolicy) -> FrameSmClass {
    if matches!(policy, BatchSmPolicy::ReplaySuppressed) || !is_countable_stanza(frame) {
        return FrameSmClass::Untracked;
    }
    if matches!(policy, BatchSmPolicy::RecordWithMamExemption) && is_mam_response_frame(frame) {
        return FrameSmClass::ReplayExempt;
    }
    FrameSmClass::Queue
}

/// The transport died mid-batch: record every not-yet-written
/// replayable frame so the resume replay window covers the rest of
/// the batch. Replay-exempt frames are dropped entirely — they were
/// never written, so counting them would permanently desync
/// `outbound_count` from the client's `h`; the client re-runs its
/// archive query instead. The cadence signal is moot (no wire), which
/// mirrors the detach-drain contract in `replay.rs`.
///
/// Also used by the connection loop's shutdown path for responses to
/// frames the drain had deferred before the transport went away.
pub(super) fn record_remaining_for_replay(
    conn: &mut WsConnState,
    frames: impl Iterator<Item = String>,
    policy: BatchSmPolicy,
) {
    if !conn.sm_state.enabled {
        return;
    }
    for frame in frames {
        if matches!(classify_frame(&frame, policy), FrameSmClass::Queue) {
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
                if let Some(h) = parse_sm_ack_h(text.as_str()) {
                    apply_sm_ack(state, &mut conn.sm_state, h).await;
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
/// Anything else — including oversized frames, which the main
/// dispatcher's `MAX_FRAME_SIZE` backstop must see and drop — returns
/// `None` and is left for the main frame dispatcher.
fn parse_sm_ack_h(frame: &str) -> Option<u32> {
    if frame.len() > MAX_FRAME_SIZE || !SmStanza::is_client_nonza_candidate(frame) {
        return None;
    }
    match SmStanza::parse(frame) {
        Some(SmStanza::Ack(ack)) => Some(ack.h),
        _ => None,
    }
}
