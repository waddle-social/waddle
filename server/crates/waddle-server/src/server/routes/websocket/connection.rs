use super::*;
use super::{
    cleanup::{cleanup_connection_shutdown, cleanup_invalidated_detached_session},
    frame::handle_xmpp_frame,
    outbound::handle_outbound_stanza,
    send::{send_ws_message, send_ws_text_frames},
    session_init::{build_internal_server_error_stream_error, load_blocklist_for_bind},
    state::WsConnState,
    stream_management::{is_countable_stanza, sm_show_name},
};

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

                        // Register connection after successful authentication AND resource binding
                        // This ensures the JID in ConnectionRegistry matches the JID stored in MUC room occupants
                        if let Some(jid) = conn.phase.bound_jid().cloned() {
                            if let Some(tx) = pending_tx.take() {
                                // Mirror the bind transition into the
                                // per-connection state machine (#229
                                // PR11). The SM stays `None` until
                                // here so unauthenticated traffic
                                // can't reach `on_peer_stanza`. We
                                // detect SM-resume vs fresh bind from
                                // whether `pending_resume_stream_id`
                                // was just consumed below.
                                let resumed = conn.pending_resume_stream_id.is_some();
                                // Seed the SM's XEP-0191 session-state
                                // snapshot (#229 PR13).
                                //
                                // Fresh bind: load from
                                // `DatabaseBlockingStorage`. On Err we
                                // FAIL the bind via a stream-error
                                // close rather than silently
                                // initializing an empty list — a
                                // session-long fail-open would bypass
                                // XEP-0191 for the entire connection
                                // (Codex P1 / Qodo P1 review).
                                //
                                // XEP-0198 resume: the previous
                                // session was detached/dropped, but we
                                // deliberately do NOT re-read from DB.
                                // Re-reading would let blocklist
                                // mutations from other resources
                                // during the detach window leak into
                                // the resumed stream, contradicting
                                // the snapshot semantic. The resumed
                                // session starts with an empty
                                // snapshot; subsequent XEP-0191
                                // IQ-set traffic on the resumed stream
                                // re-populates it via the SM's
                                // internal blocklist mutators.
                                let blocklist = if resumed {
                                    Blocklist::empty()
                                } else {
                                    match load_blocklist_for_bind(
                                        &state.deps.app_state.db_pool,
                                        &jid,
                                    )
                                    .await
                                    {
                                        Ok(bl) => bl,
                                        Err(error) => {
                                            error!(
                                                jid = %jid,
                                                %error,
                                                "Failed to load XEP-0191 blocklist at \
                                                 bind; failing the bind to avoid a \
                                                 session-long fail-open. Client should \
                                                 reconnect."
                                            );
                                            let stream_error =
                                                build_internal_server_error_stream_error(
                                                    "Session initialization failed; \
                                                     please reconnect.",
                                                );
                                            let _ = send_ws_message(
                                                &mut ws_sender,
                                                Message::Text(stream_error),
                                                "Failed to send blocklist-load \
                                                 stream error",
                                            )
                                            .await;
                                            break;
                                        }
                                    }
                                };
                                conn.ensure_state_machine(
                                    &domain,
                                    &state.deps.protocol.dispatcher,
                                    jid.clone(),
                                    resumed,
                                    blocklist,
                                );
                                let owner = state.deps.protocol.connection_registry.register_with_stream_state(
                                    jid.clone(),
                                    tx,
                                    conn.carbons_enabled,
                                    conn.roster_interested,
                                );
                                conn.registry_owner = Some(owner.clone());
                                // Publish the SM stream id onto the freshly-registered entry
                                // so the offline-flush path keys claims by the XEP-0198
                                // session id, not the resource JID. Locked Q7b SM-ack
                                // lifecycle (issue #209). For a fresh bind without SM
                                // enabled, sm_state.stream_id is None — the flush path
                                // falls back to delete-on-push for non-SM sessions.
                                if let Some(entry) = state
                                    .deps
                                    .protocol
                                    .connection_registry
                                    .get_entry(&jid)
                                {
                                    entry.set_sm_stream_id(
                                        conn.sm_state
                                            .stream_id
                                            .clone()
                                            .map(waddle_xmpp::pending_delivery::SmSessionId::new),
                                    );
                                }
                                if conn.presence_available {
                                    state
                                        .deps
                                        .protocol
                                        .connection_registry
                                        .update_presence(&jid, true, 0);
                                    state
                                        .deps
                                        .protocol
                                        .connection_registry
                                        .update_presence_state(
                                            &jid,
                                            conn.presence_show.as_ref().map(sm_show_name).map(str::to_string),
                                            conn.presence_status.clone(),
                                            conn.presence_priority,
                                        );
                                }
                                if let Some(stream_id) = conn.pending_resume_stream_id.take() {
                                    match state
                                        .deps
                                        .protocol
                                        .sm_session_registry
                                        .complete_claim(&stream_id)
                                        .await
                                    {
                                        Ok(Some(SmClaimCompletion::Resumed(detached))) => {
                                            if let Some(h) = conn.pending_resume_h.take() {
                                                conn.sm_state.restore_from_session(&detached);
                                                conn.sm_state.acknowledge(h);
                                                responses = vec![
                                                    waddle_xmpp::stream_management::SmResumed::new(
                                                        stream_id.clone(),
                                                        conn.sm_state.get_inbound_count(),
                                                    )
                                                    .to_xml(),
                                                ];
                                                responses
                                                    .extend(conn.sm_state.get_stanzas_to_resend(h));
                                            }
                                        }
                                        Ok(Some(SmClaimCompletion::Expired(detached))) => {
                                            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim expired before completion");
                                            cleanup_invalidated_detached_session(
                                                state.as_ref(),
                                                detached,
                                                Some(&owner),
                                            )
                                            .await;
                                            let _ = state
                                                .deps
                                                .protocol
                                                .connection_registry
                                                .unregister_if_owner(&jid, &owner);
                                            conn.registry_owner = None;
                                            conn.phase = ConnectionPhase::closing(Some(jid.clone()));
                                            responses = vec![waddle_xmpp::stream_management::SmFailed::with_condition("item-not-found").to_xml()];
                                        }
                                        Ok(None) => {
                                            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim disappeared before completion");
                                            let _ = state
                                                .deps
                                                .protocol
                                                .connection_registry
                                                .unregister_if_owner(&jid, &owner);
                                            conn.registry_owner = None;
                                            conn.phase = ConnectionPhase::closing(Some(jid.clone()));
                                            responses = vec![waddle_xmpp::stream_management::SmFailed::with_condition("item-not-found").to_xml()];
                                        }
                                        Err(error) => {
                                            warn!(stream_id = %stream_id, jid = %jid, error = %error, "Failed to complete SM resume claim");
                                            let _ = state
                                                .deps
                                                .protocol
                                                .connection_registry
                                                .unregister_if_owner(&jid, &owner);
                                            conn.registry_owner = None;
                                            conn.phase = ConnectionPhase::closing(Some(jid.clone()));
                                            responses = vec![waddle_xmpp::stream_management::SmFailed::with_condition("internal-server-error").to_xml()];
                                        }
                                    }
                                    state.deps.protocol.resumable_sessions.remove(&stream_id);
                                } else if !conn.phase.is_resumed() {
                                    match state
                                        .deps
                                        .protocol
                                        .sm_session_registry
                                        .invalidate_sessions_for_jid(&jid)
                                        .await
                                    {
                                        Ok(removed) => {
                                            for detached in removed {
                                                cleanup_invalidated_detached_session(
                                                    state.as_ref(),
                                                    detached,
                                                    Some(&owner),
                                                )
                                                .await;
                                            }
                                        }
                                        Err(error) => {
                                            warn!(jid = %jid, error = %error, "Failed to invalidate older detached SM sessions for fresh bind");
                                        }
                                    }
                                }
                                info!(
                                    jid = %jid,
                                    resumed = conn.phase.is_resumed(),
                                    carbons_enabled = conn.carbons_enabled,
                                    "WebSocket connection registered"
                                );
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
                        if conn.suppress_sm_record_next_batch {
                            conn.suppress_sm_record_next_batch = false;
                        } else if conn.sm_state.enabled {
                            for frame in &responses {
                                if is_countable_stanza(frame) {
                                    conn.sm_state.record_outbound(frame.clone());
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
