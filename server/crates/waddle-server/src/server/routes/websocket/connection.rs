use super::*;
use super::{
    batch_write::{
        write_response_batch_report_with_admission, write_response_batch_with_admission,
        BatchAuthority, BatchSmPolicy, BatchWriteOutcome,
    },
    cleanup::cleanup_connection_shutdown,
    frame::{handle_xmpp_frame_with_admission, ResponseBatch, ResponseFrame, StreamErrorFrame},
    interpret_loop::build_interpret_deps,
    outbound::{handle_outbound_stanza, OutboundAuthority},
    registration::{register_bound_connection_after_frame_with_admission, RegistrationAfterFrame},
    replay::drive_interpret_loop,
    send::{
        close_ws_connection, send_ws_message, send_ws_message_with_authority, send_ws_text_frames,
        send_ws_text_frames_with_authority, AuthoritySendOutcome,
    },
    session_init::build_internal_server_error_stream_error,
    state::{InboundFrameTerminal, WsConnState, TERMINAL_RECOVERY_QUEUE_CAP},
    stream_management::SmRegistrationFinalization,
    timers::TransportTimers,
    transport_xml::{
        build_conflict_stream_error, build_system_shutdown_stream_error,
        websocket_stream_close_element, websocket_stream_close_xml,
    },
};
use axum::{
    extract::{FromRequest, Request},
    response::IntoResponse,
};
use futures::stream::{SplitSink, SplitStream};
use waddle_xmpp::stream_management::SmRequest;

#[derive(Debug, PartialEq, Eq)]
enum WebSocketAdmissionRevocation {
    Shutdown,
    Lifecycle(crate::clustering::NodeAdmissionError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchCompletionOutcome {
    Delivered,
    RetainedForRecovery,
    RetryOnly,
}

/// Revalidate immediately before returning the upgrade response, and again
/// inside the upgrade callback before any XMPP state is created. Axum/Hyper
/// writes HTTP 101 after the handler returns, so a lifecycle transition in
/// that final transport-only gap may still result in 101; the callback check
/// guarantees that socket is then dropped without processing XMPP.
fn revalidate_websocket_admission(
    state: &WebSocketState,
    permit: &crate::clustering::NodeAdmissionPermit,
) -> Result<(), WebSocketAdmissionRevocation> {
    if state.deps.shutdown.stop_token().is_cancelled() {
        return Err(WebSocketAdmissionRevocation::Shutdown);
    }
    permit
        .revalidate()
        .map_err(WebSocketAdmissionRevocation::Lifecycle)
}

async fn close_revoked_upgraded_socket<S, E>(socket: &mut S)
where
    S: futures::Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    let _ = send_ws_message(
        socket,
        Message::Close(None),
        "Failed to send WebSocket close frame after admission revocation",
    )
    .await;
    let _ = close_ws_connection(
        socket,
        "Failed to close WebSocket after admission revocation",
    )
    .await;
}

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
    State(state): State<Arc<WebSocketState>>,
    request: Request,
) -> Response {
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
    // This is deliberately after Ecdysis's mint-then-check gate and before
    // the RFC 7395 upgrade. A fenced, recovering, draining, or terminally
    // failed node must return plain HTTP 503 rather than open an XMPP stream
    // it cannot safely own.
    let admission_permit = match state.deps.app_state.node_lifecycle.admit() {
        Ok(permit) => permit,
        Err(error) => {
            info!(%error, "Rejecting XMPP WebSocket upgrade: node is not serving");
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    info!("XMPP WebSocket connection request");

    let ws = match WebSocketUpgrade::from_request(request, &state).await {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };

    if let Err(reason) = revalidate_websocket_admission(&state, &admission_permit) {
        info!(
            ?reason,
            "Rejecting XMPP WebSocket upgrade: admission was revoked"
        );
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    ws.protocols(["xmpp"])
        .on_upgrade(move |mut socket| async move {
            if let Err(reason) = revalidate_websocket_admission(&state, &admission_permit) {
                info!(
                    ?reason,
                    "Closing upgraded WebSocket: admission was revoked before XMPP start"
                );
                // HTTP 101 is already on the wire. RFC 7395 therefore requires a
                // WebSocket close handshake rather than silently dropping the
                // upgraded TCP stream; no XMPP stream exists at this boundary.
                close_revoked_upgraded_socket(&mut socket).await;
                return;
            }
            handle_xmpp_websocket(socket, state, connection_guard, admission_permit).await;
        })
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
/// Bounded deferred-frame work per connection-loop turn. This clears a
/// full normal parking budget promptly without letting one client monopolize
/// the executor while its deferred queue is non-empty.
const DEFERRED_INBOUND_DRAIN_CHUNK: usize = 8;

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

struct ConnectionIo<'a> {
    sender: &'a mut SplitSink<WebSocket, Message>,
    receiver: &'a mut SplitStream<WebSocket>,
}

#[derive(Clone, Copy)]
struct FrameAuthority<'a> {
    permit: &'a crate::clustering::NodeAdmissionPermit,
    shutdown: &'a tokio_util::sync::CancellationToken,
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
    admission_permit: crate::clustering::NodeAdmissionPermit,
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
    // Set when this connection is asked (identity-matched) to force-detach.
    // The ack is deliberately sent only AFTER
    // `cleanup_connection_shutdown` below has run this connection's normal
    // XEP-0198 detach-for-resume persistence — the asking lifecycle must
    // never proceed until the detach-flush this node performs has actually
    // landed.
    let mut pending_force_detach: Vec<waddle_xmpp::registry::ForceDetachRequest> = Vec::new();

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

    'connection: loop {
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
                if shutdown_token.is_cancelled() || admission_permit.revalidate().is_err() {
                    close_live_session_for_node_unavailable(&mut ws_sender, &conn).await;
                    break;
                }
                if !matches!(
                    send_ws_message_with_authority(
                        &mut ws_sender,
                        Message::Text(SmRequest::to_xml().into()),
                        "Failed to send SM <r/> at send-window loop pause",
                        Some((&admission_permit, &shutdown_token)),
                    )
                    .await,
                    AuthoritySendOutcome::Sent
                ) {
                    break;
                }
                conn.sm_state.note_ack_request_sent();
            }
        } else {
            conn.send_window_pause_deadline = None;
        }
        let send_window_pause_deadline = conn.send_window_pause_deadline;

        // Frames the mid-batch ack drain (issue #1089) pulled off the
        // socket ahead of the dispatcher must be processed in arrival
        // order BEFORE the socket is polled again — so the socket arm
        // below is gated on the queue being empty, and an
        // always-ready arm processes a bounded ordered chunk per
        // iteration. Deferred processing stays a select arm (not a
        // pre-select loop), and yields between chunks, so a client
        // streaming frames mid-flood cannot monopolize the executor.
        let deferred_pending = !conn.deferred_inbound.is_empty();
        tokio::select! {
            biased;

            // The exact serving generation that admitted this socket is its
            // authority. Revoke it ahead of every queued frame, outbound item,
            // and deferred stanza; recovery mints a new generation and can
            // never resurrect this transport.
            _ = admission_permit.revoked() => {
                info!(
                    jid = ?conn.phase.bound_jid(),
                    "Node lifecycle changed: closing live session"
                );
                close_live_session_for_node_unavailable(&mut ws_sender, &conn).await;
                break;
            }

            // Process stop is the highest-priority event. If cancellation and
            // a queued frame/work item are both ready, no further stanza work
            // starts after the node has begun draining or failed critically.
            _ = shutdown_token.cancelled() => {
                info!(
                    jid = ?conn.phase.bound_jid(),
                    "Graceful shutdown: closing live session with system-shutdown stream error"
                );
                close_live_session_for_node_unavailable(&mut ws_sender, &conn).await;
                break;
            }

            // Process a bounded drain-deferred inbound chunk.
            _ = std::future::ready(()), if deferred_pending => {
                for _ in 0..DEFERRED_INBOUND_DRAIN_CHUNK {
                    let Some(text) = conn.deferred_inbound.pop_front() else {
                        break;
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
                        ConnectionIo {
                            sender: &mut ws_sender,
                            receiver: &mut ws_receiver,
                        },
                        FrameAuthority {
                            permit: &admission_permit,
                            shutdown: &shutdown_token,
                        },
                    )
                    .await
                    {
                        break 'connection;
                    }
                }
                tokio::task::yield_now().await;
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
                            ConnectionIo {
                                sender: &mut ws_sender,
                                receiver: &mut ws_receiver,
                            },
                            FrameAuthority {
                                permit: &admission_permit,
                                shutdown: &shutdown_token,
                            },
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
                        if !matches!(
                            send_ws_message_with_authority(
                                &mut ws_sender,
                                Message::Pong(data),
                                "Failed to send pong",
                                Some((&admission_permit, &shutdown_token)),
                            )
                            .await,
                            AuthoritySendOutcome::Sent
                        ) {
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
                            OutboundAuthority {
                                permit: &admission_permit,
                                shutdown: &shutdown_token,
                            },
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
                            &admission_permit,
                            &shutdown_token,
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

            // A force-detach request for this connection's own stream. Cross-node
            // XEP-0198 resume uses the live-steal handshake; stale UserActor
            // retirement uses the same safe connection-owned close path.
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
                            warn!("force-detach rejected: identity mismatch");
                            let _ = request
                                .ack
                                .send(waddle_xmpp::registry::ForceDetachOutcome::IdentityMismatch);
                        } else if shutdown_token.is_cancelled()
                            || admission_permit.revalidate().is_err()
                        {
                            // The old serving generation may no longer emit
                            // the optional `<conflict/>`. Still acknowledge
                            // only after normal detach persistence below.
                            pending_force_detach.push(request);
                            if let Some(bound_bare) = bound_bare.as_ref() {
                                pending_force_detach.extend(drain_ready_force_detach_requests(
                                    &mut force_detach_rx,
                                    bound_bare,
                                ));
                            }
                            break;
                        } else {
                            info!("force-detaching this session (<conflict/> close)");
                            close_live_session_for_force_detach(
                                &mut ws_sender,
                                &conn,
                                &admission_permit,
                                &shutdown_token,
                            )
                            .await;
                            // Deferred: acked only after this connection's own
                            // detach-for-resume cleanup below actually runs.
                            pending_force_detach.push(request);
                            if let Some(bound_bare) = bound_bare.as_ref() {
                                pending_force_detach.extend(drain_ready_force_detach_requests(
                                    &mut force_detach_rx,
                                    bound_bare,
                                ));
                            }
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

            // RFC 7395 §3.8 keepalive clock (issue #1090). Fires only
            // when the state machine armed a timer; the tick is fed
            // back into the machine, whose policy decides: quiet
            // re-arm, probe + re-arm, or close a dead peer. A
            // keepalive close breaks the loop with a graceful WS close
            // handshake and rides the normal shutdown fork below — SM
            // detach-for-resume when negotiated, full cleanup
            // otherwise.
            timer_id = timers.next_expired() => {
                let session = conn.authenticated_session.clone();
                let principal = session
                    .as_ref()
                    .map(super::ResolvedPrincipal::from_authenticated_session);
                let Some(sm) = conn.state_machine.as_mut() else {
                    warn!(timer_id = timer_id.0, "Timer fired without a state machine; disarming");
                    continue;
                };
                let events = sm.handle(InboundEvent::Tick(timer_id));
                let interpret_deps =
                    build_interpret_deps(state.as_ref(), principal);
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
                    if !matches!(
                        send_ws_message_with_authority(
                            &mut ws_sender,
                            Message::Ping(axum::body::Bytes::new()),
                            "Failed to send keepalive ping",
                            Some((&admission_permit, &shutdown_token)),
                        )
                        .await,
                        AuthoritySendOutcome::Sent
                    ) {
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
        // Claim terminalization for the superseded stream happens inside
        // cleanup's superseded branch AFTER the ingress-shadow idle barrier
        // (`forget_terminal_shadow_stream_and_release_claim`). Deferring the
        // claim here, pre-drain, would hand the still-captured fence to the
        // release-retry janitor while admitted submissions are in flight.
    } else {
        if shutdown_token.is_cancelled() || admission_permit.revalidate().is_err() {
            let dropped = discard_deferred_inbound(&mut conn);
            if dropped > 0 {
                info!(
                    dropped,
                    "Dropping deferred inbound frames after node authority revocation; sender may replay them"
                );
            }
        } else {
            process_deferred_inbound_after_transport_loss(
                &domain,
                &state,
                &mut conn,
                &admission_permit,
                &shutdown_token,
            )
            .await;
        }
        drain_ordered_relay_handoffs_before_cleanup(
            &state.deps.protocol.ingress_shadow,
            &mut handoff_rx,
            &mut conn,
        )
        .await;
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
    if !pending_force_detach.is_empty() {
        if let Some(bound_bare) = conn.phase.bound_jid().map(|jid| jid.to_bare()) {
            pending_force_detach.extend(drain_ready_force_detach_requests(
                &mut force_detach_rx,
                &bound_bare,
            ));
        }
    }
    let shutdown_outcome = if !pending_force_detach.is_empty() {
        cleanup_force_detach_shutdown_with_late_waiter_service(
            state.as_ref(),
            &mut outbound_rx,
            &mut conn,
            superseded,
            &mut force_detach_rx,
            &mut pending_force_detach,
        )
        .await
    } else {
        cleanup_connection_shutdown(state.as_ref(), &mut outbound_rx, &mut conn, superseded).await
    };
    finalize_replay_recorded_completions(&state, &mut conn, shutdown_outcome);

    // ADR-0017 Phase 3 Slice 6: only now — after this connection's own
    // detach-for-resume persistence has actually run above — tell the
    // cross-node resume asker it is safe to proceed with `steal_for_resume`.
    // Council-adjudicated FIX 4: ack only what actually happened — map the
    // typed `shutdown_outcome` onto the wire outcome rather than always
    // claiming `Detached`, so the asker never proceeds with
    // `steal_for_resume` against a snapshot that was never persisted (a
    // storage-error fallback, an ownership-race promotion, or any other
    // non-detach cleanup path).
    ack_force_detach_requests(pending_force_detach, shutdown_outcome);

    info!("XMPP WebSocket connection closed");
}

async fn close_live_session_for_node_unavailable<S, E>(ws_sender: &mut S, conn: &WsConnState)
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    let close_peer = async {
        if conn.has_committed_live_stream_open() {
            let _ = send_ws_text_frames(
                ws_sender,
                [
                    build_system_shutdown_stream_error(),
                    websocket_stream_close_xml(),
                ],
                "Failed to send system-shutdown stream error",
            )
            .await;
        }
        let _ = close_ws_connection(
            ws_sender,
            "Failed to send WebSocket close frame after node became unavailable",
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
            "Node unavailable: peer did not accept the close frames in time; \
             proceeding to detach without them"
        );
    }
}

/// Close a live stream displaced by a cross-node XEP-0198 resume.  The
/// conflict wire frames are deliberately gated on a committed live stream;
/// regardless of that gate the WebSocket transport is closed.
async fn close_live_session_for_force_detach<S, E>(
    ws_sender: &mut S,
    conn: &WsConnState,
    admission_permit: &crate::clustering::NodeAdmissionPermit,
    shutdown_token: &tokio_util::sync::CancellationToken,
) where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    if conn.has_committed_live_stream_open() {
        let _ = send_ws_text_frames_with_authority(
            ws_sender,
            [build_conflict_stream_error(), websocket_stream_close_xml()],
            "Failed to send conflict stream error",
            (admission_permit, shutdown_token),
        )
        .await;
    }
    let _ = close_ws_connection(
        ws_sender,
        "Failed to send WebSocket close frame after conflict",
    )
    .await;
}

fn force_detach_outcome_from_shutdown(
    outcome: cleanup::ConnectionShutdownOutcome,
) -> waddle_xmpp::registry::ForceDetachOutcome {
    match outcome {
        cleanup::ConnectionShutdownOutcome::Detached => {
            waddle_xmpp::registry::ForceDetachOutcome::Detached
        }
        cleanup::ConnectionShutdownOutcome::NotPersisted => {
            waddle_xmpp::registry::ForceDetachOutcome::NotPersisted
        }
    }
}

pub(super) fn drain_ready_force_detach_requests(
    rx: &mut Option<mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>>,
    bound_bare: &jid::BareJid,
) -> Vec<waddle_xmpp::registry::ForceDetachRequest> {
    let mut drained = Vec::new();
    let Some(receiver) = rx.as_mut() else {
        return drained;
    };
    loop {
        match receiver.try_recv() {
            Ok(request) => {
                if request.requester_bare_jid != *bound_bare {
                    warn!("force-detach rejected while draining queue: identity mismatch");
                    let _ = request
                        .ack
                        .send(waddle_xmpp::registry::ForceDetachOutcome::IdentityMismatch);
                    continue;
                }
                drained.push(request);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                *rx = None;
                break;
            }
        }
    }
    drained
}

/// Cross-node semantics are authoritative whenever ANY queued request carries
/// that origin, regardless of queue order: a stale-retirement request drained
/// ahead of a cross-node-resume one must not make cleanup skip the synchronous
/// cross-node unregister fence while both waiters are acknowledged from the
/// same outcome — the remote resumer would race the still-pending removal and
/// reject a valid resume.
pub(super) fn authoritative_force_detach_origin(
    requests: &[waddle_xmpp::registry::ForceDetachRequest],
) -> Option<waddle_xmpp::registry::ForceDetachOrigin> {
    requests
        .iter()
        .any(|request| request.origin == waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume)
        .then_some(waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume)
        .or_else(|| requests.first().map(|request| request.origin))
}

pub(super) fn release_stale_force_detach_waiters_before_cross_node_cleanup(
    requests: &mut Vec<waddle_xmpp::registry::ForceDetachRequest>,
    primary_origin: Option<waddle_xmpp::registry::ForceDetachOrigin>,
) {
    if primary_origin != Some(waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume) {
        return;
    }

    let mut remaining = Vec::with_capacity(requests.len());
    for request in requests.drain(..) {
        if request.origin == waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement
        {
            // A queued stale-retirement waiter already owns the UserRegistry
            // actor turn. Answer it before the cross-node cleanup's
            // synchronous unregister ask so that actor can finish its exact
            // removal work instead of timing the ask out behind itself.
            let _ = request
                .ack
                .send(waddle_xmpp::registry::ForceDetachOutcome::NotPersisted);
            continue;
        }
        remaining.push(request);
    }
    *requests = remaining;
}

async fn cleanup_force_detach_shutdown_with_late_waiter_service(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    conn: &mut WsConnState,
    superseded: bool,
    force_detach_rx: &mut Option<mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>>,
    pending_force_detach: &mut Vec<waddle_xmpp::registry::ForceDetachRequest>,
) -> cleanup::ConnectionShutdownOutcome {
    let primary_force_detach_origin = authoritative_force_detach_origin(pending_force_detach);
    release_stale_force_detach_waiters_before_cross_node_cleanup(
        pending_force_detach,
        primary_force_detach_origin,
    );

    let Some(origin) = primary_force_detach_origin else {
        return cleanup_connection_shutdown(state, outbound_rx, conn, superseded).await;
    };

    let late_waiter_task = conn
        .phase
        .bound_jid()
        .map(|jid| jid.to_bare())
        .and_then(|bound_bare| {
            start_late_force_detach_waiter_service(force_detach_rx, bound_bare, origin)
        });

    let shutdown_outcome = cleanup::cleanup_force_detach_connection_shutdown(
        state,
        outbound_rx,
        conn,
        superseded,
        origin,
    )
    .await;

    if let Some((cancel, task)) = late_waiter_task {
        cancel.cancel();
        match task.await {
            Ok(mut late_requests) => pending_force_detach.append(&mut late_requests),
            Err(error) => {
                warn!(?error, "late force-detach waiter service task failed");
            }
        }
    }

    shutdown_outcome
}

fn start_late_force_detach_waiter_service(
    force_detach_rx: &mut Option<mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>>,
    bound_bare: jid::BareJid,
    primary_origin: waddle_xmpp::registry::ForceDetachOrigin,
) -> Option<(
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<Vec<waddle_xmpp::registry::ForceDetachRequest>>,
)> {
    let receiver = force_detach_rx.take()?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_task = cancel.clone();
    Some((
        cancel,
        tokio::spawn(async move {
            service_late_force_detach_waiters_during_cross_node_cleanup(
                receiver,
                bound_bare,
                primary_origin,
                cancel_task,
            )
            .await
        }),
    ))
}

async fn service_late_force_detach_waiters_during_cross_node_cleanup(
    mut receiver: mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>,
    bound_bare: jid::BareJid,
    primary_origin: waddle_xmpp::registry::ForceDetachOrigin,
    cancel: tokio_util::sync::CancellationToken,
) -> Vec<waddle_xmpp::registry::ForceDetachRequest> {
    let mut pending = Vec::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                drain_late_force_detach_waiters_during_cross_node_cleanup(
                    &mut receiver,
                    &bound_bare,
                    primary_origin,
                    &mut pending,
                );
                return pending;
            }
            request = receiver.recv() => {
                let Some(request) = request else {
                    return pending;
                };
                handle_late_force_detach_waiter_during_cross_node_cleanup(
                    request,
                    &bound_bare,
                    primary_origin,
                    &mut pending,
                );
            }
        }
    }
}

fn drain_late_force_detach_waiters_during_cross_node_cleanup(
    receiver: &mut mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>,
    bound_bare: &jid::BareJid,
    primary_origin: waddle_xmpp::registry::ForceDetachOrigin,
    pending: &mut Vec<waddle_xmpp::registry::ForceDetachRequest>,
) {
    loop {
        match receiver.try_recv() {
            Ok(request) => handle_late_force_detach_waiter_during_cross_node_cleanup(
                request,
                bound_bare,
                primary_origin,
                pending,
            ),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            | Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
        }
    }
}

fn handle_late_force_detach_waiter_during_cross_node_cleanup(
    request: waddle_xmpp::registry::ForceDetachRequest,
    bound_bare: &jid::BareJid,
    primary_origin: waddle_xmpp::registry::ForceDetachOrigin,
    pending: &mut Vec<waddle_xmpp::registry::ForceDetachRequest>,
) {
    if request.requester_bare_jid != *bound_bare {
        warn!("force-detach rejected during cleanup: identity mismatch");
        let _ = request
            .ack
            .send(waddle_xmpp::registry::ForceDetachOutcome::IdentityMismatch);
        return;
    }

    if primary_origin == waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume
        && request.origin == waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement
    {
        // While the cross-node cleanup is synchronously re-entering the
        // UserRegistry actor, a newly queued stale-retirement request already
        // owns that same actor turn and must be released immediately.
        let _ = request
            .ack
            .send(waddle_xmpp::registry::ForceDetachOutcome::NotPersisted);
        return;
    }

    if primary_origin != waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume
        && request.origin == waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume
    {
        // The running cleanup skipped the synchronous actor-unregister fence
        // a cross-node resume REQUIRES. Handing this stronger waiter the
        // weaker cleanup's Detached would let the remote takeover race the
        // still-pending owner retirement — answer NotPersisted so the
        // resumer's bounded retry re-asks once the retirement settles.
        let _ = request
            .ack
            .send(waddle_xmpp::registry::ForceDetachOutcome::NotPersisted);
        return;
    }

    pending.push(request);
}

fn ack_force_detach_requests(
    requests: Vec<waddle_xmpp::registry::ForceDetachRequest>,
    shutdown_outcome: cleanup::ConnectionShutdownOutcome,
) {
    let outcome = force_detach_outcome_from_shutdown(shutdown_outcome);
    for request in requests {
        let _ = request.ack.send(outcome);
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn websocket_request() -> Request<Body> {
        Request::builder()
            .uri("/ws")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Body::empty())
            .expect("valid RFC 7395 websocket handshake request")
    }

    #[tokio::test]
    async fn websocket_rejects_every_non_serving_admission_state_before_upgrade() {
        let state = super::super::tests::create_test_websocket_state().await;
        let app = router(state.clone());

        // `tower::oneshot` has no Hyper upgrade extension, so a serving node
        // reaches Axum's expected 426 rejection instead of attempting a real
        // 101. The non-serving cases below are deliberately rejected before
        // this extractor and therefore return plain HTTP 503.
        assert_eq!(
            app.clone()
                .oneshot(websocket_request())
                .await
                .expect("response")
                .status(),
            axum::http::StatusCode::UPGRADE_REQUIRED
        );

        state.deps.app_state.node_lifecycle.begin_fenced_recovery();
        assert_eq!(
            app.clone()
                .oneshot(websocket_request())
                .await
                .expect("response")
                .status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );

        state.deps.app_state.node_lifecycle.begin_drain();
        assert_eq!(
            app.clone()
                .oneshot(websocket_request())
                .await
                .expect("response")
                .status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );

        state
            .deps
            .app_state
            .node_lifecycle
            .fail(crate::clustering::CriticalNodeFailure::UserRegistryTerminated);
        assert_eq!(
            app.oneshot(websocket_request())
                .await
                .expect("response")
                .status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn lifecycle_transition_revokes_an_inflight_upgrade_permit() {
        let state = super::super::tests::create_test_websocket_state().await;
        let lifecycle = &state.deps.app_state.node_lifecycle;
        let permit = lifecycle.admit().expect("initial serving permit");

        lifecycle.begin_fenced_recovery();
        assert_eq!(
            revalidate_websocket_admission(&state, &permit),
            Err(WebSocketAdmissionRevocation::Lifecycle(
                crate::clustering::NodeAdmissionError::NotServing(
                    crate::clustering::NodeAdmission::FencedRecovering
                )
            ))
        );

        lifecycle.serve();
        assert_eq!(
            revalidate_websocket_admission(&state, &permit),
            Err(WebSocketAdmissionRevocation::Lifecycle(
                crate::clustering::NodeAdmissionError::Revoked
            ))
        );

        let recovered_permit = lifecycle.admit().expect("recovered serving permit");
        lifecycle.fail(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated);
        assert!(matches!(
            revalidate_websocket_admission(&state, &recovered_permit),
            Err(WebSocketAdmissionRevocation::Lifecycle(
                crate::clustering::NodeAdmissionError::NotServing(
                    crate::clustering::NodeAdmission::Failed(
                        crate::clustering::CriticalNodeFailure::RoomRegistryTerminated
                    )
                )
            ))
        ));
    }

    #[tokio::test]
    async fn ordinary_process_stop_revokes_upgrade_without_latching_critical_failure() {
        let state = super::super::tests::create_test_websocket_state().await;
        let permit = state
            .deps
            .app_state
            .node_lifecycle
            .admit()
            .expect("initial serving permit");

        state.deps.shutdown.stop_token().cancel();

        assert_eq!(
            revalidate_websocket_admission(&state, &permit),
            Err(WebSocketAdmissionRevocation::Shutdown)
        );
        assert_eq!(state.deps.app_state.node_lifecycle.critical_failure(), None);
    }

    #[tokio::test]
    async fn biased_connection_select_prefers_cancelled_stop_to_ready_bound_stanza() {
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        let ready_bound_stanza =
            std::future::ready(Stanza::Message(xmpp_parsers::message::Message::new(Some(
                "peer@example.com".parse::<jid::Jid>().expect("peer JID"),
            ))));
        let mut routed = false;

        tokio::select! {
            biased;
            _ = stop.cancelled() => {}
            _ = ready_bound_stanza => routed = true,
        }

        assert!(
            !routed,
            "cancelled process stop must win before stanza work"
        );
    }
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
    io: ConnectionIo<'_>,
    authority: FrameAuthority<'_>,
) -> bool {
    let RegistrationChannels {
        pending_tx,
        force_detach_rx,
    } = channels;
    let ConnectionIo {
        sender: ws_sender,
        receiver: ws_receiver,
    } = io;
    let FrameAuthority {
        permit: admission_permit,
        shutdown: shutdown_token,
    } = authority;
    if close_if_frame_authority_revoked(state, conn, ws_sender, admission_permit, shutdown_token)
        .await
    {
        return false;
    }
    debug!(len = text.len(), "Received XMPP WebSocket message");
    // Any inbound frame is liveness evidence for the
    // RFC 7395 §3.8 keepalive policy (issue #1090).
    conn.note_transport_activity();

    // Handle XMPP framing (RFC 7395)
    let mut responses = handle_xmpp_frame_with_admission(
        text,
        domain,
        state.as_ref(),
        conn,
        admission_permit,
        shutdown_token,
    )
    .await;
    if matches!(
        conn.inbound_frame_terminal.take(),
        Some(InboundFrameTerminal::AuthorityRevoked)
    ) {
        close_live_session_for_node_unavailable(ws_sender, conn).await;
        return false;
    }
    if close_if_frame_authority_revoked(state, conn, ws_sender, admission_permit, shutdown_token)
        .await
    {
        return false;
    }

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
    match register_bound_connection_after_frame_with_admission(
        state.as_ref(),
        domain,
        conn,
        pending_tx,
        admission_permit,
        shutdown_token,
    )
    .await
    {
        RegistrationAfterFrame::Unchanged => {}
        RegistrationAfterFrame::SessionInitializationFailed => {
            if close_if_frame_authority_revoked(
                state,
                conn,
                ws_sender,
                admission_permit,
                shutdown_token,
            )
            .await
            {
                return false;
            }
            let stream_error = build_internal_server_error_stream_error(
                "Session initialization failed; please reconnect.",
            );
            let send_outcome = send_ws_text_frames_with_authority(
                ws_sender,
                [stream_error, websocket_stream_close_xml()],
                "Failed to send session-init stream error",
                (admission_permit, shutdown_token),
            )
            .await;
            // A registration abort (actor busy/failed) can kill a resume
            // whose provisional acceptance was never counted; the stream
            // error just written IS that attempt's wire terminal.
            if matches!(send_outcome, AuthoritySendOutcome::Sent)
                && conn.pending_resume_stream_id.is_some()
            {
                super::stream_management::observe_sm_resume_finalized(
                    waddle_xmpp::telemetry::attributes::SmResumeOutcome::Internal,
                    conn.pending_resume_stream_id
                        .as_deref()
                        .map(waddle_xmpp::pending_delivery::SmSessionId::new)
                        .as_ref(),
                );
            }
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
                    responses = ResponseBatch::from_frames(vec![resumed.to_element()]);
                    // Issue #1178: like the pre-registration resume path,
                    // replayed stanzas carry a XEP-0203 <delay/> with their
                    // original receipt time.
                    let server_domain = state.deps.auth_state.xmpp_domain.as_str();
                    responses.frames.extend(
                        conn.sm_state
                            .get_stanzas_to_resend(replay_after_h)
                            .into_iter()
                            .map(|entry| {
                                ResponseFrame::from_serialized_xml(
                                    waddle_xmpp::stream_management::stamp_replay_delay(
                                        &entry.stanza_xml,
                                        server_domain,
                                        entry.original_receipt_at,
                                    ),
                                )
                            }),
                    );
                }
                SmRegistrationFinalization::ReplaceWithFailed(failed) => {
                    responses = ResponseBatch::from_frames(vec![failed.to_element()]);
                }
                SmRegistrationFinalization::ReplaceWithHandledCountTooHigh {
                    acknowledged,
                    send_count,
                } => {
                    responses = ResponseBatch::from_frames(vec![
                        ResponseFrame::from(StreamErrorFrame::HandledCountTooHigh {
                            acknowledged,
                            send_count,
                        }),
                        ResponseFrame::from(websocket_stream_close_element()),
                    ]);
                }
            }
        }
        RegistrationAfterFrame::AuthorityRevoked
        | RegistrationAfterFrame::AuthorityRevokedAfterSmFinalization => {
            let _ = close_if_frame_authority_revoked(
                state,
                conn,
                ws_sender,
                admission_permit,
                shutdown_token,
            )
            .await;
            return false;
        }
    }

    ensure_websocket_stream_close_for_closing_phase(conn, &mut responses.frames);

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
    let write_report = write_response_batch_report_with_admission(
        ws_sender,
        ws_receiver,
        state.as_ref(),
        conn,
        responses.frames.clone(),
        policy,
        BatchAuthority {
            permit: admission_permit,
            shutdown: shutdown_token,
        },
    )
    .await;
    // The terminal resume frame is always frame 0 of its batch, and the
    // writer sends frames in order — any written frame means the terminal
    // reached the wire. Record the staged result exactly then, even when a
    // LATER frame of the same batch broke the transport; and never when the
    // batch died before its first write.
    if write_report.written_frame_count > 0 {
        if let Some(outcome) = conn.pending_finalized_resume_outcome.take() {
            super::stream_management::observe_sm_resume_finalized(
                outcome,
                conn.sm_state
                    .stream_id
                    .as_deref()
                    .map(waddle_xmpp::pending_delivery::SmSessionId::new)
                    .as_ref(),
            );
        }
    }
    match write_report.outcome {
        BatchWriteOutcome::Continue => {
            conn.commit_server_stream_open_response();
            conn.publish_pending_sm_enable(state.as_ref());
            let accepted_frame_indices: Vec<_> = (0..responses.frames.len()).collect();
            settle_batch_completions(
                state,
                BatchCompletionOutcome::Delivered,
                conn.sm_state.is_resumable(),
                &accepted_frame_indices,
                response_batch_completion_frames(responses),
            );
        }
        BatchWriteOutcome::TransportClosed | BatchWriteOutcome::DeferredCapExhausted => {
            settle_batch_completions(
                state,
                BatchCompletionOutcome::RetainedForRecovery,
                conn.sm_state.is_resumable(),
                &write_report.accepted_frame_indices,
                response_batch_completion_frames(responses),
            );
            return false;
        }
        BatchWriteOutcome::AuthorityRevoked => {
            settle_batch_completions(
                state,
                BatchCompletionOutcome::RetainedForRecovery,
                conn.sm_state.is_resumable(),
                &write_report.accepted_frame_indices,
                response_batch_completion_frames(responses),
            );
            // No further frame was recorded or written. Any `<enable/>`
            // response that did reach the socket returned Continue and must
            // still publish synchronously at its wire commit point.
            let _ = close_if_frame_authority_revoked(
                state,
                conn,
                ws_sender,
                admission_permit,
                shutdown_token,
            )
            .await;
            return false;
        }
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

async fn close_if_frame_authority_revoked(
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    ws_sender: &mut SplitSink<WebSocket, Message>,
    permit: &crate::clustering::NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> bool {
    if !shutdown.is_cancelled() && permit.revalidate().is_ok() {
        return false;
    }

    cleanup_frame_authority_revocation(state, conn).await;
    close_live_session_for_node_unavailable(ws_sender, conn).await;
    true
}

async fn cleanup_frame_authority_revocation(_state: &WebSocketState, conn: &mut WsConnState) {
    // `<enable/>` has not reached its wire commit point yet. Dropping this
    // typed guard inventories exact claim release; a stale generation must
    // never send `<enabled/>`.
    drop(conn.pending_sm_enable_commit.take());
    if let Some(stream_id) = conn.pending_resume_stream_id.take() {
        conn.pending_resume_h = None;
        drop(conn.pending_resume_claim.take());
        debug!(%stream_id, "Released SM resume claim after authority revocation");
    }
}

async fn handle_ordered_relay_handoff_completion<S, SE, R, RE>(
    ws_sender: &mut S,
    ws_receiver: &mut R,
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    completion: crate::server::routes::interpret::OrderedRelayHandoffCompletion,
    permit: &crate::clustering::NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> bool
where
    S: futures::Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    if shutdown.is_cancelled() || permit.revalidate().is_err() {
        conn.sm_inbound_completion
            .abandon(completion.inbound_sequence);
        return false;
    }
    conn.sm_inbound_completion.complete(
        completion.inbound_sequence,
        &mut conn.sm_state,
        |submission| {
            let _ = state.deps.protocol.ingress_shadow.try_submit(submission);
        },
    );
    let replies = serialize_ordered_relay_handoff_replies(completion.replies);
    if replies.is_empty() {
        return true;
    }
    match write_response_batch_with_admission(
        ws_sender,
        ws_receiver,
        state.as_ref(),
        conn,
        replies,
        BatchSmPolicy::Record,
        BatchAuthority { permit, shutdown },
    )
    .await
    {
        BatchWriteOutcome::Continue => true,
        BatchWriteOutcome::TransportClosed | BatchWriteOutcome::DeferredCapExhausted => false,
        BatchWriteOutcome::AuthorityRevoked => false,
    }
}

async fn drain_ordered_relay_handoffs_before_cleanup(
    ingress_shadow: &crate::ingress_shadow::IngressShadowHandle,
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
            if terminal_recovery_recording_is_full(conn) {
                warn!(
                    cap = TERMINAL_RECOVERY_QUEUE_CAP,
                    recorded = conn.terminal_sm_recovery.queue_len(),
                    pending = conn.sm_inbound_completion.pending_count(),
                    "Stopped ordered-relay handoff recording after terminal SM recovery cap; sender replay remains conservative"
                );
                break;
            }
            let Some(completion) = handoff_rx.recv().await else {
                break;
            };
            apply_ordered_relay_handoff_completion(ingress_shadow, conn, completion);
        }
        conn.sm_inbound_completion.reset();
        return;
    }

    let deadline = tokio::time::Instant::now() + ORDERED_RELAY_HANDOFF_CLEANUP_DEADLINE;
    let mut drained = 0usize;
    while conn.sm_inbound_completion.has_pending()
        && drained < ORDERED_RELAY_HANDOFF_CLEANUP_MAX_COMPLETIONS
    {
        if terminal_recovery_recording_is_full(conn) {
            warn!(
                cap = TERMINAL_RECOVERY_QUEUE_CAP,
                recorded = conn.terminal_sm_recovery.queue_len(),
                pending = conn.sm_inbound_completion.pending_count(),
                "Stopped ordered-relay handoff recording after terminal SM recovery cap; sender replay remains conservative"
            );
            break;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok(Some(completion)) = tokio::time::timeout(remaining, handoff_rx.recv()).await else {
            break;
        };
        apply_ordered_relay_handoff_completion(ingress_shadow, conn, completion);
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
    ingress_shadow: &crate::ingress_shadow::IngressShadowHandle,
    conn: &mut WsConnState,
    completion: crate::server::routes::interpret::OrderedRelayHandoffCompletion,
) {
    conn.sm_inbound_completion.complete(
        completion.inbound_sequence,
        &mut conn.sm_state,
        |submission| {
            let _ = ingress_shadow.try_submit(submission);
        },
    );
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
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    permit: &crate::clustering::NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) {
    if conn.sm_inbound_completion.has_unhandled_hole() {
        let dropped = discard_deferred_inbound(conn);
        if dropped > 0 {
            warn!(
                dropped,
                "Dropping deferred inbound frames after XEP-0198 handled-count hole; sender will replay"
            );
        }
        return;
    }
    if conn.sm_recovery_required {
        // Terminal recovery deliberately invalidates resume, so the client
        // never learns a handled count covering these still-parked stanzas
        // and, per XEP-0198, resends them after the failed resume. Executing
        // them here would run routing and non-idempotent IQ mutations a
        // second time on that resend; discarding is lossless because nothing
        // parked was ever handled or counted.
        let dropped = discard_deferred_inbound(conn);
        if dropped > 0 {
            warn!(
                dropped,
                "Dropping unhandled deferred inbound frames after terminal SM recovery; \
                 sender replays them after the deliberately failed resume"
            );
        }
        return;
    }
    while let Some(text) = conn.deferred_inbound.pop_front() {
        if terminal_recovery_recording_is_full(conn) {
            let dropped = 1 + discard_deferred_inbound(conn);
            warn!(
                cap = TERMINAL_RECOVERY_QUEUE_CAP,
                recorded = conn.terminal_sm_recovery.queue_len(),
                dropped,
                "Dropping deferred inbound frames after terminal SM recovery cap; sender will replay after fresh bind"
            );
            break;
        }
        let responses =
            handle_xmpp_frame_with_admission(&text, domain, state.as_ref(), conn, permit, shutdown)
                .await;
        if matches!(
            conn.inbound_frame_terminal.take(),
            Some(InboundFrameTerminal::AuthorityRevoked)
        ) || shutdown.is_cancelled()
            || permit.revalidate().is_err()
        {
            // No transport remains to close. Release any provisional control
            // ownership now; the caller immediately runs normal detach/full
            // cleanup for the connection state that remains authoritative.
            cleanup_frame_authority_revocation(state, conn).await;
            let dropped = discard_deferred_inbound(conn);
            if dropped > 0 {
                warn!(
                    dropped,
                    "Dropping deferred inbound suffix after authority revocation"
                );
            }
            break;
        }
        conn.sync_state_machine_phase();
        let policy = if conn.suppress_sm_record_next_batch {
            conn.suppress_sm_record_next_batch = false;
            BatchSmPolicy::ReplaySuppressed
        } else {
            BatchSmPolicy::Record
        };
        record_response_batch_for_replay(state, conn, responses, policy);
        if conn.sm_inbound_completion.has_unhandled_hole() {
            let dropped = discard_deferred_inbound(conn);
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

fn record_response_batch_for_replay(
    _state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    responses: ResponseBatch,
    policy: BatchSmPolicy,
) {
    let accepted_frame_indices =
        batch_write::record_remaining_for_replay(conn, responses.frames.iter().cloned(), policy);
    let outcome = match policy {
        BatchSmPolicy::Record => BatchCompletionOutcome::RetainedForRecovery,
        BatchSmPolicy::ReplaySuppressed => BatchCompletionOutcome::RetryOnly,
    };
    let (completions_to_complete, retained) = select_batch_completions_to_complete(
        outcome,
        conn.sm_state.is_resumable(),
        &accepted_frame_indices,
        response_batch_completion_frames(responses),
    );
    if retained > 0 {
        debug!(
            retained,
            ?outcome,
            resumable = conn.sm_state.is_resumable(),
            "Retaining room effect completions for retry after replay-only response batch was not durably accepted"
        );
    }
    queue_replay_recorded_completions(conn, completions_to_complete);
}

fn terminal_recovery_recording_is_full(conn: &WsConnState) -> bool {
    conn.sm_recovery_required
        && conn.terminal_sm_recovery.queue_len() >= TERMINAL_RECOVERY_QUEUE_CAP
}

fn discard_deferred_inbound(conn: &mut WsConnState) -> usize {
    let dropped = conn.deferred_inbound.len();
    conn.deferred_inbound.clear();
    dropped
}

fn ensure_websocket_stream_close_for_closing_phase(
    conn: &WsConnState,
    responses: &mut Vec<ResponseFrame>,
) {
    if !matches!(conn.phase, ConnectionPhase::Closing { .. })
        || response_batch_ends_with_websocket_stream_close(responses)
    {
        return;
    }

    responses.push(ResponseFrame::from(websocket_stream_close_element()));
}

fn response_batch_ends_with_websocket_stream_close(responses: &[ResponseFrame]) -> bool {
    responses
        .last()
        .is_some_and(ResponseFrame::is_websocket_stream_close)
}

fn response_batch_completion_frames(
    responses: ResponseBatch,
) -> Vec<(
    crate::room_effect_outbox::drain::RoomEffectCompletion,
    usize,
)> {
    responses
        .completions
        .into_iter()
        .zip(responses.completion_frame_indices)
        .collect()
}

fn complete_batch_completions(
    state: &Arc<WebSocketState>,
    completions: Vec<crate::room_effect_outbox::drain::RoomEffectCompletion>,
) {
    for completion in completions {
        let state = Arc::clone(state);
        tokio::spawn(async move {
            match crate::room_effect_outbox::drain::complete_after_write(
                state.as_ref(),
                &completion,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => {
                    debug!(
                        key = ?completion.key,
                        "Retaining room effect completion after missing local acceptance"
                    );
                }
                Err(error) => {
                    warn!(
                        key = ?completion.key,
                        %error,
                        "Failed to finish room effect completion after accepted response batch"
                    );
                }
            }
        });
    }
}

fn settle_batch_completions(
    state: &Arc<WebSocketState>,
    outcome: BatchCompletionOutcome,
    resumable: bool,
    accepted_frame_indices: &[usize],
    completion_frames: Vec<(
        crate::room_effect_outbox::drain::RoomEffectCompletion,
        usize,
    )>,
) {
    let (completions_to_complete, retained) = select_batch_completions_to_complete(
        outcome,
        resumable,
        accepted_frame_indices,
        completion_frames,
    );
    if retained > 0 {
        debug!(
            retained,
            ?outcome,
            resumable,
            "Retaining room effect completions for retry after response batch was not durably accepted"
        );
    }
    complete_batch_completions(state, completions_to_complete);
}

fn queue_replay_recorded_completions(
    conn: &mut WsConnState,
    completions: Vec<crate::room_effect_outbox::drain::RoomEffectCompletion>,
) {
    let mut queued: std::collections::HashSet<_> = conn
        .pending_replay_completions
        .iter()
        .map(|completion| (completion.key.clone(), completion.lease.clone()))
        .collect();
    for completion in completions {
        let completion_key = (completion.key.clone(), completion.lease.clone());
        if queued.insert(completion_key) {
            conn.pending_replay_completions.push(completion);
        }
    }
}

fn finalize_replay_recorded_completions(
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    shutdown_outcome: cleanup::ConnectionShutdownOutcome,
) {
    if conn.pending_replay_completions.is_empty() {
        return;
    }

    if shutdown_outcome == cleanup::ConnectionShutdownOutcome::Detached {
        complete_batch_completions(state, std::mem::take(&mut conn.pending_replay_completions));
        return;
    }

    debug!(
        retained = conn.pending_replay_completions.len(),
        ?shutdown_outcome,
        "Retaining replay-only room effect completions because cleanup did not persist a detached owner"
    );
}

fn select_batch_completions_to_complete(
    outcome: BatchCompletionOutcome,
    resumable: bool,
    accepted_frame_indices: &[usize],
    completion_frames: Vec<(
        crate::room_effect_outbox::drain::RoomEffectCompletion,
        usize,
    )>,
) -> (
    Vec<crate::room_effect_outbox::drain::RoomEffectCompletion>,
    usize,
) {
    let accepted_frame_indices: std::collections::HashSet<_> =
        accepted_frame_indices.iter().copied().collect();
    let mut grouped = std::collections::HashMap::new();
    for (completion, frame_index) in completion_frames {
        let completion_key = (completion.key.clone(), completion.lease.clone());
        grouped
            .entry(completion_key)
            .and_modify(
                |(_, all_frames_accepted): &mut (
                    crate::room_effect_outbox::drain::RoomEffectCompletion,
                    bool,
                )| {
                    *all_frames_accepted &= accepted_frame_indices.contains(&frame_index)
                },
            )
            .or_insert((completion, accepted_frame_indices.contains(&frame_index)));
    }
    let mut completions_to_complete = Vec::new();
    let mut retained = 0usize;
    for (_, (completion, all_frames_accepted)) in grouped {
        let complete = all_frames_accepted
            && (matches!(outcome, BatchCompletionOutcome::Delivered)
                || (resumable && matches!(outcome, BatchCompletionOutcome::RetainedForRecovery)));
        if complete {
            completions_to_complete.push(completion);
        } else {
            retained += 1;
        }
    }
    (completions_to_complete, retained)
}

#[cfg(test)]
mod tests {
    use super::super::{
        batch_write::{
            write_response_batch_with_admission, BatchAuthority, BatchSmPolicy, BatchWriteOutcome,
        },
        cleanup::cleanup_connection_shutdown,
        state::TERMINAL_RECOVERY_QUEUE_CAP,
        transport_xml::websocket_stream_open_xml,
    };
    use super::*;
    use jid::{BareJid, FullJid};
    use std::pin::Pin;
    use std::str::FromStr;
    use std::task::{Context, Poll};
    use tokio::sync::oneshot;
    use waddle_xmpp::stream_management::SmSessionRegistry;

    #[derive(Default)]
    struct UpgradeCloseSink {
        sent: Vec<Message>,
        closed: bool,
    }

    impl futures::Sink<Message> for UpgradeCloseSink {
        type Error = &'static str;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.sent.push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.closed = true;
            Poll::Ready(Ok(()))
        }
    }

    struct RevokeWhileReadyPendingSink {
        lifecycle: crate::clustering::NodeLifecycle,
        start_send_called: bool,
    }

    impl futures::Sink<Message> for RevokeWhileReadyPendingSink {
        type Error = &'static str;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.lifecycle.begin_fenced_recovery();
            Poll::Pending
        }

        fn start_send(mut self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            self.start_send_called = true;
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    fn replay_completion_room_jid() -> BareJid {
        BareJid::from_str("replay-completion@muc.example.com").expect("room JID")
    }

    fn other_replay_completion_room_jid() -> BareJid {
        BareJid::from_str("other-replay-completion@muc.example.com").expect("room JID")
    }

    fn replay_completion_origin() -> crate::room_effect_outbox::RoomEffectOriginInstanceId {
        crate::room_effect_outbox::RoomEffectOriginInstanceId::new(
            "connection-replay-completion".to_owned(),
        )
        .expect("origin instance")
    }

    fn replay_completion_node() -> crate::room_effect_outbox::RoomEffectProducingNode {
        crate::room_effect_outbox::RoomEffectProducingNode::from_node_identity(
            waddle_xmpp::ownership::NodeIdentity::new("node-a", "epoch-a"),
        )
    }

    async fn create_owned_room_and_lifecycle_for_replay_completion(
        state: &WebSocketState,
        room_jid: &BareJid,
    ) -> waddle_xmpp::muc::RoomLifecycleId {
        let lifecycle = waddle_xmpp::muc::RoomLifecycleId::generate();
        state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "connection-replay-completion".to_owned(),
                channel_id: "connection-replay-completion".to_owned(),
                config: waddle_xmpp::muc::RoomConfig::default(),
            })
            .await
            .expect("create room");
        let connection = state
            .deps
            .protocol
            .room_effect_outbox
            .database()
            .guard()
            .await
            .expect("connection");
        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
                (),
            )
            .await
            .expect("create lifecycle table");
        connection
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    waddle_xmpp::muc::RoomRevision::initial().as_i64(),
                    waddle_xmpp::muc::RoomLifecycleState::Active.as_db_str(),
                ],
            )
            .await
            .expect("insert lifecycle");
        lifecycle
    }

    async fn enqueue_inline_config_reservation_for_replay_completion(
        state: &WebSocketState,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        revision: waddle_xmpp::muc::RoomRevision,
        recipient: &FullJid,
    ) -> waddle_xmpp::muc::RoomEffectReservation {
        enqueue_inline_config_reservation_for_room_replay_completion(
            state,
            replay_completion_room_jid(),
            lifecycle,
            revision,
            recipient,
        )
        .await
    }

    async fn enqueue_inline_config_reservation_for_room_replay_completion(
        state: &WebSocketState,
        room_jid: BareJid,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        revision: waddle_xmpp::muc::RoomRevision,
        recipient: &FullJid,
    ) -> waddle_xmpp::muc::RoomEffectReservation {
        let effects = waddle_xmpp::muc::RoomMutationEffects::config(
            room_jid,
            vec![waddle_xmpp::muc::MucConfigStatusCode::NonPrivacyConfigurationChange],
            vec![recipient.clone()],
        );
        let origin = replay_completion_origin();
        let producing_node = replay_completion_node();
        let store = state.deps.protocol.room_effect_outbox.as_ref();
        let mut tx = store.database().begin().await.expect("transaction");
        let reservation = store
            .enqueue_in_tx(
                &mut tx,
                crate::room_effect_outbox::RoomEffectEnqueue {
                    lifecycle,
                    revision,
                    effects: &effects,
                    origin: &origin,
                    producing_node: &producing_node,
                    now_ms: 0,
                },
            )
            .await
            .expect("enqueue");
        tx.commit().await.expect("commit");
        reservation
    }

    async fn drain_single_inline_completion_frame(
        state: &WebSocketState,
        reservation: &waddle_xmpp::muc::RoomEffectReservation,
        initiator: &FullJid,
    ) -> (
        ResponseFrame,
        crate::room_effect_outbox::drain::RoomEffectCompletion,
    ) {
        let mut frames = crate::room_effect_outbox::drain::drain_reservation_inline(
            state,
            reservation,
            Some(initiator),
        )
        .await
        .expect("inline drain");
        assert_eq!(
            frames.len(),
            1,
            "one initiator frame should carry one completion"
        );
        let frame = frames.frames.pop().expect("inline frame");
        (ResponseFrame::from(frame.stanza), frame.completion)
    }

    async fn wait_for_room_effect_queue_depth(state: &Arc<WebSocketState>, expected: i64) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .queue_depth()
                    .await
                    .expect("queue depth")
                    == expected
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queue depth wait");
    }

    #[tokio::test]
    async fn post_upgrade_admission_revocation_sends_websocket_close() {
        let mut socket = UpgradeCloseSink::default();

        close_revoked_upgraded_socket(&mut socket).await;

        assert_eq!(socket.sent, vec![Message::Close(None)]);
        assert!(socket.closed);
    }

    #[tokio::test]
    async fn revocation_while_open_response_is_pending_closes_websocket_without_stream_error() {
        let state = super::super::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.begin_server_stream_open_response();
        let mut sender = RevokeWhileReadyPendingSink {
            lifecycle,
            start_send_called: false,
        };
        let mut reader = futures::stream::pending::<Result<Message, &'static str>>();

        let outcome = write_response_batch_with_admission(
            &mut sender,
            &mut reader,
            state.as_ref(),
            &mut conn,
            vec![websocket_stream_open_xml("example.com")],
            BatchSmPolicy::Record,
            BatchAuthority {
                permit: &permit,
                shutdown: &shutdown,
            },
        )
        .await;

        assert!(matches!(outcome, BatchWriteOutcome::AuthorityRevoked));
        assert!(
            !sender.start_send_called,
            "the server <open/> must not reach start_send after revocation"
        );
        assert!(
            !conn.has_committed_live_stream_open(),
            "an interrupted open batch must not permit a server-first stream error"
        );

        let mut closing_sender = UpgradeCloseSink::default();
        close_live_session_for_node_unavailable(&mut closing_sender, &conn).await;

        assert_eq!(closing_sender.sent, vec![]);
        assert!(closing_sender.closed, "the transport still closes");
    }

    #[tokio::test]
    async fn committed_open_keeps_system_shutdown_and_rfc7395_close_on_revocation() {
        let state = super::super::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.begin_server_stream_open_response();
        let mut sender = UpgradeCloseSink::default();
        let mut reader = futures::stream::pending::<Result<Message, &'static str>>();

        let outcome = write_response_batch_with_admission(
            &mut sender,
            &mut reader,
            state.as_ref(),
            &mut conn,
            vec![websocket_stream_open_xml("example.com")],
            BatchSmPolicy::Record,
            BatchAuthority {
                permit: &permit,
                shutdown: &shutdown,
            },
        )
        .await;
        assert!(matches!(outcome, BatchWriteOutcome::Continue));
        conn.commit_server_stream_open_response();
        assert!(conn.has_committed_live_stream_open());

        let mut closing_sender = UpgradeCloseSink::default();
        close_live_session_for_node_unavailable(&mut closing_sender, &conn).await;

        assert_eq!(
            closing_sender.sent,
            vec![
                Message::Text(build_system_shutdown_stream_error().into()),
                Message::Text(websocket_stream_close_xml().into()),
            ]
        );
        assert!(
            closing_sender.closed,
            "the transport closes after RFC 7395 close"
        );
    }

    #[tokio::test]
    async fn force_detach_writes_conflict_then_framing_close_only_for_committed_stream() {
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.begin_server_stream_open_response();
        conn.commit_server_stream_open_response();
        let mut sender = UpgradeCloseSink::default();

        close_live_session_for_force_detach(&mut sender, &conn, &permit, &shutdown).await;

        assert_eq!(
            sender.sent,
            vec![
                Message::Text(build_conflict_stream_error().into()),
                Message::Text(websocket_stream_close_xml().into()),
            ]
        );
        assert!(sender.closed, "force-detach completes the WebSocket close");
    }

    #[tokio::test]
    async fn force_detach_suppresses_conflict_before_live_stream_commit() {
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.begin_server_stream_open_response();
        let mut sender = UpgradeCloseSink::default();

        close_live_session_for_force_detach(&mut sender, &conn, &permit, &shutdown).await;

        assert!(sender.sent.is_empty());
        assert!(sender.closed);
    }

    #[tokio::test]
    async fn late_stale_force_detach_waiter_is_released_during_cross_node_cleanup() {
        let bare_jid = BareJid::from_str("late-stale@example.com").expect("valid bare jid");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut rx = Some(rx);
        let (ack_tx, ack_rx) = oneshot::channel();
        let (cancel, task) = start_late_force_detach_waiter_service(
            &mut rx,
            bare_jid.clone(),
            waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume,
        )
        .expect("late waiter service task");

        tx.send(waddle_xmpp::registry::ForceDetachRequest {
            origin: waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement,
            requester_bare_jid: bare_jid,
            ack: ack_tx,
        })
        .await
        .expect("send stale waiter");

        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), ack_rx)
                .await
                .expect("stale waiter ack completes")
                .expect("stale waiter ack"),
            waddle_xmpp::registry::ForceDetachOutcome::NotPersisted
        );

        cancel.cancel();
        assert!(
            task.await.expect("late waiter task joins").is_empty(),
            "stale retirements should be answered inline, not buffered"
        );
    }

    #[tokio::test]
    async fn late_owner_managed_force_detach_waiter_is_preserved_for_final_ack() {
        let bare_jid = BareJid::from_str("late-owner@example.com").expect("valid bare jid");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut rx = Some(rx);
        let (ack_tx, mut ack_rx) = oneshot::channel();
        let (cancel, task) = start_late_force_detach_waiter_service(
            &mut rx,
            bare_jid.clone(),
            waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume,
        )
        .expect("late waiter service task");

        tx.send(waddle_xmpp::registry::ForceDetachRequest {
            origin: waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
            requester_bare_jid: bare_jid,
            ack: ack_tx,
        })
        .await
        .expect("send owner-managed waiter");

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut ack_rx)
                .await
                .is_err(),
            "owner-managed cleanup must remain pending until final shutdown outcome"
        );

        cancel.cancel();
        let pending = task.await.expect("late waiter task joins");
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].origin,
            waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement
        );
    }

    #[tokio::test]
    async fn late_force_detach_identity_mismatch_is_rejected_during_cross_node_cleanup() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut rx = Some(rx);
        let (ack_tx, ack_rx) = oneshot::channel();
        let (cancel, task) = start_late_force_detach_waiter_service(
            &mut rx,
            BareJid::from_str("bound@example.com").expect("valid bound bare jid"),
            waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume,
        )
        .expect("late waiter service task");

        tx.send(waddle_xmpp::registry::ForceDetachRequest {
            origin: waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume,
            requester_bare_jid: BareJid::from_str("mismatch@example.com")
                .expect("valid mismatched bare jid"),
            ack: ack_tx,
        })
        .await
        .expect("send mismatched waiter");

        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), ack_rx)
                .await
                .expect("mismatched waiter ack completes")
                .expect("mismatched waiter ack"),
            waddle_xmpp::registry::ForceDetachOutcome::IdentityMismatch
        );

        cancel.cancel();
        assert!(
            task.await.expect("late waiter task joins").is_empty(),
            "mismatched requests must not survive into final ack handling"
        );
    }

    #[tokio::test]
    async fn late_stale_force_detach_waiter_is_collected_during_stale_retirement_origin() {
        let bare_jid = BareJid::from_str("late-stale-primary@example.com").expect("valid bare jid");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut rx = Some(rx);
        let (ack_tx, mut ack_rx) = oneshot::channel();
        let (cancel, task) = start_late_force_detach_waiter_service(
            &mut rx,
            bare_jid.clone(),
            waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement,
        )
        .expect("late waiter service task");

        tx.send(waddle_xmpp::registry::ForceDetachRequest {
            origin: waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement,
            requester_bare_jid: bare_jid,
            ack: ack_tx,
        })
        .await
        .expect("send late stale waiter");

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut ack_rx)
                .await
                .is_err(),
            "late stale retirement during stale-retirement cleanup must remain pending for final ack"
        );

        cancel.cancel();
        let pending = task.await.expect("late waiter task joins");
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].origin,
            waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement
        );
    }

    #[tokio::test]
    async fn late_owner_managed_force_detach_waiter_is_collected_during_owner_managed_origin() {
        let bare_jid = BareJid::from_str("late-owner-primary@example.com").expect("valid bare jid");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut rx = Some(rx);
        let (ack_tx, mut ack_rx) = oneshot::channel();
        let (cancel, task) = start_late_force_detach_waiter_service(
            &mut rx,
            bare_jid.clone(),
            waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
        )
        .expect("late waiter service task");

        tx.send(waddle_xmpp::registry::ForceDetachRequest {
            origin: waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
            requester_bare_jid: bare_jid,
            ack: ack_tx,
        })
        .await
        .expect("send late owner-managed waiter");

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut ack_rx)
                .await
                .is_err(),
            "late owner-managed retirement during owner-managed cleanup must remain pending for final ack"
        );

        cancel.cancel();
        let pending = task.await.expect("late waiter task joins");
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].origin,
            waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement
        );
    }

    #[tokio::test]
    async fn abandoned_inbound_slot_does_not_block_handoff_cleanup() {
        let state = super::super::tests::create_test_websocket_state().await;
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
            drain_ordered_relay_handoffs_before_cleanup(
                &state.deps.protocol.ingress_shadow,
                &mut rx,
                &mut conn,
            ),
        )
        .await
        .expect("abandoned sequence must not block cleanup");

        assert_eq!(conn.sm_state.get_inbound_count(), 0);
    }

    #[tokio::test]
    async fn revoked_consumed_handoff_is_abandoned_before_cleanup() {
        let state = super::super::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("handoff-revoked".to_string(), true, Some(300));
        let inbound_sequence = conn.sm_inbound_completion.reserve(&conn.sm_state);
        let completion = crate::server::routes::interpret::OrderedRelayHandoffCompletion {
            inbound_sequence,
            replies: Vec::new(),
        };
        let mut sender = UpgradeCloseSink::default();
        let mut receiver = futures::stream::pending::<Result<Message, &'static str>>();
        let (handoff_tx, mut handoff_rx) = tokio::sync::mpsc::unbounded_channel();
        conn.ordered_relay_handoff_tx = Some(handoff_tx);
        lifecycle.begin_fenced_recovery();

        assert!(
            !handle_ordered_relay_handoff_completion(
                &mut sender,
                &mut receiver,
                &state,
                &mut conn,
                completion,
                &permit,
                &shutdown,
            )
            .await
        );
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            drain_ordered_relay_handoffs_before_cleanup(
                &state.deps.protocol.ingress_shadow,
                &mut handoff_rx,
                &mut conn,
            ),
        )
        .await
        .expect("abandoned consumed completion must not block cleanup");

        assert_eq!(conn.sm_state.get_inbound_count(), 0);
        assert!(!conn.sm_inbound_completion.has_pending());
        assert!(sender.sent.is_empty());
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

        let state = super::super::tests::create_test_websocket_state().await;
        drain_ordered_relay_handoffs_before_cleanup(
            &state.deps.protocol.ingress_shadow,
            &mut rx,
            &mut conn,
        )
        .await;

        assert_eq!(
            conn.sm_state.get_inbound_count(),
            1,
            "the contiguous pre-hole stanza must be acknowledged before detach"
        );
        assert!(!conn.sm_inbound_completion.has_pending());
    }

    #[test]
    fn terminal_recovery_handoff_replies_bypass_the_capped_sm_queue() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let state = runtime.block_on(super::super::tests::create_test_websocket_state());
        let mut conn = WsConnState::new();
        conn.sm_state = waddle_xmpp::stream_management::StreamManagementState::with_config(8, 100);
        conn.sm_state
            .enable("terminal-handoff".to_string(), true, Some(300));
        for sequence in 1..=8 {
            let mut prefix = xmpp_parsers::message::Message::new(Some(
                "alice@example.com".parse().expect("recipient JID"),
            ));
            prefix.id = Some(xmpp_parsers::message::Id(sequence.to_string()));
            let _ = conn.sm_state.record_outbound(
                waddle_xmpp::parser::stanza_to_string(prefix).expect("serialize prefix"),
                waddle_xmpp::telemetry::attributes::SmEvictionPath::Batch,
            );
        }
        conn.begin_terminal_sm_recovery();
        let inbound_sequence = conn.sm_inbound_completion.reserve(&conn.sm_state);
        let reply = xmpp_parsers::message::Message::new(Some(
            "alice@example.com".parse().expect("recipient JID"),
        ));

        apply_ordered_relay_handoff_completion(
            &state.deps.protocol.ingress_shadow,
            &mut conn,
            crate::server::routes::interpret::OrderedRelayHandoffCompletion {
                inbound_sequence,
                replies: vec![Stanza::Message(reply)],
            },
        );

        assert_eq!(conn.sm_state.queue_len(), 8);
        assert_eq!(conn.sm_state.replay_gap_through(), None);
        assert_eq!(conn.terminal_sm_recovery.queue_len(), 1);
    }

    #[tokio::test]
    async fn replay_recording_defers_inline_room_effect_settlement_until_detach_persists() {
        let state = super::super::tests::create_test_websocket_state().await;
        let room_jid = replay_completion_room_jid();
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");
        let lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &room_jid).await;
        let reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (frame, completion) =
            drain_single_inline_completion_frame(state.as_ref(), &reservation, &initiator).await;

        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("deferred-replay-completion".to_string(), true, Some(300));
        record_response_batch_for_replay(
            &state,
            &mut conn,
            ResponseBatch::from_completion_frames(vec![(frame, completion.clone())]),
            BatchSmPolicy::Record,
        );

        assert_eq!(
            conn.sm_state.get_stanzas_to_resend(0).len(),
            1,
            "the initiator reply is retained for SM replay"
        );
        assert_eq!(
            state
                .deps
                .protocol
                .room_effect_outbox
                .queue_depth()
                .await
                .expect("queue depth"),
            1,
            "pre-detach replay recording must keep the leased completion until cleanup proves detach"
        );

        finalize_replay_recorded_completions(
            &state,
            &mut conn,
            cleanup::ConnectionShutdownOutcome::Detached,
        );
        wait_for_room_effect_queue_depth(&state, 0).await;
    }

    #[tokio::test]
    async fn replay_recording_not_persisted_cleanup_retains_inline_room_effect_reservations() {
        let state = super::super::tests::create_test_websocket_state().await;
        let room_jid = replay_completion_room_jid();
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");
        let lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &room_jid).await;
        let reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (frame, completion) =
            drain_single_inline_completion_frame(state.as_ref(), &reservation, &initiator).await;

        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("terminal-replay-completion".to_string(), true, Some(300));
        conn.begin_terminal_sm_recovery();
        record_response_batch_for_replay(
            &state,
            &mut conn,
            ResponseBatch::from_completion_frames(vec![(frame, completion.clone())]),
            BatchSmPolicy::Record,
        );

        assert_eq!(
            conn.terminal_sm_recovery.queue_len(),
            1,
            "the initiator reply is retained in the terminal recovery replay buffer"
        );
        finalize_replay_recorded_completions(
            &state,
            &mut conn,
            cleanup::ConnectionShutdownOutcome::NotPersisted,
        );
        assert_eq!(
            state
                .deps
                .protocol
                .room_effect_outbox
                .queue_depth()
                .await
                .expect("queue depth"),
            1,
            "a NotPersisted cleanup must retain replay-only completions for retry"
        );
        assert!(
            crate::room_effect_outbox::drain::complete_after_write(state.as_ref(), &completion)
                .await
                .expect("complete retained completion"),
            "the retained completion must still be available for retry after NotPersisted cleanup"
        );
    }

    #[tokio::test]
    async fn not_persisted_cleanup_keeps_replay_only_tail_after_written_prefix_settles() {
        let state = super::super::tests::create_test_websocket_state().await;
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");

        let first_room_jid = replay_completion_room_jid();
        let first_lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &first_room_jid)
                .await;
        let first_reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            first_lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (first_frame, first_completion) =
            drain_single_inline_completion_frame(state.as_ref(), &first_reservation, &initiator)
                .await;

        let second_room_jid = other_replay_completion_room_jid();
        let second_lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &second_room_jid)
                .await;
        let second_reservation = enqueue_inline_config_reservation_for_room_replay_completion(
            state.as_ref(),
            second_room_jid,
            second_lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (second_frame, second_completion) =
            drain_single_inline_completion_frame(state.as_ref(), &second_reservation, &initiator)
                .await;

        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("mixed-prefix-and-tail".to_string(), true, Some(300));

        settle_batch_completions(
            &state,
            BatchCompletionOutcome::RetainedForRecovery,
            true,
            &[0],
            response_batch_completion_frames(ResponseBatch::from_completion_frames(vec![(
                first_frame,
                first_completion,
            )])),
        );
        wait_for_room_effect_queue_depth(&state, 1).await;

        record_response_batch_for_replay(
            &state,
            &mut conn,
            ResponseBatch::from_completion_frames(vec![(second_frame, second_completion.clone())]),
            BatchSmPolicy::Record,
        );
        assert_eq!(conn.pending_replay_completions.len(), 1);

        finalize_replay_recorded_completions(
            &state,
            &mut conn,
            cleanup::ConnectionShutdownOutcome::NotPersisted,
        );

        assert_eq!(
            state
                .deps
                .protocol
                .room_effect_outbox
                .queue_depth()
                .await
                .expect("queue depth"),
            1,
            "the live-written prefix must settle while the replay-only tail stays leased"
        );
        assert!(
            crate::room_effect_outbox::drain::complete_after_write(
                state.as_ref(),
                &second_completion
            )
            .await
            .expect("complete retained replay-only tail"),
            "the replay-only tail must remain available for retry after NotPersisted cleanup"
        );
    }

    #[tokio::test]
    async fn live_writer_partial_sm_acceptance_completes_only_recorded_room_effects() {
        let state = super::super::tests::create_test_websocket_state().await;
        let room_jid = replay_completion_room_jid();
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");
        let lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &room_jid).await;
        let first_reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let other_room_jid = other_replay_completion_room_jid();
        let second_lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &other_room_jid)
                .await;
        let second_reservation = enqueue_inline_config_reservation_for_room_replay_completion(
            state.as_ref(),
            other_room_jid,
            second_lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (first_frame, first_completion) =
            drain_single_inline_completion_frame(state.as_ref(), &first_reservation, &initiator)
                .await;
        let (second_frame, second_completion) =
            drain_single_inline_completion_frame(state.as_ref(), &second_reservation, &initiator)
                .await;

        let mut responses =
            ResponseBatch::from_completion_frames(vec![(first_frame, first_completion)]);
        responses
            .frames
            .push(ResponseFrame::from(websocket_stream_close_element()));
        responses.append_batch(ResponseBatch::from_completion_frames(vec![(
            second_frame,
            second_completion.clone(),
        )]));

        settle_batch_completions(
            &state,
            BatchCompletionOutcome::Delivered,
            true,
            &[0],
            response_batch_completion_frames(responses),
        );

        wait_for_room_effect_queue_depth(&state, 1).await;

        assert!(
            crate::room_effect_outbox::drain::complete_after_write(
                state.as_ref(),
                &second_completion
            )
            .await
            .expect("complete retained completion"),
            "the unrecorded completion must still be retained for retry"
        );
    }

    #[tokio::test]
    async fn resumable_partial_acceptance_retains_multi_frame_completion_for_retry() {
        let state = super::super::tests::create_test_websocket_state().await;
        let room_jid = replay_completion_room_jid();
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");
        let lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &room_jid).await;
        let reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (frame, completion) =
            drain_single_inline_completion_frame(state.as_ref(), &reservation, &initiator).await;

        let mut responses =
            ResponseBatch::from_completion_frames(vec![(frame.clone(), completion.clone())]);
        responses
            .frames
            .push(ResponseFrame::from(websocket_stream_close_element()));
        responses.append_batch(ResponseBatch::from_completion_frames(vec![(
            frame,
            completion.clone(),
        )]));

        settle_batch_completions(
            &state,
            BatchCompletionOutcome::RetainedForRecovery,
            true,
            &[0],
            response_batch_completion_frames(responses),
        );

        assert_eq!(
            state
                .deps
                .protocol
                .room_effect_outbox
                .queue_depth()
                .await
                .expect("queue depth"),
            1,
            "a partially accepted sibling frame set must retain the shared completion"
        );
        assert!(
            crate::room_effect_outbox::drain::complete_after_write(state.as_ref(), &completion)
                .await
                .expect("complete retained completion"),
            "the shared completion must still be retained for retry"
        );
    }

    #[tokio::test]
    async fn non_resumable_replay_recording_retains_inline_room_effect_reservations_for_retry() {
        let state = super::super::tests::create_test_websocket_state().await;
        let room_jid = replay_completion_room_jid();
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");
        let lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &room_jid).await;
        let reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (frame, completion) =
            drain_single_inline_completion_frame(state.as_ref(), &reservation, &initiator).await;

        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("non-resumable-replay-completion".to_string(), false, None);
        record_response_batch_for_replay(
            &state,
            &mut conn,
            ResponseBatch::from_completion_frames(vec![(frame, completion.clone())]),
            BatchSmPolicy::Record,
        );

        assert_eq!(
            state
                .deps
                .protocol
                .room_effect_outbox
                .queue_depth()
                .await
                .expect("queue depth"),
            1,
            "non-resumable replay recording must retain the leased completion for retry"
        );
        assert!(
            crate::room_effect_outbox::drain::complete_after_write(state.as_ref(), &completion)
                .await
                .expect("complete retained completion"),
            "the recorded-but-non-resumable completion must still be retained for retry"
        );
    }

    #[tokio::test]
    async fn resumable_replay_gap_recording_retains_inline_room_effect_reservations_for_retry() {
        let state = super::super::tests::create_test_websocket_state().await;
        let room_jid = replay_completion_room_jid();
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");
        let lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &room_jid).await;
        let reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (frame, completion) =
            drain_single_inline_completion_frame(state.as_ref(), &reservation, &initiator).await;

        let mut conn = WsConnState::new();
        conn.sm_state = waddle_xmpp::stream_management::StreamManagementState::with_config(1, 100);
        conn.sm_state.enable(
            "resumable-replay-gap-completion".to_string(),
            true,
            Some(300),
        );
        let _ = conn.sm_state.record_outbound(
            "<message xmlns='jabber:client' id='already-owned'><body>prefix</body></message>"
                .to_string(),
            waddle_xmpp::telemetry::attributes::SmEvictionPath::DirectOutbound,
        );
        record_response_batch_for_replay(
            &state,
            &mut conn,
            ResponseBatch::from_completion_frames(vec![(frame, completion.clone())]),
            BatchSmPolicy::Record,
        );

        assert_eq!(
            conn.sm_state.replay_gap_through(),
            Some(1),
            "overflowing the live resumable queue must mark the replay gap"
        );
        assert_eq!(
            state
                .deps
                .protocol
                .room_effect_outbox
                .queue_depth()
                .await
                .expect("queue depth"),
            1,
            "a gapped replay tail must retain the leased completion for retry"
        );
        assert!(
            crate::room_effect_outbox::drain::complete_after_write(state.as_ref(), &completion)
                .await
                .expect("complete retained completion"),
            "a replay-gap frame must not settle the completion as accepted work"
        );
    }

    #[tokio::test]
    async fn authority_revoked_resumable_recording_settles_only_recorded_completions() {
        let state = super::super::tests::create_test_websocket_state().await;
        let room_jid = replay_completion_room_jid();
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");
        let lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &room_jid).await;
        let first_reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let other_room_jid = other_replay_completion_room_jid();
        let second_lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &other_room_jid)
                .await;
        let second_reservation = enqueue_inline_config_reservation_for_room_replay_completion(
            state.as_ref(),
            other_room_jid,
            second_lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (first_frame, first_completion) =
            drain_single_inline_completion_frame(state.as_ref(), &first_reservation, &initiator)
                .await;
        let (second_frame, second_completion) =
            drain_single_inline_completion_frame(state.as_ref(), &second_reservation, &initiator)
                .await;

        let mut responses =
            ResponseBatch::from_completion_frames(vec![(first_frame, first_completion)]);
        responses
            .frames
            .push(ResponseFrame::from(websocket_stream_close_element()));
        responses.append_batch(ResponseBatch::from_completion_frames(vec![(
            second_frame,
            second_completion.clone(),
        )]));

        settle_batch_completions(
            &state,
            BatchCompletionOutcome::RetainedForRecovery,
            true,
            &[0],
            response_batch_completion_frames(responses),
        );

        wait_for_room_effect_queue_depth(&state, 1).await;

        assert!(
            crate::room_effect_outbox::drain::complete_after_write(
                state.as_ref(),
                &second_completion
            )
            .await
            .expect("complete retained completion"),
            "only the unrecorded authority-revoked completion must remain for retry"
        );
    }

    #[tokio::test]
    async fn batch_completion_selection_matrix_covers_outcome_resumable_acceptance_and_siblings() {
        let state = super::super::tests::create_test_websocket_state().await;
        let room_jid = replay_completion_room_jid();
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");
        let lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &room_jid).await;
        let first_reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (first_frame, first_completion) =
            drain_single_inline_completion_frame(state.as_ref(), &first_reservation, &initiator)
                .await;

        for outcome in [
            BatchCompletionOutcome::Delivered,
            BatchCompletionOutcome::RetainedForRecovery,
            BatchCompletionOutcome::RetryOnly,
        ] {
            for resumable in [false, true] {
                for partial_acceptance in [false, true] {
                    for multi_frame_completion in [false, true] {
                        let responses = if multi_frame_completion {
                            let mut responses = ResponseBatch::from_completion_frames(vec![(
                                first_frame.clone(),
                                first_completion.clone(),
                            )]);
                            responses.append_batch(ResponseBatch::from_completion_frames(vec![(
                                first_frame.clone(),
                                first_completion.clone(),
                            )]));
                            responses
                        } else {
                            ResponseBatch::from_completion_frames(vec![(
                                first_frame.clone(),
                                first_completion.clone(),
                            )])
                        };
                        let accepted_indices: Vec<_> = if partial_acceptance {
                            if multi_frame_completion {
                                vec![0]
                            } else {
                                Vec::new()
                            }
                        } else if multi_frame_completion {
                            vec![0, 1]
                        } else {
                            vec![0]
                        };
                        let (selected, retained) = select_batch_completions_to_complete(
                            outcome,
                            resumable,
                            &accepted_indices,
                            response_batch_completion_frames(responses),
                        );
                        let should_complete = !partial_acceptance
                            && (matches!(outcome, BatchCompletionOutcome::Delivered)
                                || (resumable
                                    && matches!(
                                        outcome,
                                        BatchCompletionOutcome::RetainedForRecovery
                                    )));
                        assert_eq!(
                            selected.len(),
                            usize::from(should_complete),
                            "outcome={outcome:?} resumable={resumable} partial={partial_acceptance} multi={multi_frame_completion}"
                        );
                        assert_eq!(
                            retained,
                            usize::from(!should_complete),
                            "outcome={outcome:?} resumable={resumable} partial={partial_acceptance} multi={multi_frame_completion}"
                        );
                        if should_complete {
                            assert_eq!(selected[0].key, first_completion.key);
                        }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn replay_recording_partial_terminal_acceptance_retains_all_room_effects_until_cleanup() {
        let state = super::super::tests::create_test_websocket_state().await;
        let room_jid = replay_completion_room_jid();
        let initiator: FullJid = "alice@example.com/device".parse().expect("initiator JID");
        let lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &room_jid).await;
        let first_reservation = enqueue_inline_config_reservation_for_replay_completion(
            state.as_ref(),
            lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let other_room_jid = other_replay_completion_room_jid();
        let second_lifecycle =
            create_owned_room_and_lifecycle_for_replay_completion(state.as_ref(), &other_room_jid)
                .await;
        let second_reservation = enqueue_inline_config_reservation_for_room_replay_completion(
            state.as_ref(),
            other_room_jid,
            second_lifecycle,
            waddle_xmpp::muc::RoomRevision::initial(),
            &initiator,
        )
        .await;
        let (first_frame, first_completion) =
            drain_single_inline_completion_frame(state.as_ref(), &first_reservation, &initiator)
                .await;
        let (second_frame, second_completion) =
            drain_single_inline_completion_frame(state.as_ref(), &second_reservation, &initiator)
                .await;

        let mut responses =
            ResponseBatch::from_completion_frames(vec![(first_frame, first_completion.clone())]);
        responses
            .frames
            .push(ResponseFrame::from(websocket_stream_close_element()));
        responses.append_batch(ResponseBatch::from_completion_frames(vec![(
            second_frame,
            second_completion.clone(),
        )]));

        let mut conn = WsConnState::new();
        conn.sm_state.enable(
            "partial-terminal-replay-completion".to_string(),
            true,
            Some(300),
        );
        conn.begin_terminal_sm_recovery();
        for sequence in 0..(TERMINAL_RECOVERY_QUEUE_CAP - 1) {
            let mut prefix =
                xmpp_parsers::message::Message::new(Some(jid::Jid::from(initiator.clone())));
            prefix.id = Some(xmpp_parsers::message::Id(format!(
                "partial-terminal-prefix-{sequence}"
            )));
            conn.record_terminal_recovery_outbound(
                waddle_xmpp::parser::stanza_to_string(prefix).expect("serialize terminal prefix"),
            );
        }

        record_response_batch_for_replay(&state, &mut conn, responses, BatchSmPolicy::Record);

        assert_eq!(
            state
                .deps
                .protocol
                .room_effect_outbox
                .queue_depth()
                .await
                .expect("queue depth"),
            2,
            "pre-detach terminal replay recording must keep both recorded and unrecorded completions leased"
        );

        assert_eq!(
            conn.terminal_sm_recovery.queue_len(),
            TERMINAL_RECOVERY_QUEUE_CAP,
            "terminal recovery should accept only one more completion frame"
        );
        assert!(
            crate::room_effect_outbox::drain::complete_after_write(
                state.as_ref(),
                &first_completion
            )
            .await
            .expect("complete retained recorded completion"),
            "the terminally recorded completion must stay retained until cleanup settles it"
        );
        assert!(
            crate::room_effect_outbox::drain::complete_after_write(
                state.as_ref(),
                &second_completion
            )
            .await
            .expect("complete retained completion"),
            "the terminally unrecorded completion must still be retained for retry"
        );
    }

    #[tokio::test]
    async fn terminal_recovery_cap_discards_parked_mam_frames_and_promotes_prefix() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = super::super::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let jid: FullJid = "alice@example.com/terminal-cap".parse().expect("jid");
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let owner = state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx.clone());
        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::ready(jid.clone(), false);
        conn.registry_owner = Some(owner);
        conn.sm_state
            .enable("terminal-cap".to_string(), true, Some(300));
        conn.begin_terminal_sm_recovery();
        for sequence in 0..TERMINAL_RECOVERY_QUEUE_CAP {
            let mut prefix = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.clone())));
            prefix.id = Some(xmpp_parsers::message::Id(format!(
                "terminal-cap-{sequence}"
            )));
            let _ = conn.terminal_sm_recovery.record_outbound(
                waddle_xmpp::parser::stanza_to_string(prefix).expect("serialize terminal prefix"),
                waddle_xmpp::telemetry::attributes::SmEvictionPath::ReplayTail,
            );
        }
        conn.deferred_inbound.extend((0..96).map(|sequence| {
            let mam_query =
                xmpp_parsers::minidom::Element::builder("query", waddle_xmpp_core::mam::MAM_NS)
                    .build();
            let iq = xmpp_parsers::iq::Iq::Set {
                from: None,
                to: None,
                id: format!("mam-{sequence}"),
                payload: mam_query,
            };
            axum::extract::ws::Utf8Bytes::from(
                waddle_xmpp::parser::stanza_to_string(iq).expect("serialize MAM query"),
            )
        }));

        process_deferred_inbound_after_transport_loss(
            "example.com",
            &state,
            &mut conn,
            &permit,
            &shutdown,
        )
        .await;

        assert!(
            conn.deferred_inbound.is_empty(),
            "all parked MAM frames are discarded"
        );
        assert_eq!(
            conn.terminal_sm_recovery.queue_len(),
            TERMINAL_RECOVERY_QUEUE_CAP
        );
        assert_eq!(conn.terminal_sm_recovery.replay_gap_through(), None);
        for path in ["batch", "replay_tail", "direct_outbound", "detach_drain"] {
            assert_eq!(
                metrics.counter_sum("xmpp.sm.unacked_evicted", &[("path", path)]),
                None,
                "terminal recovery cap must not evict recorded stanzas on path {path}"
            );
        }
        assert_eq!(
            cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await,
            super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
        );
        assert!(
            state
                .deps
                .protocol
                .sm_session_registry
                .peek_session("terminal-cap")
                .await
                .expect("registry lookup")
                .is_none(),
            "terminal recovery promotes rather than retaining a resumable snapshot"
        );
        let pending = state
            .deps
            .protocol
            .pending_delivery_storage
            .list(&jid.to_bare())
            .await
            .expect("list promoted rows");
        assert!(
            pending.iter().any(|row| {
                matches!(
                    &row.payload,
                    waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                        if message.id.as_ref().is_some_and(|id| id.0 == "terminal-cap-0")
                )
            }),
            "recorded prefix is promoted"
        );
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
        let ingress_shadow = crate::ingress_shadow::IngressShadowHandle::disabled();
        let mut drain = Box::pin(drain_ordered_relay_handoffs_before_cleanup(
            &ingress_shadow,
            &mut rx,
            &mut conn,
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

        let state = super::super::tests::create_test_websocket_state().await;
        drain_ordered_relay_handoffs_before_cleanup(
            &state.deps.protocol.ingress_shadow,
            &mut rx,
            &mut conn,
        )
        .await;

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

        assert_eq!(discard_deferred_inbound(&mut conn), 2);
        assert!(conn.deferred_inbound.is_empty());
        assert_eq!(conn.sm_state.get_inbound_count(), 0);
    }

    #[tokio::test]
    async fn revoked_generation_drops_deferred_transport_loss_suffix_without_advancing_h() {
        let state = super::super::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("deferred-revoked".to_string(), true, Some(300));
        let mut first = xmpp_parsers::message::Message::new(Some(
            "bob@example.com".parse().expect("recipient JID"),
        ));
        first.id = Some(xmpp_parsers::message::Id("first".to_string()));
        let mut later = xmpp_parsers::message::Message::new(Some(
            "carol@example.com".parse().expect("recipient JID"),
        ));
        later.id = Some(xmpp_parsers::message::Id("later".to_string()));
        conn.deferred_inbound.extend([
            axum::extract::ws::Utf8Bytes::from(
                waddle_xmpp::parser::stanza_to_string(first).expect("serialize first message"),
            ),
            axum::extract::ws::Utf8Bytes::from(
                waddle_xmpp::parser::stanza_to_string(later).expect("serialize later message"),
            ),
        ]);
        lifecycle.begin_fenced_recovery();

        process_deferred_inbound_after_transport_loss(
            "example.com",
            &state,
            &mut conn,
            &permit,
            &shutdown,
        )
        .await;

        assert!(conn.deferred_inbound.is_empty());
        assert_eq!(conn.sm_state.get_inbound_count(), 0);
        assert!(conn.sm_state.get_stanzas_to_resend(0).is_empty());
    }

    #[test]
    fn closing_phase_appends_websocket_stream_close_frame() {
        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::closing(None);
        let mut responses = vec![ResponseFrame::from(
            Element::builder("failed", waddle_xmpp::stream_management::SM_NS).build(),
        )];

        ensure_websocket_stream_close_for_closing_phase(&conn, &mut responses);

        assert_eq!(responses.len(), 2);
        let close = Element::from_str(&responses[1].clone().into_serialized_xml())
            .expect("close frame xml");
        assert_eq!(close.name(), "close");
        assert_eq!(close.ns(), "urn:ietf:params:xml:ns:xmpp-framing");
    }

    #[test]
    fn closing_phase_does_not_duplicate_websocket_stream_close_frame() {
        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::closing(None);
        let mut responses = vec![ResponseFrame::from(websocket_stream_close_element())];

        ensure_websocket_stream_close_for_closing_phase(&conn, &mut responses);

        assert_eq!(responses.len(), 1);
    }
}
