use super::*;
use super::{
    batch_write::{write_response_batch, BatchWriteOutcome},
    cleanup::cleanup_connection_shutdown,
    frame::handle_xmpp_frame,
    interpret_loop::build_interpret_deps,
    outbound::handle_outbound_stanza,
    registration::{register_bound_connection_after_frame, RegistrationAfterFrame},
    replay::drive_interpret_loop,
    send::{close_ws_connection, send_ws_message, send_ws_text_frames},
    session_init::build_internal_server_error_stream_error,
    state::WsConnState,
    stream_management::SmRegistrationFinalization,
    timers::TransportTimers,
    transport_xml::{build_handled_count_too_high_stream_error, websocket_stream_close_xml},
};
use futures::stream::{SplitSink, SplitStream};

/// Create the WebSocket router
pub fn router(state: Arc<WebSocketState>) -> Router {
    Router::new()
        .route("/ws", get(xmpp_websocket_handler))
        .with_state(state)
}

/// GET /ws
///
/// WebSocket endpoint for XMPP over WebSocket (RFC 7395).
/// Upgrades HTTP connection to WebSocket and handles XMPP framing.
async fn xmpp_websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WebSocketState>>,
) -> Response {
    info!("XMPP WebSocket connection request");

    ws.protocols(["xmpp"])
        .on_upgrade(move |socket| handle_xmpp_websocket(socket, state))
}

/// Size of the outbound message channel buffer
const OUTBOUND_CHANNEL_SIZE: usize = 256;

/// Handle an XMPP WebSocket connection
async fn handle_xmpp_websocket(socket: WebSocket, state: Arc<WebSocketState>) {
    let domain = state.deps.auth_state.xmpp_domain.clone();
    info!(domain = %domain, "XMPP WebSocket connection established");

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Create outbound channel for receiving messages from other connections.
    // After the session is registered, `pending_tx` is handed to the
    // ConnectionRegistry and `None`'d out here — the registry becomes the sole
    // holder of the sender. If another session arrives for the same FullJid,
    // the registry replaces our entry, drops the sender, and our `recv()`
    // returns `None` — that's how we detect replacement and exit cleanly.
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<OutboundStanza>(OUTBOUND_CHANNEL_SIZE);
    let mut pending_tx: Option<mpsc::Sender<OutboundStanza>> = Some(outbound_tx);

    // Track connection state
    let mut conn = WsConnState::new();
    // RFC 7395 §3.8 keepalive (issue #1090): the liveness policy lives
    // in the per-connection sans-io machine, so one must exist from
    // the very first instant — a client that wedges before
    // authenticating is reaped by the same clock. `TransportReady`
    // arms the keepalive timer; the machine re-arms it on every tick.
    conn.init_prebind_state_machine(
        &domain,
        &state.deps.protocol.dispatcher,
        state.deps.ws_keepalive,
    );
    let mut timers = TransportTimers::new();
    if let Some(sm) = conn.state_machine.as_mut() {
        let events = sm.handle(InboundEvent::TransportReady);
        let interpret_deps = build_interpret_deps(state.as_ref(), None);
        let drive = drive_interpret_loop(events, sm, &interpret_deps).await;
        timers.apply(drive.timer_commands);
    }
    // Set when our own registry slot was replaced by a newer connection for
    // the same FullJid (detected via outbound_rx closing). In that case the
    // cleanup block below must NOT touch the registry or MUC state — those
    // belong to the newcomer now.
    let mut superseded = false;

    loop {
        // Frames the mid-batch ack drain (issue #1089) pulled off the
        // socket ahead of the dispatcher. They must be processed in
        // arrival order BEFORE the socket is polled again, or a frame
        // the client sent mid-flood would be reordered behind frames
        // it sent afterwards.
        if let Some(text) = conn.deferred_inbound.pop_front() {
            if !handle_inbound_text(
                &text,
                &domain,
                &state,
                &mut conn,
                &mut pending_tx,
                &mut ws_sender,
                &mut ws_receiver,
            )
            .await
            {
                break;
            }
            continue;
        }
        tokio::select! {
            // Handle inbound WebSocket messages from the client
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if !handle_inbound_text(
                            text.as_str(),
                            &domain,
                            &state,
                            &mut conn,
                            &mut pending_tx,
                            &mut ws_sender,
                            &mut ws_receiver,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        conn.note_transport_activity();
                        warn!("Received binary WebSocket message (not supported for XMPP)");
                    }
                    Some(Ok(Message::Ping(data))) => {
                        conn.note_transport_activity();
                        if !send_ws_message(&mut ws_sender, Message::Pong(data), "Failed to send pong")
                            .await
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // Liveness evidence for the keepalive policy —
                        // any payload counts, no probe correlation
                        // (issue #1090 "any inbound frame = alive").
                        conn.note_transport_activity();
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket close requested");
                        break;
                    }
                    Some(Err(e)) => {
                        error!(error = %e, "WebSocket error");
                        break;
                    }
                    None => {
                        // Stream ended
                        debug!("WebSocket stream ended");
                        break;
                    }
                }
            }

            // Handle outbound messages routed from other connections
            outbound = outbound_rx.recv() => {
                match outbound {
                    Some(outbound_stanza) => {
                        if !handle_outbound_stanza(
                            &mut ws_sender,
                            &mut ws_receiver,
                            &state,
                            &mut conn,
                            &mut timers,
                            outbound_stanza,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    None => {
                        // Outbound channel closed. All clones of the sender (our
                        // own outbound_tx + any copy held by the registry) have
                        // been dropped. The only path to this state after
                        // registration is a replacement register for the same
                        // FullJid: the registry drops our entry (and with it
                        // the sender) to install the new session's sender.
                        // Mark as superseded so the cleanup block skips
                        // unregister/MUC-cleanup/detach — all of those would
                        // target the newcomer's registry slot and occupant.
                        info!("Outbound channel closed; session superseded by replacement");
                        superseded = true;
                        break;
                    }
                }
            }

            // RFC 7395 §3.8 keepalive clock (issue #1090). Fires only
            // when the state machine armed a timer; the tick is fed
            // back into the machine, whose policy decides: quiet
            // re-arm, probe + re-arm, or close a dead peer. A
            // keepalive close breaks the loop with a graceful WS close
            // handshake and rides the normal shutdown fork below — SM
            // detach-for-resume when negotiated, full cleanup
            // otherwise.
            timer_id = timers.next_expired() => {
                let Some(sm) = conn.state_machine.as_mut() else {
                    warn!(timer_id = timer_id.0, "Timer fired without a state machine; disarming");
                    continue;
                };
                let events = sm.handle(InboundEvent::Tick(timer_id));
                let interpret_deps =
                    build_interpret_deps(state.as_ref(), conn.authenticated_session.as_ref());
                let drive = drive_interpret_loop(events, sm, &interpret_deps).await;
                timers.apply(drive.timer_commands);
                if !drive.frames.is_empty() {
                    // No tick pathway emits stanzas today; a frame here
                    // means a new timer consumer forgot to extend this
                    // arm with the SM-recording write contract.
                    warn!(
                        frames = drive.frames.len(),
                        "Timer tick produced wire frames; dropping"
                    );
                }
                let mut ping_send_failed = false;
                for _ in 0..drive.keepalive_probes {
                    if !send_ws_message(
                        &mut ws_sender,
                        Message::Ping(axum::body::Bytes::new()),
                        "Failed to send keepalive ping",
                    )
                    .await
                    {
                        ping_send_failed = true;
                        break;
                    }
                }
                if ping_send_failed {
                    break;
                }
                if drive.close {
                    // The policy's Log event (relayed above via interpret)
                    // carries the reason (miss limit vs negotiation
                    // deadline); this line adds the connection identity
                    // for correlation with gateway/Loki reset queries.
                    info!(
                        jid = ?conn.phase.bound_jid(),
                        "Keepalive policy closed the connection"
                    );
                    let _ = close_ws_connection(
                        &mut ws_sender,
                        "Failed to send WebSocket close frame after keepalive close",
                    )
                    .await;
                    break;
                }
            }
        }
    }

    // Connection is ending. Decide between two paths:
    //   A. Fully clean up (unregister + remove MUC occupants) — the default
    //      for non-SM sessions and for SM sessions that didn't negotiate
    //      resume.
    //   B. Detach for resumption — for SM sessions with `resume='true'`,
    //      stash state into the SmSessionRegistry so a reconnecting client
    //      can `<resume/>` without re-joining MUC or re-authenticating.
    //      MUC occupants stay in place during the detach window so other
    //      users continue to see this user as present.
    //
    // Short-circuit when this task was superseded: the registry and MUC
    // occupant slots now belong to the newer connection for this FullJid,
    // and any cleanup we do here would clobber the newcomer.
    cleanup_connection_shutdown(state.as_ref(), &mut outbound_rx, &mut conn, superseded).await;

    info!("XMPP WebSocket connection closed");
}

/// Handle one inbound XMPP text frame end to end: framing, phase
/// mirroring, post-frame registration, and the chunked XEP-0198-aware
/// response write. Extracted from the connection loop's Text arm so
/// frames deferred by the mid-batch ack drain (issue #1089) go
/// through the exact same path as frames read off the socket.
///
/// Returns `false` when the connection loop must break.
async fn handle_inbound_text(
    text: &str,
    domain: &str,
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    pending_tx: &mut Option<mpsc::Sender<OutboundStanza>>,
    ws_sender: &mut SplitSink<WebSocket, Message>,
    ws_receiver: &mut SplitStream<WebSocket>,
) -> bool {
    debug!(len = text.len(), "Received XMPP WebSocket message");
    // Any inbound frame is liveness evidence for the
    // RFC 7395 §3.8 keepalive policy (issue #1090).
    conn.note_transport_activity();

    // Handle XMPP framing (RFC 7395)
    let mut responses = handle_xmpp_frame(text, domain, state.as_ref(), conn).await;

    // Mirror any phase transition `handle_xmpp_frame` performed (most
    // importantly Ready → Closing on SASL failure / stream error)
    // into the per-connection state machine. Without this, late
    // `PeerStanza` dispatches from the outbound channel would still
    // go through the recipient pipeline even though the legacy phase
    // tracker has marked the connection Closing.
    conn.sync_state_machine_phase();

    // Register the connection after successful authentication and
    // resource binding. This keeps the transport loop focused on
    // WebSocket I/O while the registration module owns registry
    // publication and post-registration SM finalization.
    match register_bound_connection_after_frame(state.as_ref(), domain, conn, pending_tx).await {
        RegistrationAfterFrame::Unchanged => {}
        RegistrationAfterFrame::SessionInitializationFailed => {
            let stream_error = build_internal_server_error_stream_error(
                "Session initialization failed; please reconnect.",
            );
            let _ = send_ws_text_frames(
                ws_sender,
                [stream_error, websocket_stream_close_xml()],
                "Failed to send session-init stream error",
            )
            .await;
            let _ = close_ws_connection(
                ws_sender,
                "Failed to send WebSocket close frame after session-init error",
            )
            .await;
            return false;
        }
        RegistrationAfterFrame::Registered(sm_finalization) => match sm_finalization {
            SmRegistrationFinalization::KeepExistingResponses => {}
            SmRegistrationFinalization::ReplaceWithResumed {
                resumed,
                replay_after_h,
            } => {
                responses = vec![resumed.to_xml()];
                responses.extend(conn.sm_state.get_stanzas_to_resend(replay_after_h));
            }
            SmRegistrationFinalization::ReplaceWithFailed(failed) => {
                responses = vec![failed.to_xml()];
            }
            SmRegistrationFinalization::ReplaceWithHandledCountTooHigh {
                acknowledged,
                send_count,
            } => {
                responses = vec![
                    build_handled_count_too_high_stream_error(acknowledged, send_count),
                    websocket_stream_close_xml(),
                ];
            }
        },
    }

    ensure_websocket_stream_close_for_closing_phase(conn, &mut responses);

    // Write the batch through the chunked XEP-0198-aware writer
    // (issue #1089): each countable stanza is recorded just before
    // its own write, an `<r/>` follows every `ack_threshold`th one,
    // and already-arrived inbound frames are drained after each `<r/>`
    // so `<a/>` acks shrink the unacked queue mid-flood.
    //
    // Exception: when `handle_sm_resume` just ran, the responses ARE
    // the replay of the restored unacked queue — those stanzas
    // already have their original sequence numbers and are still in
    // the queue. Re-recording them would bump `outbound_count` past
    // reality and push duplicate queue entries, breaking subsequent
    // acks and a second resume.
    let record = if conn.suppress_sm_record_next_batch {
        conn.suppress_sm_record_next_batch = false;
        false
    } else {
        true
    };
    match write_response_batch(
        ws_sender,
        ws_receiver,
        state.as_ref(),
        conn,
        responses,
        record,
    )
    .await
    {
        BatchWriteOutcome::Continue => {}
        BatchWriteOutcome::TransportClosed => return false,
    }

    if matches!(conn.phase, ConnectionPhase::Closing { .. }) {
        let _ = close_ws_connection(
            ws_sender,
            "Failed to send WebSocket close frame after XMPP stream close",
        )
        .await;
        return false;
    }
    true
}

fn ensure_websocket_stream_close_for_closing_phase(
    conn: &WsConnState,
    responses: &mut Vec<String>,
) {
    if !matches!(conn.phase, ConnectionPhase::Closing { .. })
        || response_batch_ends_with_websocket_stream_close(responses)
    {
        return;
    }

    responses.push(websocket_stream_close_xml());
}

fn response_batch_ends_with_websocket_stream_close(responses: &[String]) -> bool {
    let websocket_close = websocket_stream_close_xml();
    responses
        .last()
        .is_some_and(|frame| frame == &websocket_close)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn closing_phase_appends_websocket_stream_close_frame() {
        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::closing(None);
        let mut responses = vec![element_to_xml(
            Element::builder("failed", waddle_xmpp::stream_management::SM_NS).build(),
        )];

        ensure_websocket_stream_close_for_closing_phase(&conn, &mut responses);

        assert_eq!(responses.len(), 2);
        let close = Element::from_str(&responses[1]).expect("close frame xml");
        assert_eq!(close.name(), "close");
        assert_eq!(close.ns(), "urn:ietf:params:xml:ns:xmpp-framing");
    }

    #[test]
    fn closing_phase_does_not_duplicate_websocket_stream_close_frame() {
        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::closing(None);
        let mut responses = vec![websocket_stream_close_xml()];

        ensure_websocket_stream_close_for_closing_phase(&conn, &mut responses);

        assert_eq!(responses.len(), 1);
    }
}
