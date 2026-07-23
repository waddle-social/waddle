use super::*;
use super::{
    batch_write::{write_response_batch, BatchSmPolicy, BatchWriteOutcome},
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
    transport_xml::{
        build_conflict_stream_error, build_handled_count_too_high_stream_error,
        build_system_shutdown_stream_error, websocket_stream_close_xml,
    },
};
use axum::response::IntoResponse;
use futures::stream::{SplitSink, SplitStream};
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
    uri: axum::http::Uri,
    State(state): State<Arc<WebSocketState>>,
) -> Response {
    // Count room-capable admission before either shutdown check. The room gate
    // and Ecdysis guard then cover the same upgrade race: shutdown either
    // observes this accepted connection or this request is rejected.
    let mut room_scope = match state.deps.room_serving.try_scope() {
        Ok(scope) => scope,
        Err(error) => {
            info!(
                ?error,
                "Rejecting XMPP WebSocket upgrade: room serving is closed"
            );
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };

    // Graceful shutdown gate (issue #1091). The guard is minted BEFORE
    // the stop-token check: the upgrade handshake spans a client
    // round-trip, so a check-then-mint order would let a connection be
    // admitted after `wait_for_connections_drained` already observed
    // zero guards and the Q6 drain concluded — reintroducing the very
    // unpromoted-queue gap this issue closes. Mint-then-check means
    // every request either sees the cancelled token (503, guard drops)
    // or is counted by the drain.
    let connection_guard = state.deps.shutdown.connection_guard();
    if state.deps.shutdown.stop_token().is_cancelled() {
        info!("Rejecting XMPP WebSocket upgrade: server is draining");
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    info!("XMPP WebSocket connection request");

    // #1326 phase A: the browser cannot set headers on a WebSocket
    // upgrade, so the client's connect trace arrives as a
    // `traceparent` query parameter. Emit a short-lived
    // `xmpp.connection.established` span carrying an OTel span LINK to
    // the client's context plus `client.trace_id`, so the browser's
    // `xmpp.connect` span and this server session join in Tempo.
    //
    // Deliberately NOT instrumenting the whole connection future: a
    // span wrapping a connection that lives for hours would only
    // export at close and would pull every `xmpp.stanza.dispatch`
    // span on the session into one giant trace (Tempo truncates
    // those). Dispatch spans stay independent roots, correlated via
    // their `xmpp.resource`/`user` attributes instead.
    if let Some(established_span) = connection_established_span(uri.query()) {
        let _enter = established_span.enter();
    }

    ws.protocols(["xmpp"]).on_upgrade(move |socket| async move {
        // The scope stays unarmed while Axum is negotiating the upgrade. If
        // the upgrade is never established, its drop is a clean non-operation.
        // Once established, cancellation/panic anywhere through connection
        // cleanup makes terminal release unsafe.
        room_scope.arm();
        handle_xmpp_websocket(socket, state, connection_guard).await;
        room_scope.complete_clean();
    })
}

fn connection_established_span(query: Option<&str>) -> Option<tracing::Span> {
    let client_parent = crate::server::trace::client_trace_parent_from_query(query)?;
    let established_span = tracing::info_span!(
        "xmpp.connection.established",
        client.trace_id = %client_parent.trace_id(),
    );
    tracing_opentelemetry::OpenTelemetrySpanExt::add_link(
        &established_span,
        client_parent.remote_span_context(),
    );
    Some(established_span)
}

/// Size of the outbound message channel buffer
const OUTBOUND_CHANNEL_SIZE: usize = 256;

/// Deadline for a loop-level XEP-0198 send-window pause (issue #1219).
/// While `sm_state.needs_send_pause()` latches, the connection loop stops
/// draining the outbound mpsc so its producers backpressure; client acks
/// keep flowing via `ws_receiver` and normally release the pause within an
/// RTT. If none arrives within this window the peer is dead — the loop
/// breaks into the same detach-for-resume path a keepalive close uses.
/// Matches the batch writer's inline pause deadline.
const SEND_WINDOW_LOOP_PAUSE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// Budget for writing the system-shutdown stream error + close frames
/// to one peer during graceful shutdown (issue #1091). Deliberately
/// much shorter than the generic 60s `SEND_STALL_TIMEOUT`: a peer that
/// has stopped reading must not hold its connection guard past the
/// drain window — the notification is best-effort, while the SM detach
/// that follows the break is what actually preserves the session.
const SHUTDOWN_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Aggregate grace period for ordered-relay completions that precede a
/// terminal XEP-0198 handled-count hole. Those tasks have already spent the
/// later stanza's 15-second wedge budget running, so this window primarily
/// closes the scheduler/channel race at detach. It is deliberately aggregate:
/// an unbounded relay backlog or a panicked spawned task must not strand
/// connection/shutdown cleanup forever.
const ORDERED_RELAY_HANDOFF_CLEANUP_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(2);
const ORDERED_RELAY_HANDOFF_CLEANUP_MAX_COMPLETIONS: usize = 1_024;

/// The two channel halves handed over to the `ConnectionRegistry` at
/// registration time (`register_bound_connection_after_frame`, then
/// ADR-0017 Phase 3 Slice 6's force-detach receiver take-over). Bundled so
/// `handle_inbound_text` stays under the clippy too-many-arguments
/// threshold, mirroring `SmCtx`'s identical rationale one file over.
struct RegistrationChannels<'a> {
    pending_tx: &'a mut Option<mpsc::Sender<OutboundStanza>>,
    force_detach_rx: &'a mut Option<mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>>,
}

/// Poll `rx` if present, otherwise never resolve (ADR-0017 Phase 3 Slice 6).
/// Lets the connection loop's `select!` carry an `Option<mpsc::Receiver<_>>`
/// arm — `None` before this connection is registered (the force-detach
/// receiver is only handed over post-registration, see
/// `handle_inbound_text`) — without an `if`-guard/`.unwrap()` pairing: the
/// arm simply never fires while `rx` is `None`, exactly like a genuinely
/// empty, never-closing channel would.
async fn recv_optional<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// Handle an XMPP WebSocket connection
async fn handle_xmpp_websocket(
    socket: WebSocket,
    state: Arc<WebSocketState>,
    // Minted in the upgrade handler and held for the connection's whole
    // lifetime — including the detach/cleanup below — so the ecdysis
    // drain (issue #1091) only completes once every live session has
    // been closed AND its SM state handed to the session registry for
    // Q6 promotion.
    _connection_guard: waddle_ecdysis::ConnectionGuard,
) {
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let shutdown_token = state.deps.shutdown.stop_token();
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

    // ADR-0017 Phase 3 Slice 6: the cross-node resume live-steal handshake's
    // force-detach receiver. `None` until this connection registers — the
    // registry mints a fresh channel pair per `ConnectionEntry`
    // (`ConnectionEntry::new`), and `handle_inbound_text` takes the receiver
    // half via `entry.take_force_detach_rx()` immediately after
    // registration succeeds, mirroring `pending_tx`'s own hand-off pattern.
    let mut force_detach_rx: Option<mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>> =
        None;
    // Set when this connection is asked (identity-matched) to force-detach
    // for a cross-node resume. The ack is deliberately sent only AFTER
    // `cleanup_connection_shutdown` below has run this connection's normal
    // XEP-0198 detach-for-resume persistence — the asking node's
    // `steal_for_resume` must never proceed until the detach-flush this
    // node performs has actually landed.
    let mut pending_force_detach_ack: Option<
        tokio::sync::oneshot::Sender<waddle_xmpp::registry::ForceDetachOutcome>,
    > = None;

    // Track connection state
    let mut conn = WsConnState::new();
    let (handoff_tx, mut handoff_rx) = mpsc::unbounded_channel::<
        crate::server::routes::interpret::OrderedRelayHandoffCompletion,
    >();
    conn.ordered_relay_handoff_tx = Some(handoff_tx);
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
        // Loop-level XEP-0198 send-window gate (issue #1219). When the
        // outstanding unacked count has latched the pause, stop draining the
        // outbound mpsc (its `recv()` arm is guarded below): producers then
        // backpressure on the 256-slot channel instead of piling more into
        // the SM unacked queue, while `ws_receiver` keeps delivering the
        // `<a/>` acks that shrink the window. On the rising edge send one
        // forced `<r/>` (the wasm client acks only when asked) and arm a
        // deadline; on recovery, disarm. No await lives inside a select arm
        // for this — acks flow freely and the gate is deadlock-free by
        // construction.
        let send_window_paused = conn.sm_state.needs_send_pause();
        if send_window_paused {
            let rising_edge = conn.send_window_pause_deadline.is_none();
            if rising_edge {
                conn.send_window_pause_deadline =
                    Some(tokio::time::Instant::now() + SEND_WINDOW_LOOP_PAUSE_DEADLINE);
                waddle_xmpp::telemetry::reliability::increment_sm_send_window_pause();
            }
            // Prompt an ack on the rising edge, and AGAIN whenever the client
            // has acked since our last prompt but not yet recovered the
            // window. The wasm client acks only when asked, so without the
            // re-request a partial ack (XEP-0198 §5 `h` = handled ≤ received)
            // would strand the stream in the hysteresis band until the
            // deadline (mirrors the batch writer's inline re-request). The
            // deadline stays armed from the rising edge as the true dead-peer
            // bound, so a genuinely stuck client is still detached in time.
            if rising_edge || conn.sm_state.last_acked != conn.send_window_last_request_acked {
                conn.send_window_last_request_acked = conn.sm_state.last_acked;
                if !send_ws_message(
                    &mut ws_sender,
                    Message::Text(SmRequest::to_xml().into()),
                    "Failed to send SM <r/> at send-window loop pause",
                )
                .await
                {
                    break;
                }
            }
        } else {
            conn.send_window_pause_deadline = None;
        }
        let send_window_pause_deadline = conn.send_window_pause_deadline;

        // Frames the mid-batch ack drain (issue #1089) pulled off the
        // socket ahead of the dispatcher must be processed in arrival
        // order BEFORE the socket is polled again — so the socket arm
        // below is gated on the queue being empty, and an
        // always-ready arm processes one deferred frame per
        // iteration. Deferred processing stays a select arm (not a
        // pre-select loop) so the outbound channel and the keepalive
        // timer keep getting polled between deferred frames — a
        // client streaming frames mid-flood must not be able to
        // starve routed stanzas or dead-peer detection.
        let deferred_pending = !conn.deferred_inbound.is_empty();
        tokio::select! {
            // Process one drain-deferred inbound frame.
            _ = std::future::ready(()), if deferred_pending => {
                let Some(text) = conn.deferred_inbound.pop_front() else {
                    continue;
                };
                if !handle_inbound_text(
                    &text,
                    &domain,
                    &state,
                    &mut conn,
                    RegistrationChannels {
                        pending_tx: &mut pending_tx,
                        force_detach_rx: &mut force_detach_rx,
                    },
                    &mut ws_sender,
                    &mut ws_receiver,
                )
                .await
                {
                    break;
                }
            }

            // Handle inbound WebSocket messages from the client
            msg = ws_receiver.next(), if !deferred_pending => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if !handle_inbound_text(
                            text.as_str(),
                            &domain,
                            &state,
                            &mut conn,
                            RegistrationChannels {
                                pending_tx: &mut pending_tx,
                                force_detach_rx: &mut force_detach_rx,
                            },
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

            // Handle outbound messages routed from other connections.
            // Gated by the send-window pause (issue #1219): while paused we
            // do not pull new outbound work, so its producers backpressure on
            // the mpsc rather than overflowing the SM unacked queue. The
            // `ws_receiver` arm above stays active, so client acks keep
            // arriving and releasing the pause.
            outbound = outbound_rx.recv(), if !send_window_paused => {
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

            handoff = handoff_rx.recv() => {
                match handoff {
                    Some(handoff) => {
                        if !handle_ordered_relay_handoff_completion(
                            &mut ws_sender,
                            &mut ws_receiver,
                            &state,
                            &mut conn,
                            handoff,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    None => {
                        warn!("Ordered relay handoff completion channel closed");
                        break;
                    }
                }
            }

            // ADR-0017 Phase 3 Slice 6: a cross-node XEP-0198 resume
            // live-steal handshake ask for this connection's own stream id.
            // The identity check gates the destructive close itself (defense
            // in depth against a wrong-identity `previd` forcing a disconnect
            // before rejection) — a mismatch answers inline and this
            // connection keeps serving normally; a match sends `<conflict/>`
            // (XEP-0198 "Resumption" SHOULD) and closes, falling through to
            // the SAME detach-for-resume cleanup a graceful/keepalive close
            // uses (never transitions `phase` to `Closing`).
            request = recv_optional(&mut force_detach_rx) => {
                match request {
                    Some(request) => {
                        let bound_bare = conn.phase.bound_jid().map(|jid| jid.to_bare());
                        if bound_bare.as_ref() != Some(&request.requester_bare_jid) {
                            warn!(
                                requester = %request.requester_bare_jid,
                                bound = ?bound_bare,
                                "Cross-node resume force-detach rejected: identity mismatch"
                            );
                            let _ = request
                                .ack
                                .send(waddle_xmpp::registry::ForceDetachOutcome::IdentityMismatch);
                        } else {
                            info!(
                                jid = ?conn.phase.bound_jid(),
                                "Cross-node resume: force-detaching this session (<conflict/> close)"
                            );
                            if conn.stream_open_sent {
                                let _ = send_ws_text_frames(
                                    &mut ws_sender,
                                    [build_conflict_stream_error(), websocket_stream_close_xml()],
                                    "Failed to send conflict stream error",
                                )
                                .await;
                            }
                            let _ = close_ws_connection(
                                &mut ws_sender,
                                "Failed to send WebSocket close frame after conflict",
                            )
                            .await;
                            // Deferred: acked only after this connection's own
                            // detach-for-resume cleanup below actually runs.
                            pending_force_detach_ack = Some(request.ack);
                            break;
                        }
                    }
                    None => {
                        // The registry entry was removed (e.g. superseded)
                        // without this channel ever being used — disable
                        // this arm for the remainder of the loop.
                        force_detach_rx = None;
                    }
                }
            }

            // Graceful shutdown (issue #1091): SIGTERM cancelled the
            // ecdysis stop token. Close this live session natively —
            // RFC 6120 §4.9.3.20 <system-shutdown/> stream error, then
            // the RFC 7395 <close/> frame and the WS close handshake —
            // and break WITHOUT transitioning to Closing: the cleanup
            // fork below must still see a resumable phase so an
            // XEP-0198 session detaches into the SmSessionRegistry,
            // where the shutdown drain promotes its unacked queue (Q6).
            //
            // Stream frames are sent only after the server has answered
            // the client's <open/>: RFC 6120 §4.9.1.2 forbids a stream
            // error before the response stream header, so a connection
            // still in the upgrade-to-open gap gets the WS closing
            // handshake alone.
            _ = shutdown_token.cancelled() => {
                info!(
                    jid = ?conn.phase.bound_jid(),
                    "Graceful shutdown: closing live session with system-shutdown stream error"
                );
                let close_peer = async {
                    if conn.stream_open_sent {
                        let _ = send_ws_text_frames(
                            &mut ws_sender,
                            [
                                build_system_shutdown_stream_error(),
                                websocket_stream_close_xml(),
                            ],
                            "Failed to send system-shutdown stream error",
                        )
                        .await;
                    }
                    let _ = close_ws_connection(
                        &mut ws_sender,
                        "Failed to send WebSocket close frame after system-shutdown",
                    )
                    .await;
                };
                if tokio::time::timeout(SHUTDOWN_CLOSE_TIMEOUT, close_peer)
                    .await
                    .is_err()
                {
                    warn!(
                        jid = ?conn.phase.bound_jid(),
                        timeout_secs = SHUTDOWN_CLOSE_TIMEOUT.as_secs(),
                        "Graceful shutdown: peer did not accept the close frames in time; \
                         proceeding to detach without them"
                    );
                }
                break;
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

            // Loop-level send-window pause deadline (issue #1219). Fires
            // only while paused; if the client never acks the window down in
            // time it is dead, so break WITHOUT transitioning to Closing —
            // the cleanup fork below detaches an SM session for resume, and
            // the retained queue is bounded (pacing kept it ≤ cap) so that
            // resume stays clean.
            _ = async {
                match send_window_pause_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            }, if send_window_paused => {
                waddle_xmpp::telemetry::reliability::increment_sm_send_window_pause_timeout();
                warn!(
                    jid = ?conn.phase.bound_jid(),
                    deadline_secs = SEND_WINDOW_LOOP_PAUSE_DEADLINE.as_secs(),
                    "SM send-window loop pause timed out with no recovering ack; \
                     closing into detach-for-resume"
                );
                break;
            }
        }
    }

    // Frames the mid-batch ack drain had already pulled off the
    // socket when the transport went away (issue #1089). The old loop
    // would have processed them before ever seeing the close, so
    // their inbound side effects (routing to peers, inbound_count)
    // must still happen. There is no wire to answer on: responses to
    // the departed client are recorded for XEP-0198 resume replay
    // instead of being written.
    //
    // Skipped when superseded: the registry slot, MUC occupancy, and
    // SM continuity now belong to the replacement session, and
    // running handlers from the stale session here could still emit
    // side effects (routing, inbound_count) against state the
    // newcomer owns — the same reason the cleanup block below
    // short-circuits.
    if !superseded {
        if let (Some(jid), Some(owner)) = (conn.phase.bound_jid(), conn.registry_owner.as_ref()) {
            superseded = state
                .deps
                .protocol
                .connection_registry
                .entry_if_owner(jid, owner)
                .is_none();
        }
    }
    if superseded {
        super::stream_management::defer_superseded_sm_claim(state.as_ref(), &conn.sm_state);
    } else {
        process_deferred_inbound_after_transport_loss(&domain, state.as_ref(), &mut conn).await;
        drain_ordered_relay_handoffs_before_cleanup(&mut handoff_rx, &mut conn).await;
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
    let shutdown_outcome =
        cleanup_connection_shutdown(state.as_ref(), &mut outbound_rx, &mut conn, superseded).await;

    // ADR-0017 Phase 3 Slice 6: only now — after this connection's own
    // detach-for-resume persistence has actually run above — tell the
    // cross-node resume asker it is safe to proceed with `steal_for_resume`.
    // Council-adjudicated FIX 4: ack only what actually happened — map the
    // typed `shutdown_outcome` onto the wire outcome rather than always
    // claiming `Detached`, so the asker never proceeds with
    // `steal_for_resume` against a snapshot that was never persisted (a
    // storage-error fallback, an ownership-race promotion, or any other
    // non-detach cleanup path).
    if let Some(ack) = pending_force_detach_ack.take() {
        let outcome = match shutdown_outcome {
            cleanup::ConnectionShutdownOutcome::Detached => {
                waddle_xmpp::registry::ForceDetachOutcome::Detached
            }
            cleanup::ConnectionShutdownOutcome::NotPersisted => {
                waddle_xmpp::registry::ForceDetachOutcome::NotPersisted
            }
        };
        let _ = ack.send(outcome);
    }

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
    channels: RegistrationChannels<'_>,
    ws_sender: &mut SplitSink<WebSocket, Message>,
    ws_receiver: &mut SplitStream<WebSocket>,
) -> bool {
    let RegistrationChannels {
        pending_tx,
        force_detach_rx,
    } = channels;
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
        RegistrationAfterFrame::Registered(sm_finalization) => {
            // ADR-0017 Phase 3 Slice 6: wire this connection's own
            // force-detach receiver into the main loop's select! now that
            // registration published the `ConnectionEntry` this channel
            // pair lives on. `take_force_detach_rx` returns `Some` exactly
            // once per entry, so a racing/duplicate registration attempt
            // for the same entry observes `None` here, same as intended.
            if let Some(jid) = conn.phase.bound_jid() {
                if let Some(entry) = state.deps.protocol.connection_registry.get_entry(jid) {
                    if let Some(rx) = entry.take_force_detach_rx() {
                        *force_detach_rx = Some(rx);
                    }
                }
            }
            match sm_finalization {
                SmRegistrationFinalization::KeepExistingResponses => {}
                SmRegistrationFinalization::ReplaceWithResumed {
                    resumed,
                    replay_after_h,
                } => {
                    responses = vec![resumed.to_xml()];
                    // Issue #1178: like the pre-registration resume path,
                    // replayed stanzas carry a XEP-0203 <delay/> with their
                    // original receipt time.
                    let server_domain = state.deps.auth_state.xmpp_domain.as_str();
                    responses.extend(
                        conn.sm_state
                            .get_stanzas_to_resend(replay_after_h)
                            .into_iter()
                            .map(|entry| {
                                waddle_xmpp::stream_management::stamp_replay_delay(
                                    &entry.stanza_xml,
                                    server_domain,
                                    entry.original_receipt_at,
                                )
                            }),
                    );
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
            }
        }
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
    let policy = if conn.suppress_sm_record_next_batch {
        conn.suppress_sm_record_next_batch = false;
        BatchSmPolicy::ReplaySuppressed
    } else {
        BatchSmPolicy::Record
    };
    match write_response_batch(
        ws_sender,
        ws_receiver,
        state.as_ref(),
        conn,
        responses,
        policy,
    )
    .await
    {
        BatchWriteOutcome::Continue => {
            conn.publish_pending_sm_enable(state.as_ref());
        }
        BatchWriteOutcome::TransportClosed => return false,
    }

    // A timed-out message/presence dispatch was cancelled before the server
    // accepted XEP-0198 responsibility. End this transport without moving to
    // `Closing`: resumable sessions persist the pre-hole `h`; non-resumable
    // sessions leave their unacknowledged suffix to the sender's stream-end
    // policy. In both cases the server must not falsely acknowledge the hole.
    if conn.sm_inbound_completion.has_unhandled_hole() {
        warn!(
            jid = ?conn.phase.bound_jid(),
            inbound_h = conn.sm_state.get_inbound_count(),
            resumable = conn.sm_state.is_resumable(),
            "Ending transport after unhandled stanza dispatch timeout; preserving sender responsibility"
        );
        return false;
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

async fn handle_ordered_relay_handoff_completion(
    ws_sender: &mut SplitSink<WebSocket, Message>,
    ws_receiver: &mut SplitStream<WebSocket>,
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    completion: crate::server::routes::interpret::OrderedRelayHandoffCompletion,
) -> bool {
    conn.sm_inbound_completion
        .complete(completion.inbound_sequence, &mut conn.sm_state);
    let replies = serialize_ordered_relay_handoff_replies(completion.replies);
    if replies.is_empty() {
        return true;
    }
    match write_response_batch(
        ws_sender,
        ws_receiver,
        state.as_ref(),
        conn,
        replies,
        BatchSmPolicy::Record,
    )
    .await
    {
        BatchWriteOutcome::Continue => true,
        BatchWriteOutcome::TransportClosed => false,
    }
}

async fn drain_ordered_relay_handoffs_before_cleanup(
    handoff_rx: &mut mpsc::UnboundedReceiver<
        crate::server::routes::interpret::OrderedRelayHandoffCompletion,
    >,
    conn: &mut WsConnState,
) {
    if !conn.sm_inbound_completion.has_pending() {
        return;
    }
    if !conn.sm_inbound_completion.has_unhandled_hole() {
        while conn.sm_inbound_completion.has_pending() {
            let Some(completion) = handoff_rx.recv().await else {
                break;
            };
            apply_ordered_relay_handoff_completion(conn, completion);
        }
        conn.sm_inbound_completion.reset();
        return;
    }

    let deadline = tokio::time::Instant::now() + ORDERED_RELAY_HANDOFF_CLEANUP_DEADLINE;
    let mut drained = 0usize;
    while conn.sm_inbound_completion.has_pending()
        && drained < ORDERED_RELAY_HANDOFF_CLEANUP_MAX_COMPLETIONS
    {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok(Some(completion)) = tokio::time::timeout(remaining, handoff_rx.recv()).await else {
            break;
        };
        apply_ordered_relay_handoff_completion(conn, completion);
        drained += 1;
        // Tokio timeouts cannot preempt a future that continuously consumes a
        // ready unbounded-channel backlog. Yield so the deadline is observable
        // even under adversarial queued completion volume.
        tokio::task::yield_now().await;
    }
    if conn.sm_inbound_completion.has_pending() {
        warn!(
            pending = conn.sm_inbound_completion.pending_count(),
            drained,
            timeout_ms = ORDERED_RELAY_HANDOFF_CLEANUP_DEADLINE.as_millis(),
            "Stopped bounded pre-hole ordered-relay drain; preserving conservative sender replay"
        );
    }
    conn.sm_inbound_completion.reset();
}

fn apply_ordered_relay_handoff_completion(
    conn: &mut WsConnState,
    completion: crate::server::routes::interpret::OrderedRelayHandoffCompletion,
) {
    conn.sm_inbound_completion
        .complete(completion.inbound_sequence, &mut conn.sm_state);
    let replies = serialize_ordered_relay_handoff_replies(completion.replies);
    batch_write::record_remaining_for_replay(conn, replies.into_iter(), BatchSmPolicy::Record);
}

fn serialize_ordered_relay_handoff_replies(replies: Vec<Stanza>) -> Vec<String> {
    replies
        .into_iter()
        .filter_map(|reply| {
            let serialized = match reply {
                Stanza::Iq(reply) => waddle_xmpp::parser::stanza_to_string(*reply),
                Stanza::Message(reply) => waddle_xmpp::parser::stanza_to_string(reply),
                Stanza::Presence(reply) => waddle_xmpp::parser::stanza_to_string(reply),
            };
            match serialized {
                Ok(xml) => Some(xml),
                Err(error) => {
                    warn!(%error, "failed to serialize ordered relay handoff reply");
                    None
                }
            }
        })
        .collect()
}

/// Process inbound frames the mid-batch drain deferred before the
/// transport was lost. Runs after the connection loop breaks, before
/// shutdown cleanup: side effects (peer routing, `inbound_count`)
/// still happen; responses cannot be written, so replayable countable
/// ones are recorded into the unacked queue for a future resume.
/// Registration is skipped — a dead transport must not (re)register
/// itself.
async fn process_deferred_inbound_after_transport_loss(
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
) {
    if conn.sm_inbound_completion.has_unhandled_hole() {
        let dropped = discard_deferred_inbound_after_unhandled_hole(conn);
        if dropped > 0 {
            warn!(
                dropped,
                "Dropping deferred inbound frames after XEP-0198 handled-count hole; sender will replay"
            );
        }
        return;
    }
    while let Some(text) = conn.deferred_inbound.pop_front() {
        let responses = handle_xmpp_frame(&text, domain, state, conn).await;
        conn.sync_state_machine_phase();
        let policy = if conn.suppress_sm_record_next_batch {
            conn.suppress_sm_record_next_batch = false;
            BatchSmPolicy::ReplaySuppressed
        } else {
            BatchSmPolicy::Record
        };
        batch_write::record_remaining_for_replay(conn, responses.into_iter(), policy);
        if conn.sm_inbound_completion.has_unhandled_hole() {
            let dropped = discard_deferred_inbound_after_unhandled_hole(conn);
            if dropped > 0 {
                warn!(
                    dropped,
                    "Dropping later deferred inbound frames after XEP-0198 handled-count hole; sender will replay"
                );
            }
            break;
        }
    }
}

fn discard_deferred_inbound_after_unhandled_hole(conn: &mut WsConnState) -> usize {
    let dropped = conn.deferred_inbound.len();
    conn.deferred_inbound.clear();
    dropped
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
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::{SdkTracerProvider, SpanData, SpanExporter};
    use std::str::FromStr;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::prelude::*;

    #[derive(Clone, Debug)]
    struct CaptureSpanExporter(Arc<Mutex<Vec<SpanData>>>);

    impl SpanExporter for CaptureSpanExporter {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.0.lock().expect("capture lock").extend(batch);
            Ok(())
        }
    }

    #[test]
    fn connection_span_records_client_trace_id_and_remote_link_from_query() {
        let exported = Arc::new(Mutex::new(Vec::new()));
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(CaptureSpanExporter(Arc::clone(&exported)))
            .build();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer().with_tracer(provider.tracer("connection-span-test")),
        );

        tracing::subscriber::with_default(subscriber, || {
            let span = connection_established_span(Some(
                "traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            ))
            .expect("valid traceparent creates a connection span");
            let _entered = span.enter();
        });

        let exported = exported.lock().expect("capture lock");
        let span = exported
            .iter()
            .find(|span| span.name == "xmpp.connection.established")
            .expect("connection span exported");
        assert!(
            span.attributes.iter().any(|attribute| {
                attribute.key.as_str() == "client.trace_id"
                    && attribute.value.to_string() == "0af7651916cd43dd8448eb211c80319c"
            }),
            "connection span must expose the linked client trace id: {:?}",
            span.attributes,
        );
        assert_eq!(span.links.len(), 1, "connection span has one client link");
        let linked = &span.links[0].span_context;
        assert_eq!(
            linked.trace_id().to_string(),
            "0af7651916cd43dd8448eb211c80319c"
        );
        assert_eq!(linked.span_id().to_string(), "b7ad6b7169203331");
        assert!(linked.is_remote());
    }

    #[tokio::test]
    async fn abandoned_inbound_slot_does_not_block_handoff_cleanup() {
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("stream-timeout".to_string(), true, Some(300));
        let abandoned = conn.sm_inbound_completion.reserve(&conn.sm_state);
        conn.sm_inbound_completion.abandon(abandoned);

        // Keep the sender alive: the abandoned sequence itself must not remain
        // pending and make cleanup wait for a completion that cannot arrive.
        let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            drain_ordered_relay_handoffs_before_cleanup(&mut rx, &mut conn),
        )
        .await
        .expect("abandoned sequence must not block cleanup");

        assert_eq!(conn.sm_state.get_inbound_count(), 0);
    }

    #[tokio::test]
    async fn cleanup_waits_for_pre_hole_ordered_relay_completion() {
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("stream-timeout".to_string(), true, Some(300));
        let earlier = conn.sm_inbound_completion.reserve(&conn.sm_state);
        let abandoned = conn.sm_inbound_completion.reserve(&conn.sm_state);
        conn.sm_inbound_completion.abandon(abandoned);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let completion = crate::server::routes::interpret::OrderedRelayHandoffCompletion {
            inbound_sequence: earlier,
            replies: Vec::new(),
        };
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            tx.send(completion).expect("cleanup receiver remains open");
        });

        drain_ordered_relay_handoffs_before_cleanup(&mut rx, &mut conn).await;

        assert_eq!(
            conn.sm_state.get_inbound_count(),
            1,
            "the contiguous pre-hole stanza must be acknowledged before detach"
        );
        assert!(!conn.sm_inbound_completion.has_pending());
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_deadline_bounds_missing_pre_hole_completion() {
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("stream-timeout".to_string(), true, Some(300));
        let _missing = conn.sm_inbound_completion.reserve(&conn.sm_state);
        let abandoned = conn.sm_inbound_completion.reserve(&conn.sm_state);
        conn.sm_inbound_completion.abandon(abandoned);

        // The connection itself still owns a sender in production, so keep
        // this one alive to prove cleanup cannot rely on channel closure.
        let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut drain = Box::pin(drain_ordered_relay_handoffs_before_cleanup(
            &mut rx, &mut conn,
        ));
        assert!(futures::poll!(drain.as_mut()).is_pending());

        tokio::time::advance(ORDERED_RELAY_HANDOFF_CLEANUP_DEADLINE).await;
        drain.await;

        assert_eq!(conn.sm_state.get_inbound_count(), 0);
        assert!(!conn.sm_inbound_completion.has_pending());
    }

    #[tokio::test]
    async fn cleanup_work_cap_bounds_ready_pre_hole_backlog() {
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("stream-timeout".to_string(), true, Some(300));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        for _ in 0..=ORDERED_RELAY_HANDOFF_CLEANUP_MAX_COMPLETIONS {
            let sequence = conn.sm_inbound_completion.reserve(&conn.sm_state);
            tx.send(
                crate::server::routes::interpret::OrderedRelayHandoffCompletion {
                    inbound_sequence: sequence,
                    replies: Vec::new(),
                },
            )
            .expect("cleanup receiver remains open");
        }
        let abandoned = conn.sm_inbound_completion.reserve(&conn.sm_state);
        conn.sm_inbound_completion.abandon(abandoned);

        drain_ordered_relay_handoffs_before_cleanup(&mut rx, &mut conn).await;

        assert_eq!(
            conn.sm_state.get_inbound_count(),
            ORDERED_RELAY_HANDOFF_CLEANUP_MAX_COMPLETIONS as u32,
            "cleanup must cap ready backlog work before detaching"
        );
        assert!(!conn.sm_inbound_completion.has_pending());
    }

    #[test]
    fn later_deferred_frames_are_discarded_after_unhandled_hole() {
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("stream-timeout".to_string(), true, Some(300));
        let sequence = conn.sm_inbound_completion.reserve(&conn.sm_state);
        conn.sm_inbound_completion.abandon(sequence);
        conn.deferred_inbound.extend([
            axum::extract::ws::Utf8Bytes::from_static("<message id='later-1'/>"),
            axum::extract::ws::Utf8Bytes::from_static("<presence/>"),
        ]);

        assert_eq!(discard_deferred_inbound_after_unhandled_hole(&mut conn), 2);
        assert!(conn.deferred_inbound.is_empty());
        assert_eq!(conn.sm_state.get_inbound_count(), 0);
    }

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
