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

use super::send::{send_ws_message_with_authority, AuthoritySendOutcome};
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
    /// The node serving generation changed before the next record/write.
    AuthorityRevoked,
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

/// Deadline for a single XEP-0198 send-window pause (issue #1219). A
/// healthy client acks within one RTT; a pause that outlives this means
/// the peer stopped reading. The connection then records the batch tail
/// (capped at queue capacity) and closes into the normal
/// detach-for-resume path — a clean resume beats a poisoned one. Chosen
/// well under the 60 s `SEND_STALL_TIMEOUT` so a stalled pace is
/// resolved long before the send stall would fire.
const SEND_WINDOW_PAUSE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// Why a send-window pause loop returned (issue #1219).
enum SendWindowOutcome {
    /// The client acked enough that the window fell to the low watermark;
    /// resume writing.
    Recovered,
    /// The deferred-inbound buffer filled with non-ack frames while paused,
    /// so the awaited `<a/>` cannot be read in order. Degrade to the
    /// pre-#1219 evict-oldest behaviour for the rest of the batch (this
    /// stream only) and keep writing.
    DeferredCapReached,
    /// No recovering ack arrived before the deadline — the peer is dead or
    /// stalled. Caller records the tail (capped) and closes for resume.
    TimedOut,
    /// The transport went away while paused.
    TransportClosed,
    AuthorityRevoked,
}

fn batch_authoritative(
    authority: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> bool {
    authority
        .is_none_or(|(permit, shutdown)| !shutdown.is_cancelled() && permit.revalidate().is_ok())
}

async fn batch_authority_revoked(
    authority: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) {
    let Some((permit, shutdown)) = authority else {
        std::future::pending::<()>().await;
        return;
    };
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => {}
        _ = permit.revoked() => {}
    }
}

async fn send_window_message<S, E>(
    sender: &mut S,
    message: Message,
    failure_message: &'static str,
    authority: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> Result<(), SendWindowOutcome>
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    match send_ws_message_with_authority(sender, message, failure_message, authority).await {
        AuthoritySendOutcome::Sent => Ok(()),
        AuthoritySendOutcome::TransportClosed => Err(SendWindowOutcome::TransportClosed),
        AuthoritySendOutcome::AuthorityRevoked => Err(SendWindowOutcome::AuthorityRevoked),
    }
}

#[derive(Clone, Copy)]
pub(super) struct BatchAuthority<'a> {
    pub(super) permit: &'a crate::clustering::NodeAdmissionPermit,
    pub(super) shutdown: &'a tokio_util::sync::CancellationToken,
}

pub(super) async fn write_response_batch_with_admission<S, SE, R, RE>(
    sender: &mut S,
    reader: &mut R,
    state: &WebSocketState,
    conn: &mut WsConnState,
    frames: Vec<String>,
    policy: BatchSmPolicy,
    authority: BatchAuthority<'_>,
) -> BatchWriteOutcome
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    write_response_batch_impl(
        sender,
        reader,
        state,
        conn,
        frames,
        policy,
        Some((authority.permit, authority.shutdown)),
    )
    .await
}

async fn write_response_batch_impl<S, SE, R, RE>(
    sender: &mut S,
    reader: &mut R,
    state: &WebSocketState,
    conn: &mut WsConnState,
    frames: Vec<String>,
    policy: BatchSmPolicy,
    authority: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> BatchWriteOutcome
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    if !batch_authoritative(authority) {
        return BatchWriteOutcome::AuthorityRevoked;
    }
    let mut frames = frames.into_iter();
    // Send-window pacing applies ONLY to batches that actually grow the SM
    // unacked queue (issue #1219 review). A `ReplaySuppressed` batch is the
    // XEP-0198 resume replay: its stanzas are ALREADY in the restored unacked
    // queue and are re-sent without recording, so it never grows the window.
    // If a stream resumes with a backlog ≥ the high watermark, the pause latch
    // is already set from `restore_from_session`; pacing the replay batch
    // would block waiting for acks of frames it has not sent yet and livelock
    // the resume (re-introducing the #1219 poisoning class). The post-replay
    // connection-loop gate paces any NEW traffic and elicits the ack that
    // drains the restored backlog.
    let pacing = matches!(policy, BatchSmPolicy::Record);
    // Once the deferred buffer fills while paused we cannot read the awaited
    // ack in order, so pacing is abandoned for the rest of THIS batch and we
    // fall back to the pre-#1219 evict-oldest behaviour. Latched so we don't
    // re-enter the pause (and re-send `<r/>`) on every remaining frame.
    let mut send_window_degraded = false;
    // Pace on ENTRY too (Codex P2 review on PR #1234): if the batch is entered
    // while the window is ALREADY latched — a `Record` batch after a resume
    // restored a full unacked queue, or one dispatched from the inbound /
    // handoff arms while the loop-level outbound gate holds the pause — the
    // first frame must not be recorded before the window recovers, or
    // `record_outbound` could evict from an already-full queue and re-poison
    // resume. Await recovery before recording anything.
    if pacing && conn.sm_state.needs_send_pause() {
        match await_send_window_recovery(sender, reader, state, conn, authority).await {
            SendWindowOutcome::Recovered => {}
            SendWindowOutcome::DeferredCapReached => send_window_degraded = true,
            SendWindowOutcome::TransportClosed | SendWindowOutcome::TimedOut => {
                record_remaining_for_replay(conn, frames, policy);
                return BatchWriteOutcome::TransportClosed;
            }
            SendWindowOutcome::AuthorityRevoked => return BatchWriteOutcome::AuthorityRevoked,
        }
    }
    while let Some(frame) = frames.next() {
        if !batch_authoritative(authority) {
            return BatchWriteOutcome::AuthorityRevoked;
        }
        let request_ack = if should_record(conn, &frame, policy) {
            conn.sm_state.record_outbound(frame.clone()).request_ack
        } else {
            false
        };
        if !batch_authoritative(authority) {
            return BatchWriteOutcome::AuthorityRevoked;
        }
        if let Err(outcome) = send_window_message(
            sender,
            Message::Text(frame.into()),
            "Failed to send WebSocket message",
            authority,
        )
        .await
        {
            return match outcome {
                SendWindowOutcome::TransportClosed => {
                    record_remaining_for_replay(conn, frames, policy);
                    BatchWriteOutcome::TransportClosed
                }
                SendWindowOutcome::AuthorityRevoked => BatchWriteOutcome::AuthorityRevoked,
                SendWindowOutcome::Recovered
                | SendWindowOutcome::DeferredCapReached
                | SendWindowOutcome::TimedOut => unreachable!("send outcome only"),
            };
        }
        if request_ack {
            if !batch_authoritative(authority) {
                return BatchWriteOutcome::AuthorityRevoked;
            }
            if let Err(outcome) = send_window_message(
                sender,
                Message::Text(SmRequest::to_xml().into()),
                "Failed to send SM <r/> request",
                authority,
            )
            .await
            {
                return match outcome {
                    SendWindowOutcome::TransportClosed => {
                        record_remaining_for_replay(conn, frames, policy);
                        BatchWriteOutcome::TransportClosed
                    }
                    SendWindowOutcome::AuthorityRevoked => BatchWriteOutcome::AuthorityRevoked,
                    SendWindowOutcome::Recovered
                    | SendWindowOutcome::DeferredCapReached
                    | SendWindowOutcome::TimedOut => unreachable!("send outcome only"),
                };
            }
            // Give already-arrived inbound frames a chance to land:
            // `<a/>` acks shrink the unacked queue mid-flood instead
            // of waiting for the whole batch to finish.
            match drain_ready_inbound(sender, reader, state, conn, authority).await {
                DrainSignal::Idle => {}
                DrainSignal::TransportClosed => {
                    record_remaining_for_replay(conn, frames, policy);
                    return BatchWriteOutcome::TransportClosed;
                }
                DrainSignal::AuthorityRevoked => {
                    return BatchWriteOutcome::AuthorityRevoked;
                }
            }
        }
        // Send-window pacing (issue #1219): if recording this frame pushed
        // the outstanding unacked count over the high watermark, stop
        // feeding the queue and block until the client acks it back down —
        // so a MAM catch-up / fan-out burst can never overflow the 1000-slot
        // queue and poison resume. XEP-0198 §4: the server may request an
        // ack at any time and is under no obligation to transmit queued
        // stanzas immediately (xep-0198.xml:307/357).
        if pacing && !send_window_degraded && conn.sm_state.needs_send_pause() {
            match await_send_window_recovery(sender, reader, state, conn, authority).await {
                SendWindowOutcome::Recovered => {}
                SendWindowOutcome::DeferredCapReached => {
                    // Cannot read the awaited ack in order behind 64 parked
                    // frames; degrade to evict-oldest for the batch tail
                    // (this stream only). record_outbound above will evict
                    // and mark the replay gap exactly as pre-#1219.
                    send_window_degraded = true;
                }
                SendWindowOutcome::TransportClosed => {
                    record_remaining_for_replay(conn, frames, policy);
                    return BatchWriteOutcome::TransportClosed;
                }
                SendWindowOutcome::TimedOut => {
                    // Dead/stalled peer: record the untransmitted tail for
                    // replay (evicting + marking the replay gap if it no longer
                    // fits, so a later resume fails loud rather than silently
                    // omitting frames — Codex P1), then close into
                    // detach-for-resume via the loop break.
                    record_remaining_for_replay(conn, frames, policy);
                    return BatchWriteOutcome::TransportClosed;
                }
                SendWindowOutcome::AuthorityRevoked => {
                    return BatchWriteOutcome::AuthorityRevoked;
                }
            }
        }
    }
    BatchWriteOutcome::Continue
}

/// Block until the XEP-0198 send window recovers to the low watermark,
/// applying `<a/>` acks inline and parking every other inbound frame in
/// the deferred buffer (issue #1219). One off-cadence `<r/>` is sent on
/// entry and re-sent after each ack that does not yet recover the window,
/// because the wasm client acks only in response to a request. Bounded by
/// [`SEND_WINDOW_PAUSE_DEADLINE`] and [`DEFERRED_INBOUND_CAP`]; other
/// select concerns (shutdown, keepalive) are not serviced while parked, so
/// the deadline is the safety valve.
async fn await_send_window_recovery<S, SE, R, RE>(
    sender: &mut S,
    reader: &mut R,
    state: &WebSocketState,
    conn: &mut WsConnState,
    authority: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> SendWindowOutcome
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    if !batch_authoritative(authority) {
        return SendWindowOutcome::AuthorityRevoked;
    }
    waddle_xmpp::telemetry::reliability::increment_sm_send_window_pause();
    let deadline = tokio::time::Instant::now() + SEND_WINDOW_PAUSE_DEADLINE;
    // Elicit an ack immediately — nothing more is being written until the
    // window recovers, so the client must be prompted.
    if let Err(outcome) = send_window_message(
        sender,
        Message::Text(SmRequest::to_xml().into()),
        "Failed to send SM <r/> at send-window pause",
        authority,
    )
    .await
    {
        return outcome;
    }
    loop {
        if !batch_authoritative(authority) {
            return SendWindowOutcome::AuthorityRevoked;
        }
        if conn.sm_state.send_window_recovered() {
            return SendWindowOutcome::Recovered;
        }
        if conn.deferred_inbound.len() >= DEFERRED_INBOUND_CAP {
            return SendWindowOutcome::DeferredCapReached;
        }
        let next = match tokio::select! {
            biased;
            _ = batch_authority_revoked(authority) => {
                return SendWindowOutcome::AuthorityRevoked;
            }
            result = tokio::time::timeout_at(deadline, reader.next()) => result,
        } {
            Ok(next) => next,
            Err(_) => {
                waddle_xmpp::telemetry::reliability::increment_sm_send_window_pause_timeout();
                warn!(
                    stream_id = conn.sm_state.stream_id.as_deref().unwrap_or("<unset>"),
                    deadline_secs = SEND_WINDOW_PAUSE_DEADLINE.as_secs(),
                    unacked = conn.sm_state.unacked_count(),
                    queue_len = conn.sm_state.queue_len(),
                    "SM send-window pause timed out with no recovering ack; \
                     closing into detach-for-resume"
                );
                return SendWindowOutcome::TimedOut;
            }
        };
        if !batch_authoritative(authority) {
            return SendWindowOutcome::AuthorityRevoked;
        }
        match next {
            Some(Ok(Message::Text(text))) => {
                conn.note_transport_activity();
                if text.len() > MAX_FRAME_SIZE {
                    warn!(
                        len = text.len(),
                        max = MAX_FRAME_SIZE,
                        "Dropping oversized inbound frame during send-window pause"
                    );
                } else if let Some(h) = parse_sm_ack_h(text.as_str()) {
                    let responses =
                        apply_sm_ack(state, &mut conn.sm_state, &mut conn.phase, h).await;
                    if !batch_authoritative(authority) {
                        return SendWindowOutcome::AuthorityRevoked;
                    }
                    for response in responses {
                        if !batch_authoritative(authority) {
                            return SendWindowOutcome::AuthorityRevoked;
                        }
                        if let Err(outcome) = send_window_message(
                            sender,
                            Message::Text(response.into()),
                            "Failed to send SM ack stream error",
                            authority,
                        )
                        .await
                        {
                            return outcome;
                        }
                    }
                    if conn.phase.is_closing() {
                        return SendWindowOutcome::TransportClosed;
                    }
                    // The ack shrank the window (via acknowledge → the pause
                    // latch clears once it reaches the low watermark). If it
                    // is not recovered yet, re-request so the client keeps
                    // acking — it does not ack unprompted.
                    if !conn.sm_state.send_window_recovered() {
                        if !batch_authoritative(authority) {
                            return SendWindowOutcome::AuthorityRevoked;
                        }
                        if let Err(outcome) = send_window_message(
                            sender,
                            Message::Text(SmRequest::to_xml().into()),
                            "Failed to re-send SM <r/> during send-window pause",
                            authority,
                        )
                        .await
                        {
                            return outcome;
                        }
                    }
                } else {
                    conn.deferred_inbound.push_back(text);
                }
            }
            Some(Ok(Message::Ping(data))) => {
                conn.note_transport_activity();
                if let Err(outcome) = send_window_message(
                    sender,
                    Message::Pong(data),
                    "Failed to send pong",
                    authority,
                )
                .await
                {
                    return outcome;
                }
            }
            Some(Ok(Message::Pong(_))) | Some(Ok(Message::Binary(_))) => {
                conn.note_transport_activity();
            }
            Some(Ok(Message::Close(_))) => {
                info!("WebSocket close requested during send-window pause");
                return SendWindowOutcome::TransportClosed;
            }
            Some(Err(error)) => {
                error!(error = %error, "WebSocket error during send-window pause");
                return SendWindowOutcome::TransportClosed;
            }
            None => {
                debug!("WebSocket stream ended during send-window pause");
                return SendWindowOutcome::TransportClosed;
            }
        }
    }
}

fn should_record(conn: &WsConnState, frame: &str, policy: BatchSmPolicy) -> bool {
    conn.sm_state.enabled && matches!(policy, BatchSmPolicy::Record) && is_countable_stanza(frame)
}

/// The transport died mid-batch (or a send-window pause timed out): record
/// every not-yet-written countable frame so the resume replay window covers
/// the rest of the batch. The cadence signal is moot (no wire), which mirrors
/// the detach-drain contract in `replay.rs`.
///
/// Also used by the connection loop's shutdown path for responses to
/// frames the drain had deferred before the transport went away.
///
/// This records via [`StreamManagementState::record_outbound`], which — if
/// the queue is genuinely over capacity — evicts the oldest entry and marks
/// the replay gap. It deliberately does NOT silently drop the untransmitted
/// tail (Codex P1 review on PR #1234): dropping without a replay gap would let
/// a later `<resume/>` succeed against the client's old `h` while omitting
/// those never-written stanzas, i.e. a silent message loss. Marking the gap
/// instead makes the resume fail loud so the client fresh-binds and recovers
/// via MAM catch-up. This only runs once the transport is already gone — the
/// send-window pacing keeps the queue under cap during normal writing — so it
/// does not reintroduce the #1219 routine-burst poison loop.
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
    AuthorityRevoked,
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
    authority: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> DrainSignal
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    loop {
        if !batch_authoritative(authority) {
            return DrainSignal::AuthorityRevoked;
        }
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
                    if !batch_authoritative(authority) {
                        return DrainSignal::AuthorityRevoked;
                    }
                    for response in responses {
                        if !batch_authoritative(authority) {
                            return DrainSignal::AuthorityRevoked;
                        }
                        if let Err(outcome) = send_window_message(
                            sender,
                            Message::Text(response.into()),
                            "Failed to send SM ack stream error",
                            authority,
                        )
                        .await
                        {
                            return match outcome {
                                SendWindowOutcome::TransportClosed => DrainSignal::TransportClosed,
                                SendWindowOutcome::AuthorityRevoked => {
                                    DrainSignal::AuthorityRevoked
                                }
                                SendWindowOutcome::Recovered
                                | SendWindowOutcome::DeferredCapReached
                                | SendWindowOutcome::TimedOut => unreachable!("send outcome only"),
                            };
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
                if let Err(outcome) = send_window_message(
                    sender,
                    Message::Pong(data),
                    "Failed to send pong",
                    authority,
                )
                .await
                {
                    return match outcome {
                        SendWindowOutcome::TransportClosed => DrainSignal::TransportClosed,
                        SendWindowOutcome::AuthorityRevoked => DrainSignal::AuthorityRevoked,
                        SendWindowOutcome::Recovered
                        | SendWindowOutcome::DeferredCapReached
                        | SendWindowOutcome::TimedOut => unreachable!("send outcome only"),
                    };
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
