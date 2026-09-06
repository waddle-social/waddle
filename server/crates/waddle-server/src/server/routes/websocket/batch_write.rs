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

use super::frame::ResponseFrame;
use super::send::{send_ws_message_with_authority, AuthoritySendOutcome};
use super::state::WsConnState;
use super::stream_management::{apply_sm_ack, is_countable_stanza};
use super::*;
use futures::FutureExt as _;
use waddle_xmpp::stream_management::{SmAck, SmRequest};
use waddle_xmpp::telemetry::attributes::SmEvictionPath;

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
    /// The paused send window exhausted its reserved inbound headroom before
    /// receiving a recovering ack. The connection must close through the
    /// non-resumable promotion path without recording the unwritten tail.
    DeferredCapExhausted,
    /// The node serving generation changed before the next record/write.
    AuthorityRevoked,
}

pub(super) struct BatchWriteReport {
    pub(super) outcome: BatchWriteOutcome,
    pub(super) accepted_frame_indices: Vec<usize>,
    /// Frames of this batch that were actually written to the transport
    /// (a prefix of the batch: frame 0 first). Distinct from
    /// `accepted_frame_indices`, which tracks frames recorded/retained for
    /// XEP-0198 replay whether or not they reached the wire.
    pub(super) written_frame_count: usize,
}

/// Normal upper bound on frames the mid-batch drain may park in
/// [`WsConnState::deferred_inbound`]. Once reached the drain stops
/// reading, so a flooding client hits TCP backpressure again instead
/// of converting its send rate into unbounded server heap.
const DEFERRED_INBOUND_CAP: usize = 64;

/// Frames reserved for a paused send window to read through ordinary client
/// traffic while looking for the `<a/>` that reopens it. Normal mid-batch
/// draining never consumes this capacity, so a pause entered near a full
/// deferred queue still gets a bounded chance to recover.
const RESERVED_ACK_HEADROOM: usize = 8;
const NORMAL_DEFERRED_INBOUND_CAP: usize = DEFERRED_INBOUND_CAP - RESERVED_ACK_HEADROOM;

/// Hard limit for deferred inbound frames while a paced batch is waiting for
/// XEP-0198 acknowledgement recovery. This bounds retained payload bytes to
/// [`DEFERRED_INBOUND_ABSOLUTE_CEILING`] times [`MAX_FRAME_SIZE`] (96 MiB at
/// the current 1 MiB frame limit), plus container overhead. Healthy
/// low-chatter clients acknowledge promptly without parking ordinary frames,
/// so their pauses stay well below this last-resort ceiling.
const DEFERRED_INBOUND_ABSOLUTE_CEILING: usize = DEFERRED_INBOUND_CAP + 4 * RESERVED_ACK_HEADROOM;

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
    /// Even the paused window's reserved inbound headroom filled before an
    /// ack arrived. The batch must stop without recording its unwritten tail:
    /// cleanup promotes the already-recorded queue and invalidates resume.
    DeferredCapExhausted,
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

pub(super) async fn write_response_batch_with_admission<S, SE, R, RE, F>(
    sender: &mut S,
    reader: &mut R,
    state: &WebSocketState,
    conn: &mut WsConnState,
    frames: Vec<F>,
    policy: BatchSmPolicy,
    authority: BatchAuthority<'_>,
) -> BatchWriteOutcome
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
    F: Into<ResponseFrame>,
{
    write_response_batch_report_with_admission(
        sender, reader, state, conn, frames, policy, authority,
    )
    .await
    .outcome
}

pub(super) async fn write_response_batch_report_with_admission<S, SE, R, RE, F>(
    sender: &mut S,
    reader: &mut R,
    state: &WebSocketState,
    conn: &mut WsConnState,
    frames: Vec<F>,
    policy: BatchSmPolicy,
    authority: BatchAuthority<'_>,
) -> BatchWriteReport
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
    F: Into<ResponseFrame>,
{
    write_response_batch_impl(
        sender,
        reader,
        state,
        conn,
        frames.into_iter().map(Into::into).collect(),
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
    frames: Vec<ResponseFrame>,
    policy: BatchSmPolicy,
    authority: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> BatchWriteReport
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    let total_frames = frames.len();
    let mut accepted_frame_indices = Vec::new();
    let mut written_frame_count = 0usize;
    let mut frames = frames.into_iter().enumerate();
    if !batch_authoritative(authority) {
        accepted_frame_indices.extend(record_remaining_for_replay_indexed(conn, frames, policy));
        return BatchWriteReport {
            outcome: BatchWriteOutcome::AuthorityRevoked,
            accepted_frame_indices,
            written_frame_count,
        };
    }
    // Send-window pacing applies ONLY before frames that would actually grow
    // the SM unacked queue (issue #1219 review). `ReplaySuppressed` resume
    // replay still never pauses because its frames are already queued, and a
    // `Record` batch whose next frame is uncountable control traffic must
    // write that control through even while a previously-recorded backlog has
    // the pause latch set.
    while let Some((frame_index, frame)) = frames.next() {
        if !batch_authoritative(authority) {
            accepted_frame_indices.extend(record_current_and_remaining_for_replay_indexed(
                conn,
                frame_index,
                frame,
                frames,
                policy,
                false,
            ));
            return BatchWriteReport {
                outcome: BatchWriteOutcome::AuthorityRevoked,
                accepted_frame_indices,
                written_frame_count,
            };
        }
        let frame_xml = frame.clone().into_serialized_xml();
        let current_should_record = should_record(conn, &frame_xml, policy);
        if current_should_record && conn.sm_state.needs_send_pause() {
            match await_send_window_recovery(sender, reader, state, conn, authority).await {
                SendWindowOutcome::Recovered => {}
                SendWindowOutcome::DeferredCapExhausted => {
                    conn.begin_terminal_sm_recovery();
                    // The client's inbound `h` already advanced for the
                    // stanzas these responses answer (frame.rs handles before
                    // this writer runs), so the current frame and the batch
                    // tail are accepted work: record them into the terminal
                    // recovery inventory instead of dropping them — cleanup
                    // promotes them alongside the recorded prefix.
                    accepted_frame_indices.extend(record_current_and_remaining_for_replay_indexed(
                        conn,
                        frame_index,
                        frame,
                        frames,
                        policy,
                        false,
                    ));
                    return BatchWriteReport {
                        outcome: BatchWriteOutcome::DeferredCapExhausted,
                        accepted_frame_indices,
                        written_frame_count,
                    };
                }
                SendWindowOutcome::TransportClosed | SendWindowOutcome::TimedOut => {
                    accepted_frame_indices.extend(record_current_and_remaining_for_replay_indexed(
                        conn,
                        frame_index,
                        frame,
                        frames,
                        policy,
                        false,
                    ));
                    return BatchWriteReport {
                        outcome: BatchWriteOutcome::TransportClosed,
                        accepted_frame_indices,
                        written_frame_count,
                    };
                }
                SendWindowOutcome::AuthorityRevoked => {
                    accepted_frame_indices.extend(record_current_and_remaining_for_replay_indexed(
                        conn,
                        frame_index,
                        frame,
                        frames,
                        policy,
                        false,
                    ));
                    return BatchWriteReport {
                        outcome: BatchWriteOutcome::AuthorityRevoked,
                        accepted_frame_indices,
                        written_frame_count,
                    };
                }
            }
        }
        let current_was_recorded = current_should_record;
        let request_ack = if current_was_recorded {
            let (request_ack, accepted) =
                record_live_sm_frame(conn, frame_xml.clone(), SmEvictionPath::Batch);
            if accepted {
                accepted_frame_indices.push(frame_index);
            }
            request_ack
        } else {
            false
        };
        if !batch_authoritative(authority) {
            accepted_frame_indices.extend(record_current_and_remaining_for_replay_indexed(
                conn,
                frame_index,
                frame,
                frames,
                policy,
                current_was_recorded,
            ));
            return BatchWriteReport {
                outcome: BatchWriteOutcome::AuthorityRevoked,
                accepted_frame_indices,
                written_frame_count,
            };
        }
        if let Err(outcome) = send_window_message(
            sender,
            Message::Text(frame.into_serialized_xml().into()),
            "Failed to send WebSocket message",
            authority,
        )
        .await
        {
            return match outcome {
                SendWindowOutcome::TransportClosed => {
                    accepted_frame_indices
                        .extend(record_remaining_for_replay_indexed(conn, frames, policy));
                    BatchWriteReport {
                        outcome: BatchWriteOutcome::TransportClosed,
                        accepted_frame_indices,
                        written_frame_count,
                    }
                }
                SendWindowOutcome::AuthorityRevoked => {
                    // The countable current frame was recorded before the
                    // readiness race; an uncountable control is intentionally
                    // excluded from XEP-0198 replay. Preserve only the tail.
                    accepted_frame_indices
                        .extend(record_remaining_for_replay_indexed(conn, frames, policy));
                    BatchWriteReport {
                        outcome: BatchWriteOutcome::AuthorityRevoked,
                        accepted_frame_indices,
                        written_frame_count,
                    }
                }
                SendWindowOutcome::Recovered
                | SendWindowOutcome::DeferredCapExhausted
                | SendWindowOutcome::TimedOut => unreachable!("send outcome only"),
            };
        }
        written_frame_count += 1;
        if request_ack {
            if !batch_authoritative(authority) {
                accepted_frame_indices
                    .extend(record_remaining_for_replay_indexed(conn, frames, policy));
                return BatchWriteReport {
                    outcome: BatchWriteOutcome::AuthorityRevoked,
                    accepted_frame_indices,
                    written_frame_count,
                };
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
                        accepted_frame_indices
                            .extend(record_remaining_for_replay_indexed(conn, frames, policy));
                        BatchWriteReport {
                            outcome: BatchWriteOutcome::TransportClosed,
                            accepted_frame_indices,
                            written_frame_count,
                        }
                    }
                    SendWindowOutcome::AuthorityRevoked => {
                        accepted_frame_indices
                            .extend(record_remaining_for_replay_indexed(conn, frames, policy));
                        BatchWriteReport {
                            outcome: BatchWriteOutcome::AuthorityRevoked,
                            accepted_frame_indices,
                            written_frame_count,
                        }
                    }
                    SendWindowOutcome::Recovered
                    | SendWindowOutcome::DeferredCapExhausted
                    | SendWindowOutcome::TimedOut => unreachable!("send outcome only"),
                };
            }
            conn.sm_state.note_ack_request_sent();
            // Give already-arrived inbound frames a chance to land:
            // `<a/>` acks shrink the unacked queue mid-flood instead
            // of waiting for the whole batch to finish.
            match drain_ready_inbound(sender, reader, state, conn, authority).await {
                DrainSignal::Idle => {}
                DrainSignal::TransportClosed => {
                    accepted_frame_indices
                        .extend(record_remaining_for_replay_indexed(conn, frames, policy));
                    return BatchWriteReport {
                        outcome: BatchWriteOutcome::TransportClosed,
                        accepted_frame_indices,
                        written_frame_count,
                    };
                }
                DrainSignal::AuthorityRevoked => {
                    accepted_frame_indices
                        .extend(record_remaining_for_replay_indexed(conn, frames, policy));
                    return BatchWriteReport {
                        outcome: BatchWriteOutcome::AuthorityRevoked,
                        accepted_frame_indices,
                        written_frame_count,
                    };
                }
            }
        }
    }
    BatchWriteReport {
        outcome: BatchWriteOutcome::Continue,
        accepted_frame_indices: (0..total_frames).collect(),
        written_frame_count,
    }
}

/// Block until the XEP-0198 send window recovers to the low watermark,
/// applying `<a/>` acks inline and parking every other inbound frame in
/// the deferred buffer (issue #1219). One off-cadence `<r/>` is sent on
/// entry and re-sent after each ack that does not yet recover the window,
/// because the wasm client acks only in response to a request. Bounded by
/// [`SEND_WINDOW_PAUSE_DEADLINE`] and the reserved portion of
/// [`DEFERRED_INBOUND_ABSOLUTE_CEILING`]. Each recovered pause receives a
/// fresh ordinary-frame allowance, but the absolute ceiling prevents a giant
/// single batch from accumulating that allowance indefinitely. Healthy
/// low-chatter clients park few or no ordinary frames before promptly acking,
/// so they never approach the ceiling; other
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
    // Each pause earns a fresh bounded ack-search allowance. A previous
    // successful pause may have parked ordinary frames, but it also proved
    // the client completed an ack roundtrip; permanently charging those
    // frames against later pauses would turn a healthy client terminal.
    let deferred_at_pause_entry = conn.deferred_inbound.len();
    let deferred_cap_for_pause = DEFERRED_INBOUND_CAP
        .max(deferred_at_pause_entry.saturating_add(RESERVED_ACK_HEADROOM))
        .min(DEFERRED_INBOUND_ABSOLUTE_CEILING);
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
    conn.sm_state.note_ack_request_sent();
    loop {
        if !batch_authoritative(authority) {
            return SendWindowOutcome::AuthorityRevoked;
        }
        if conn.sm_state.send_window_recovered() {
            return SendWindowOutcome::Recovered;
        }
        if let Err(outcome) =
            service_paused_sm_requests_from_deferred(sender, state, conn, authority).await
        {
            return outcome;
        }
        if conn.deferred_inbound.len() >= deferred_cap_for_pause {
            return SendWindowOutcome::DeferredCapExhausted;
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
                            Message::Text(response.into_serialized_xml().into()),
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
                        conn.sm_state.note_ack_request_sent();
                    }
                } else if is_client_sm_request(text.as_str()) {
                    if !batch_authoritative(authority) {
                        return SendWindowOutcome::AuthorityRevoked;
                    }
                    if super::stream_management::flush_ingress_checkpoint(
                        state,
                        &conn.sm_state,
                        &mut conn.sm_inbound_completion,
                    )
                    .await
                    .is_err()
                    {
                        return SendWindowOutcome::TransportClosed;
                    }
                    if let Err(outcome) = send_window_message(
                        sender,
                        Message::Text(
                            SmAck::new(conn.sm_state.get_inbound_count())
                                .to_xml()
                                .into(),
                        ),
                        "Failed to answer client SM <r/> during send-window pause",
                        authority,
                    )
                    .await
                    {
                        return outcome;
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

async fn service_paused_sm_requests_from_deferred<S, E>(
    sender: &mut S,
    state: &WebSocketState,
    conn: &mut WsConnState,
    authority: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> Result<(), SendWindowOutcome>
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    // Service every parked SM request, not only one sitting at the queue
    // front: ordinary frames parked ahead of an `<r/>` must not keep it
    // charged against deferred capacity, or a near-full queue terminalizes
    // before the client's recovering `<a/>` can be read. Answering early is
    // sound — `h` counts only stanzas already handled, and the parked
    // ordinary frames ahead of the request are by definition not yet
    // handled.
    while let Some(index) = conn
        .deferred_inbound
        .iter()
        .position(|text| is_client_sm_request(text.as_str()))
    {
        conn.deferred_inbound.remove(index);
        if !batch_authoritative(authority) {
            return Err(SendWindowOutcome::AuthorityRevoked);
        }
        super::stream_management::flush_ingress_checkpoint(
            state,
            &conn.sm_state,
            &mut conn.sm_inbound_completion,
        )
        .await
        .map_err(|_| SendWindowOutcome::TransportClosed)?;
        send_window_message(
            sender,
            Message::Text(
                SmAck::new(conn.sm_state.get_inbound_count())
                    .to_xml()
                    .into(),
            ),
            "Failed to answer deferred client SM <r/> during send-window pause",
            authority,
        )
        .await?;
    }
    Ok(())
}

fn should_record(conn: &WsConnState, frame_xml: &str, policy: BatchSmPolicy) -> bool {
    conn.sm_state.enabled
        && matches!(policy, BatchSmPolicy::Record)
        && is_countable_stanza(frame_xml)
}

fn record_live_sm_frame(
    conn: &mut WsConnState,
    frame_xml: String,
    eviction_path: SmEvictionPath,
) -> (bool, bool) {
    let request_ack = conn
        .sm_state
        .record_outbound(frame_xml, eviction_path)
        .request_ack;
    // A replay gap means resume can no longer guarantee recovery of newly
    // recorded-but-unwritten frames. Keep them in recovery inventory, but do
    // not settle producer completions as if they were durably accepted.
    let accepted = conn.sm_state.replay_gap_through().is_none();
    (request_ack, accepted)
}

/// The transport died mid-batch (or a send-window pause timed out): record
/// every not-yet-written countable frame so the resume replay window covers
/// the rest of the batch. The cadence signal is moot (no wire), which mirrors
/// the detach-drain contract in `replay.rs`.
///
/// Also used by the connection loop's shutdown path for responses to
/// frames the drain had deferred before the transport went away.
///
/// Before terminal recovery this records via
/// [`StreamManagementState::record_outbound`], which marks a replay gap if a
/// post-transport-loss tail cannot fit. Once deferred headroom is exhausted,
/// it instead writes exclusively to `terminal_sm_recovery`: terminal cleanup
/// promotes the bounded recorded prefix and rejects resume, so partial
/// recording is acceptable and no later frame can evict the already-recorded
/// prefix from the capped live SM queue.
pub(super) fn record_remaining_for_replay<F>(
    conn: &mut WsConnState,
    frames: impl Iterator<Item = F>,
    policy: BatchSmPolicy,
) -> Vec<usize>
where
    F: Into<ResponseFrame>,
{
    record_remaining_for_replay_indexed(conn, frames.enumerate(), policy)
}

/// Preserve the current frame plus the unconsumed iterator tail when
/// revocation lands before the current frame was locally recorded. Once a
/// countable frame has entered the SM queue, recording it again would create
/// a duplicate sequence entry; only the iterator tail remains to be saved.
fn record_current_and_remaining_for_replay_indexed(
    conn: &mut WsConnState,
    current_index: usize,
    current: ResponseFrame,
    remaining: impl Iterator<Item = (usize, ResponseFrame)>,
    policy: BatchSmPolicy,
    current_already_recorded: bool,
) -> Vec<usize> {
    if current_already_recorded {
        record_remaining_for_replay_indexed(conn, remaining, policy)
    } else {
        record_remaining_for_replay_indexed(
            conn,
            std::iter::once((current_index, current)).chain(remaining),
            policy,
        )
    }
}

fn record_remaining_for_replay_indexed<F>(
    conn: &mut WsConnState,
    frames: impl Iterator<Item = (usize, F)>,
    policy: BatchSmPolicy,
) -> Vec<usize>
where
    F: Into<ResponseFrame>,
{
    let mut accepted_frame_indices = Vec::new();
    for (frame_index, frame) in frames {
        let frame: ResponseFrame = frame.into();
        let frame_xml = frame.into_serialized_xml();
        if should_record(conn, &frame_xml, policy) {
            let accepted = if conn.sm_recovery_required {
                let before = conn.terminal_sm_recovery.queue_len();
                conn.record_terminal_recovery_outbound(frame_xml);
                conn.terminal_sm_recovery.queue_len() > before
            } else {
                let (_, accepted) =
                    record_live_sm_frame(conn, frame_xml, SmEvictionPath::ReplayTail);
                accepted
            };
            if accepted {
                accepted_frame_indices.push(frame_index);
            }
        }
    }
    conn.warn_terminal_recovery_drops_once();
    accepted_frame_indices
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
/// to [`NORMAL_DEFERRED_INBOUND_CAP`], past which the drain stops reading so
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
        if conn.deferred_inbound.len() >= NORMAL_DEFERRED_INBOUND_CAP {
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
                            Message::Text(response.into_serialized_xml().into()),
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
                                | SendWindowOutcome::DeferredCapExhausted
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
                        | SendWindowOutcome::DeferredCapExhausted
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

fn is_client_sm_request(frame: &str) -> bool {
    SmStanza::is_client_nonza_candidate(frame)
        && matches!(SmStanza::parse(frame), Some(SmStanza::Request))
}
