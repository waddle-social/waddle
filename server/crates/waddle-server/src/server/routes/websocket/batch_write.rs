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
use super::stream_management::{apply_sm_ack, is_countable_stanza, is_mam_result_message};
use super::*;
use futures::FutureExt as _;
use waddle_xmpp::stream_management::SmRequest;

/// Outcome of writing a response batch.
#[must_use = "a closed transport must break the connection loop"]
pub(super) enum BatchWriteOutcome {
    /// Every frame was written; the connection loop continues.
    Continue,
    /// The transport went away mid-batch (send failure). The caller
    /// must break the connection loop; the SM unacked queue already
    /// holds every countable frame of the batch for resume replay.
    TransportClosed,
}

/// Write a response batch to the WebSocket, recording countable
/// stanzas into the XEP-0198 unacked queue one frame at a time and
/// interleaving an `<r/>` ack request after every `ack_threshold`th
/// countable stanza.
///
/// `record` is `false` only for SM resume replay batches, whose
/// stanzas already sit in the restored unacked queue with their
/// original sequence numbers.
pub(super) async fn write_response_batch<S, SE, R, RE>(
    sender: &mut S,
    reader: &mut R,
    state: &WebSocketState,
    conn: &mut WsConnState,
    frames: Vec<String>,
    record: bool,
) -> BatchWriteOutcome
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    let mut frames = frames.into_iter();
    while let Some(frame) = frames.next() {
        let request_ack = record_frame_for_replay(conn, &frame, record);
        if !send_ws_message(
            sender,
            Message::Text(frame.into()),
            "Failed to send WebSocket message",
        )
        .await
        {
            // This frame is already recorded; the rest of the batch
            // must be too, or the resume replay window silently loses
            // the tail of the batch.
            record_remaining_for_replay(conn, frames, record);
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
                record_remaining_for_replay(conn, frames, record);
                return BatchWriteOutcome::TransportClosed;
            }
            // Give already-arrived inbound frames a chance to land:
            // `<a/>` acks shrink the unacked queue mid-flood instead
            // of waiting for the whole batch to finish.
            if matches!(
                drain_ready_inbound(sender, reader, state, conn).await,
                DrainSignal::TransportClosed
            ) {
                record_remaining_for_replay(conn, frames, record);
                return BatchWriteOutcome::TransportClosed;
            }
        }
    }
    BatchWriteOutcome::Continue
}

/// Record one outbound frame into the XEP-0198 bookkeeping, returning
/// whether the `<r/>` cadence fired. XEP-0313 MAM result messages
/// advance the wire counter but stay out of the replay queue — resume
/// must not duplicate archive results, and a history sync must not
/// evict live stanzas (issue #1089).
fn record_frame_for_replay(conn: &mut WsConnState, frame: &str, record: bool) -> bool {
    if !record || !conn.sm_state.enabled || !is_countable_stanza(frame) {
        return false;
    }
    if is_mam_result_message(frame) {
        conn.sm_state.record_outbound_replay_exempt().request_ack
    } else {
        conn.sm_state.record_outbound(frame.to_string()).request_ack
    }
}

/// The transport died mid-batch: record every not-yet-written
/// countable frame so the resume replay window covers the entire
/// batch. There is no wire to follow up on, so the cadence signal is
/// moot (mirrors the detach-drain contract in `replay.rs`).
fn record_remaining_for_replay(
    conn: &mut WsConnState,
    frames: impl Iterator<Item = String>,
    record: bool,
) {
    for frame in frames {
        let _ = record_frame_for_replay(conn, &frame, record);
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

/// Non-blockingly pull every already-buffered inbound frame off the
/// socket. `<a/>` acks are applied immediately (they are what keeps
/// the unacked queue from evicting mid-flood); every other text frame
/// is deferred, in arrival order, for the main frame dispatcher.
/// Pings are answered inline so a mid-flood client keepalive isn't
/// starved; pongs/binary only count as liveness evidence.
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
