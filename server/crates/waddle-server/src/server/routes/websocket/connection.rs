use super::*;
use super::{
    cleanup::cleanup_connection_shutdown,
    frame::handle_xmpp_frame,
    outbound::handle_outbound_stanza,
    registration::{register_bound_connection_after_frame, RegistrationAfterFrame},
    send::{send_ws_message, send_ws_text_frames},
    session_init::build_internal_server_error_stream_error,
    state::WsConnState,
    stream_management::{is_countable_stanza, SmRegistrationFinalization},
};
use waddle_xmpp::stream_management::SmRequest;

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
    // Set when our own registry slot was replaced by a newer connection for
    // the same FullJid (detected via outbound_rx closing). In that case the
    // cleanup block below must NOT touch the registry or MUC state — those
    // belong to the newcomer now.
    let mut superseded = false;

    loop {
        tokio::select! {
            // Handle inbound WebSocket messages from the client
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        debug!(len = text.len(), "Received XMPP WebSocket message");

                        // Handle XMPP framing (RFC 7395)
                        let mut responses = handle_xmpp_frame(
                            &text,
                            &domain,
                            &state,
                            &mut conn,
                        ).await;

                        // Mirror any phase transition `handle_xmpp_frame`
                        // performed (most importantly Ready → Closing on
                        // SASL failure / stream error) into the per-
                        // connection state machine. Without this, late
                        // `PeerStanza` dispatches from the outbound
                        // channel would still go through the recipient
                        // pipeline even though the legacy phase tracker
                        // has marked the connection Closing.
                        conn.sync_state_machine_phase();

                        // Register the connection after successful authentication
                        // and resource binding. This keeps the transport loop
                        // focused on WebSocket I/O while the registration module
                        // owns registry publication and post-registration SM
                        // finalization.
                        match register_bound_connection_after_frame(
                            state.as_ref(),
                            &domain,
                            &mut conn,
                            &mut pending_tx,
                        )
                        .await
                        {
                            RegistrationAfterFrame::Unchanged => {}
                            RegistrationAfterFrame::SessionInitializationFailed => {
                                let stream_error = build_internal_server_error_stream_error(
                                    "Session initialization failed; please reconnect.",
                                );
                                let _ = send_ws_message(
                                    &mut ws_sender,
                                    Message::Text(stream_error),
                                    "Failed to send blocklist-load stream error",
                                )
                                .await;
                                break;
                            }
                            RegistrationAfterFrame::Registered(sm_finalization) => {
                                match sm_finalization {
                                    SmRegistrationFinalization::KeepExistingResponses => {}
                                    SmRegistrationFinalization::ReplaceWithResumed {
                                        resumed,
                                        replay_after_h,
                                    } => {
                                        responses = vec![resumed.to_xml()];
                                        responses.extend(
                                            conn.sm_state.get_stanzas_to_resend(replay_after_h),
                                        );
                                    }
                                    SmRegistrationFinalization::ReplaceWithFailed(failed) => {
                                        responses = vec![failed.to_xml()];
                                    }
                                }
                            }
                        }

                        // Record outbound stanzas for XEP-0198 replay BEFORE
                        // writing them to the socket. If SM is enabled and
                        // the stanza is countable, push it into the unacked
                        // queue; a future resume will replay this exact XML.
                        //
                        // Exception: when `handle_sm_resume` just ran, the
                        // responses ARE the replay of the restored unacked
                        // queue — those stanzas already have their original
                        // sequence numbers and are still in the queue.
                        // Re-recording them would bump `outbound_count` past
                        // reality and push duplicate queue entries, breaking
                        // subsequent acks and a second resume.
                        let mut request_ack_after = false;
                        if conn.suppress_sm_record_next_batch {
                            conn.suppress_sm_record_next_batch = false;
                        } else if conn.sm_state.enabled {
                            for frame in &responses {
                                if is_countable_stanza(frame) {
                                    let result = conn.sm_state.record_outbound(frame.clone());
                                    request_ack_after |= result.request_ack;
                                }
                            }
                        }

                        if !send_ws_text_frames(
                            &mut ws_sender,
                            responses,
                            "Failed to send WebSocket message",
                        )
                        .await
                        {
                            break;
                        }
                        // SM cadence (XEP-0198 §4): once the unacked
                        // outbound stanza count since the last `<r/>`
                        // hits the threshold, follow the batch with an
                        // `<r/>` so the wasm client sends back `<a
                        // h='N'/>`. Without this, the unacked queue
                        // grows monotonically until the 1000-cap
                        // triggers eviction and resume is permanently
                        // broken for the stream.
                        if request_ack_after
                            && !send_ws_message(
                                &mut ws_sender,
                                Message::Text(SmRequest::to_xml()),
                                "Failed to send SM <r/> request",
                            )
                            .await
                        {
                            break;
                        }

                        if matches!(conn.phase, ConnectionPhase::Closing { .. }) {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        warn!("Received binary WebSocket message (not supported for XMPP)");
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if !send_ws_message(&mut ws_sender, Message::Pong(data), "Failed to send pong")
                            .await
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // Ignore pongs
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
                            &state,
                            &mut conn,
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
