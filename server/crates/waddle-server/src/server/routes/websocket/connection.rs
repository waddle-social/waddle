use super::batch_write::{
    write_ingress_response_batch_with_admission, write_response_batch_with_admission,
    BatchAuthority, BatchSmPolicy, BatchWriteOutcome,
};
use super::cleanup::cleanup_connection_shutdown;
use super::frame::{
    handle_xmpp_frame_with_admission, ResponseBatch, ResponseFrame, StreamErrorFrame,
};
use super::interpret_loop::build_interpret_deps;
use super::outbound::{handle_outbound_stanza, OutboundAuthority};
use super::registration::{
    register_bound_connection_after_frame_with_admission, RegistrationAfterFrame,
};
use super::replay::drive_interpret_loop;
use super::send::{
    close_ws_connection, send_ws_message, send_ws_message_with_authority, send_ws_text_frames,
    send_ws_text_frames_with_authority, AuthoritySendOutcome, InterruptibleSink,
};
use super::session_init::build_internal_server_error_stream_error;
use super::state::{InboundFrameTerminal, WsConnState, TERMINAL_RECOVERY_QUEUE_CAP};
use super::stream_management::SmRegistrationFinalization;
use super::timers::TransportTimers;
use super::transport_xml::{
    build_conflict_stream_error, build_system_shutdown_stream_error,
    websocket_stream_close_element, websocket_stream_close_xml,
};
use super::{
    batch_write, cleanup, debug, error, get, info, mpsc, warn, Arc, ConnectionPhase, InboundEvent,
    Message, OutboundStanza, Response, Router, Sink, Stanza, State, StreamExt, WebSocket,
    WebSocketState, WebSocketUpgrade,
};
use crate::clustering::NodeAdmissionPermit;
use crate::room_effect_outbox::drain::RoomEffectCompletion;
use crate::server::routes::interpret::{
    OrderedRelayHandoffCompletion, OrderedRelayHandoffReceiver,
};
use crate::websocket_transport::{
    InputError, IoCustodianInventory, IoCustodianObserver, LegacySocketAdmission, Limits,
    LiveSocketCleanup, LiveSocketTransport, PumpStop, SocketAdmissionError, StopReason,
    TransportLimits, WebSocketInput, WebSocketMessageSink,
};
use axum::extract::{FromRequest, Request};
use axum::response::IntoResponse;
use futures::FutureExt;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::panic::AssertUnwindSafe;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::protocol::TimerId;
use waddle_xmpp::registry::{LocalSocketIo, SocketOutboundEndpoint, SocketOutboundReceiver};
use waddle_xmpp::stream_management::SmRequest;

mod binding;
mod binding_messages;
#[cfg(test)]
mod binding_publisher_tests;
mod coordinator_publisher;
mod live_actor;
mod retirement;

#[cfg(test)]
pub(super) use binding::PreparedBinding;
pub(super) use binding::{
    BindingConfirmation, BindingLedger, BindingPublication, BindingPublisher, BindingRefusal,
    BindingRetirement, BindingStage, BindingStepReply, InstallOutcome, PrepareBindingReply,
};
#[cfg(test)]
pub(super) use binding_messages::ConnectionActorOperationId;
pub(super) use coordinator_publisher::CoordinatorResume;

pub(super) use retirement::{
    fence_unjoined_transfer, observe_pump_termination, PumpTermination, RetirementCause,
    RetirementEntry, RetirementFence, RetirementRefusal, TRANSFER_JOIN_DEADLINE,
};
use retirement::{OwedClose, Retire};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchCompletionOutcome {
    Delivered,
    RetainedForRecovery,
    RetryOnly,
}

async fn close_unstarted_upgraded_socket<S, E>(socket: &mut S)
where
    S: futures::Sink<Message, Error = E> + Unpin,
    E: super::send::WebSocketWriteError,
{
    let close = async {
        let _ = send_ws_message(
            socket,
            Message::Close(None),
            "Failed to send WebSocket close frame before XMPP start",
        )
        .await;
        let _ = close_ws_connection(socket, "Failed to close WebSocket before XMPP start").await;
    };
    if tokio::time::timeout(SHUTDOWN_CLOSE_TIMEOUT, close)
        .await
        .is_err()
    {
        warn!("Timed out closing WebSocket before XMPP start");
    }
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
    let gate = match state.deps.app_state.websocket_admission_gate() {
        Ok(gate) => gate,
        Err(error) => {
            info!(%error, "Rejecting XMPP WebSocket upgrade: node is not serving");
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    info!("XMPP WebSocket connection request");

    #[cfg(feature = "test-support")]
    if let Some(barrier) = &state
        .deps
        .app_state
        .websocket_admission_test_barriers
        .before_parse
    {
        barrier.arrive().await;
    }
    let ws = match WebSocketUpgrade::from_request(request, &state).await {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };

    // Parsing must succeed before a bounded slot is held. The exact lifecycle,
    // identity source and per-process inventory all come from this AppState.
    let admission = match gate.reserve(state.deps.shutdown.stop_token()) {
        Ok(admission) => admission,
        Err(error) => {
            info!(%error, "Rejecting XMPP WebSocket upgrade: admission failed");
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    #[cfg(feature = "test-support")]
    if let Some(barrier) = &state
        .deps
        .app_state
        .websocket_admission_test_barriers
        .before_response
    {
        barrier.arrive().await;
    }
    if let Err(reason) = admission.revalidate() {
        info!(
            ?reason,
            "Rejecting XMPP WebSocket upgrade: admission was revoked"
        );
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    ws.protocols(["xmpp"])
        .max_message_size(WEBSOCKET_MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            #[cfg(feature = "test-support")]
            let waiter_control = state
                .deps
                .app_state
                .websocket_admission_test_barriers
                .callback_waiter
                .clone();
            let waiter = LiveWebSocketConnection {
                socket: Box::new(socket),
                state,
                admission,
                _connection_guard: connection_guard,
            }
            .spawn();
            #[cfg(feature = "test-support")]
            let waiter = if waiter_control
                .as_ref()
                .is_some_and(|control| control.drop_before_poll)
            {
                drop(waiter);
                if let Some(control) = &waiter_control {
                    control.waiter_dropped.cancel();
                }
                None
            } else {
                Some(waiter)
            };
            #[cfg(not(feature = "test-support"))]
            let waiter = Some(waiter);

            async move {
                #[cfg(feature = "test-support")]
                if let Some(control) = waiter_control {
                    if let Some(waiter) = waiter {
                        let mut waiter = Box::pin(waiter);
                        tokio::select! {
                            _ = control.drop_waiter.cancelled() => {
                                drop(waiter);
                                control.waiter_dropped.cancel();
                            }
                            _ = &mut waiter => {}
                        }
                    }
                    return;
                }
                if let Some(waiter) = waiter {
                    waiter.await;
                }
            }
        })
}

/// Size of the outbound message channel buffer
const OUTBOUND_CHANNEL_SIZE: usize = 256;

/// Dropping an aborted connection task must release resume callers too. The
/// normal path finishes this control after cleanup; repeated finish is safe.
struct LiveResumeOwnerGuard(Arc<waddle_xmpp::registry::LiveResumeControl>);

impl Drop for LiveResumeOwnerGuard {
    fn drop(&mut self) {
        self.0.finish();
    }
}

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
const FORCE_DETACH_CONFLICT_CLOSE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(250);

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
pub(super) struct RegistrationChannels<'a> {
    pub(super) pending_tx: &'a mut Option<SocketOutboundEndpoint>,
    pub(super) outbound_receiver: Option<&'a SocketOutboundReceiver>,
    pub(super) force_detach_rx:
        &'a mut Option<mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>>,
}

/// An inbound half that can bring forward a frame its socket already holds.
///
/// A reader that polls its own socket has such a frame ready on the first
/// poll, so the default does nothing. An input fed by a pump task has to ask
/// the pump and wait for its answer.
pub(super) trait SettleInbound {
    fn settle_inbound(&mut self) -> impl std::future::Future<Output = ()> + Send {
        std::future::ready(())
    }
}

impl<T: SettleInbound + Send + ?Sized> SettleInbound for &mut T {
    fn settle_inbound(&mut self) -> impl std::future::Future<Output = ()> + Send {
        (**self).settle_inbound()
    }
}

/// Scripted readers in unit tests are polled directly.
#[cfg(test)]
impl<T> SettleInbound for futures::stream::Pending<T> {}

#[cfg(test)]
impl<A, B> SettleInbound for futures::stream::Chain<A, B> {}

#[cfg(test)]
impl<F> SettleInbound for futures::stream::PollFn<F> {}

impl<E: Send> SettleInbound for WebSocketInput<E> {
    async fn settle_inbound(&mut self) {
        self.settle().await;
    }
}

pub(super) struct ConnectionIo<'a, S, R> {
    pub(super) sender: &'a mut S,
    pub(super) receiver: &'a mut R,
}

#[derive(Clone, Copy)]
pub(super) struct FrameAuthority<'a> {
    pub(super) permit: &'a NodeAdmissionPermit,
    pub(super) shutdown: &'a tokio_util::sync::CancellationToken,
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

/// Owns an upgraded socket and its connection lifecycle in one task. Once the
/// socket has started, the task runs the connection actor, which holds the
/// live state until it stops.
struct LiveWebSocketConnection {
    // Declared first: an unstarted socket drops before the capacity and the
    // drain guard below are released.
    socket: Box<WebSocket>,
    state: Arc<WebSocketState>,
    // Keep capacity reserved through connection cleanup and release it before
    // the drain guard, after the physical socket and connection state drop.
    admission: LegacySocketAdmission,
    // Minted in the upgrade handler and held for the connection's whole
    // lifetime — including the detach/cleanup below — so the ecdysis
    // drain (issue #1091) only completes once every live session has
    // been closed AND its SM state handed to the session registry for
    // Q6 promotion.
    _connection_guard: waddle_ecdysis::ConnectionGuard,
}

type LiveSocketIo = LocalSocketIo<
    InterruptibleSink<WebSocketMessageSink<axum::Error>>,
    WebSocketInput<axum::Error>,
>;

/// The largest WebSocket message the upgrade accepts from a peer. The
/// WebSocket layer refuses a larger one before the reader pump sees it. The
/// value is the layer's own default, stated here because the pump queue
/// bounds below are derived from it.
const WEBSOCKET_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Outbound pump queue for one connection. The loop writes one frame and
/// waits for its flush, so the count only has to cover a frame in flight plus
/// the short terminal sequence.
const TRANSPORT_OUTBOUND_FRAMES: usize = 4;
/// Byte bound of the outbound pump queue, which is also the largest single
/// frame it admits. One data frame is in flight at a time, and a serialized
/// stanza larger than the largest message a peer endpoint with the same
/// WebSocket limit accepts could not be delivered. The other slots carry the
/// terminal sequence (a stream error, the framing close and the WebSocket
/// close), each far smaller than the protocol's frame limit, which is what
/// they are allowed here. A frame that does not fit the remaining budget
/// waits for it; only a frame larger than the whole budget is refused, with
/// `AdmissionError::Overloaded`, and that ends the connection as a failed
/// write, leaving a recorded stanza in the stream-management queue.
const TRANSPORT_OUTBOUND_QUEUE_BYTES: usize =
    WEBSOCKET_MAX_MESSAGE_BYTES + (TRANSPORT_OUTBOUND_FRAMES - 1) * super::MAX_FRAME_SIZE;
/// Inbound pump queue for one connection. The reader pump reads on demand, so
/// at most one frame waits here.
const TRANSPORT_INBOUND_FRAMES: usize = 1;
/// Byte bound of the inbound pump queue: its one frame at the largest size
/// the WebSocket layer delivers. The protocol's own frame limit is far lower,
/// but a frame between the two must still reach the protocol handler, which
/// answers it with a stream error.
const TRANSPORT_INBOUND_QUEUE_BYTES: usize = TRANSPORT_INBOUND_FRAMES * WEBSOCKET_MAX_MESSAGE_BYTES;
/// How long the owner task waits, once the owner has finished, for the pump
/// custodian to report both pumps joined before leaving the inventory slot in
/// place.
const TRANSPORT_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// The same wait before the fatal fence on the owner-panic path.
const TRANSPORT_PANIC_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
/// How long after node or identity revocation the pump custodian stops the
/// pumps on its own. The owner normally stops them first by dropping its
/// transport. This backstop has to outlast an owner that is inside a handler
/// at revocation and then writes its bounded system-shutdown close, or that
/// close would be lost.
const TRANSPORT_RETIREMENT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(
    super::frame_backstop::STANZA_HANDLER_WEDGE_TIMEOUT.as_secs()
        + SHUTDOWN_CLOSE_TIMEOUT.as_secs()
        + 5,
);

/// Everything the live owner holds for one started connection.
struct LiveSocketState {
    // Fields drop in declaration order when the owner is cancelled, unwinds
    // or abandons the connection without settling, and that order is part of
    // the connection's contract:
    // - the channels and timers drop first;
    // - then both physical halves, which stops the pumps;
    // - then the transport cleanup guard. It reports that the owner holds
    //   nothing more for this connection, so it must never complete while
    //   `socket_io` is still held. On a normal exit `settle` has already
    //   consumed `socket_io`, which keeps the same order;
    // - then the resume owner guard and the connection state.
    outbound_rx: SocketOutboundReceiver,
    // Set when the registry slot was replaced by a newer connection for the
    // same FullJid. Cleanup then preserves the newcomer's route while sweeping
    // only this connection's occupancy generation.
    superseded: bool,
    timers: TransportTimers,
    handoff_rx: OrderedRelayHandoffReceiver,
    socket_io: Option<LiveSocketIo>,
    _transport_cleanup: LiveSocketCleanup,
    _live_resume_owner_guard: LiveResumeOwnerGuard,
    live_resume_control: Arc<waddle_xmpp::registry::LiveResumeControl>,
    conn: WsConnState,
    // Requests this connection accepted (identity-matched). Each is
    // acknowledged only after `cleanup_connection_shutdown` has run the
    // normal XEP-0198 detach-for-resume persistence: the asking lifecycle
    // must never proceed until the detach-flush this node performs has
    // landed.
    pending_force_detach: Vec<waddle_xmpp::registry::ForceDetachRequest>,
    // The cross-node resume live-steal handshake's force-detach receiver
    // (ADR-0017 Phase 3 Slice 6). `None` until this connection registers: the
    // registry mints a fresh channel pair per `ConnectionEntry`, and
    // `handle_inbound_text` takes the receiver half right after registration
    // succeeds, mirroring the `pending_tx` hand-off.
    force_detach_rx: Option<mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>>,
    // The sending half of the outbound channel. Registration hands it to the
    // `ConnectionRegistry` and leaves `None` here, making the registry the
    // sole holder of the opaque endpoint. When another session registers for
    // the same FullJid the registry replaces its entry and `outbound_rx`
    // yields `None`, which is how replacement is detected.
    pending_tx: Option<SocketOutboundEndpoint>,
    turn: TurnGates,
    // Fires when the transport pumps stop. Each wait polls it before every
    // other source.
    pump_stop: PumpStop,
    // The connection's once-only retirement latch, shared with the write sink
    // and with the task that hosts the actor.
    retirement: Arc<RetirementFence>,
}

/// The gates one turn of the live loop computes before it waits. They are a
/// snapshot: the wait and the handler of the same turn see the values
/// `prepare_turn` stored, whatever the handler changes afterwards.
#[derive(Clone, Copy, Default)]
struct TurnGates {
    /// The XEP-0198 send window is latched shut, so no outbound work is
    /// pulled this turn.
    send_window_paused: bool,
    /// When the paused peer is treated as dead.
    send_window_pause_deadline: Option<tokio::time::Instant>,
    /// Deferred inbound frames are queued ahead of the socket.
    deferred_pending: bool,
    /// When a durable stream-management operation with an unknown commit
    /// outcome is next settled.
    durable_sm_settlement_due: Option<tokio::time::Instant>,
    /// A resume's outcome is unknown, so no client input is read: the next
    /// request may be the bind only a known rejection permits.
    inbound_held: bool,
}

/// What the live owner shares with, or borrows from, the rest of the
/// connection task for the lifetime of the loop and its settlement.
struct LiveOwnerCtx<'a> {
    state: &'a Arc<WebSocketState>,
    admission: &'a LegacySocketAdmission,
    admission_permit: &'a NodeAdmissionPermit,
    domain: &'a str,
    shutdown_token: &'a CancellationToken,
    caller_gone: &'a CancellationToken,
}

/// What one wait of the live loop produced: an event of the connection, or a
/// value from the hosting actor's command source, its mailbox.
enum TurnEvent<C> {
    Loop(LoopEvent),
    Command(C),
    /// A turn that withheld the command source found nothing else ready.
    Idle,
}

/// Whether a wait offers the hosting actor's command source.
///
/// An actor that just took a command withholds the source for one wait. That
/// wait polls the control sources, then every data source and timer in their
/// usual order, and returns [`TurnEvent::Idle`] at once when none is ready.
/// Commands and the rest of the loop therefore alternate while both stay
/// ready: a command source that never runs dry delays an inbound frame, an
/// outbound stanza, a keepalive tick or a pause deadline by one command, and
/// a busy connection delays a command by one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandTurn {
    Offered,
    Withheld,
}

/// How the owner left a started connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerExit {
    /// The connection settled, or closed before it started.
    Settled,
    /// The actor stopped without settling. The caller treats this exactly as
    /// an owner panic.
    Abandoned,
}

/// The one event a turn of the live loop waits for, carrying the value its
/// source yielded. Variants are listed in the priority order `next_event`
/// polls them.
enum LoopEvent {
    /// The transport pumps stopped, for the carried first reason.
    PumpStopped(StopReason),
    IdentityRevoked,
    PermitRevoked,
    Shutdown,
    /// `None` means the registry dropped the sending half.
    ForceDetach(Option<waddle_xmpp::registry::ForceDetachRequest>),
    CallerGone,
    /// The live-resume control serviced its queued requests while polled.
    LiveResumeServiced,
    DeferredInbound,
    Inbound(Option<Result<Message, InputError<axum::Error>>>),
    // Boxed so the largest payload does not set the size of every event.
    Outbound(Option<Box<OutboundStanza>>),
    OrderedRelayHandoff(Option<OrderedRelayHandoffCompletion>),
    TimerExpired(TimerId),
    SendWindowPauseDeadline,
    /// A durable stream-management operation whose commit outcome is unknown
    /// is due another settlement attempt.
    DurableSmSettlement,
}

impl LoopEvent {
    /// The bounded metric attribute for the turn that handles this event.
    fn turn_kind(&self) -> waddle_xmpp::telemetry::attributes::ActorTurnEvent {
        use waddle_xmpp::telemetry::attributes::ActorTurnEvent;
        match self {
            Self::PumpStopped(_)
            | Self::IdentityRevoked
            | Self::PermitRevoked
            | Self::ForceDetach(_)
            | Self::CallerGone
            | Self::LiveResumeServiced => ActorTurnEvent::Control,
            Self::Shutdown => ActorTurnEvent::Shutdown,
            Self::DeferredInbound | Self::Inbound(_) | Self::OrderedRelayHandoff(_) => {
                ActorTurnEvent::InboundFrame
            }
            Self::Outbound(_) => ActorTurnEvent::OutboundDelivery,
            Self::TimerExpired(_) | Self::SendWindowPauseDeadline | Self::DurableSmSettlement => {
                ActorTurnEvent::Timer
            }
        }
    }
}

/// Which handler ended the loop without naming a cause.
#[derive(Clone, Copy)]
enum HandlerExit {
    /// The inbound frame handler, which also ends the loop after it has
    /// closed the stream itself.
    Inbound,
    /// A handler that ends the loop only when a write did not complete.
    Write,
}

/// The revocation that has ended this connection's authority, if any.
fn revocation_cause(ctx: &LiveOwnerCtx<'_>) -> Option<RetirementCause> {
    if ctx.shutdown_token.is_cancelled() {
        return Some(RetirementCause::Shutdown);
    }
    match ctx.admission.revalidate() {
        Ok(()) => ctx
            .admission_permit
            .revalidate()
            .is_err()
            .then_some(RetirementCause::PermitRevoked),
        Err(SocketAdmissionError::IdentityRevoked) => Some(RetirementCause::IdentityRevoked),
        Err(SocketAdmissionError::Cancelled) => Some(RetirementCause::Shutdown),
        Err(_) => Some(RetirementCause::PermitRevoked),
    }
}

/// Name the retirement behind a handler that only reported that the loop
/// must end. A revocation is the server's own decision and outranks a pump
/// stop that may merely follow it; a stopped pump outranks what the handler
/// could tell on its own.
fn handler_exit_cause(
    ctx: &LiveOwnerCtx<'_>,
    pump_stop: &PumpStop,
    conn: &WsConnState,
    exit: HandlerExit,
) -> RetirementCause {
    if let Some(cause) = revocation_cause(ctx) {
        return cause;
    }
    if let Some(reason) = pump_stop.reason() {
        return RetirementCause::from_pump_stop(reason);
    }
    if conn.durable_sm.is_superseded() {
        return RetirementCause::SessionSuperseded;
    }
    match exit {
        HandlerExit::Write => RetirementCause::WriteFailure,
        HandlerExit::Inbound
            if !matches!(conn.phase, ConnectionPhase::Closing { .. })
                && conn.sm_inbound_completion.has_unhandled_hole() =>
        {
            RetirementCause::DispatchTimeout
        }
        HandlerExit::Inbound => RetirementCause::StreamClosed,
    }
}

/// The retirement behind an inbound source that ended without a peer close.
fn reader_end_cause(pump_stop: &PumpStop) -> RetirementCause {
    pump_stop
        .reason()
        .map_or(RetirementCause::PumpExit, RetirementCause::from_pump_stop)
}

fn fence_panicked_owner(lifecycle: &crate::clustering::NodeLifecycle, stop: &CancellationToken) {
    // Another socket can hold the lifecycle read lock in synchronous
    // `start_send`. Wake the fatal and shutdown paths before waiting for it.
    lifecycle.note_websocket_owner_panic();
    lifecycle.fatal_fence_token().cancel();
    stop.cancel();
    lifecycle.fail(crate::clustering::CriticalNodeFailure::WebSocketOwnerPanicked);
}

impl LiveWebSocketConnection {
    /// Transfer the whole lifecycle to the runtime before returning a waiter.
    /// Dropping even an unpolled waiter requests a soft stop; it never aborts
    /// the owner or its connection-owned settlement.
    fn spawn(self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let caller_gone = CancellationToken::new();
        let caller_guard = caller_gone.clone().drop_guard();
        let connection_ref = self.admission.connection_ref().clone();
        // `run` owns the original guard. Keep this clone outside its future so
        // unwinding cannot look like a completed connection drain before the
        // fatal fence and stop request have been published.
        let drain_guard = self._connection_guard.clone();
        let node_lifecycle = self.state.deps.app_state.node_lifecycle.clone();
        let stop = self.state.deps.shutdown.stop_token();
        let inventory = self.state.deps.app_state.websocket_inventory.clone();
        let transport = Arc::new(std::sync::OnceLock::new());
        // Shared with the live state. If the owner unwinds or is killed, this
        // task retires the connection in its place.
        let retirement = Arc::new(RetirementFence::new(connection_ref.clone()));
        #[cfg(feature = "test-support")]
        let panic_caught = self
            .state
            .deps
            .app_state
            .websocket_admission_test_barriers
            .owner_panic_caught
            .clone();
        #[cfg(feature = "test-support")]
        let panic_fenced = self
            .state
            .deps
            .app_state
            .websocket_admission_test_barriers
            .owner_panic_fenced
            .clone();
        let owner = tokio::spawn(async move {
            // An actor that stopped without settling, because it was killed
            // or panicked, counts as a panic: cleanup completion
            // is just as unconfirmed. A panic during settlement unwinds
            // through the hosting actor's stop hook to this point as well.
            let panicked = !matches!(
                AssertUnwindSafe(self.run(
                    caller_gone,
                    Arc::clone(&transport),
                    Arc::clone(&retirement),
                ))
                .catch_unwind()
                .await,
                Ok(OwnerExit::Settled)
            );
            if panicked {
                // No retirement after this one can act: the fence refuses any
                // late effect for this generation, and the socket halves were
                // dropped with the live state.
                if let RetirementEntry::First(cause) =
                    retirement.retire(RetirementCause::OwnerPanicked)
                {
                    waddle_xmpp::telemetry::authority::record_actor_retirement(cause.metric());
                }
            }
            // The owner has returned or unwound, so every transport handle is
            // dropped and the pumps are stopping. Release the inventory slot
            // once the custodian has seen both pumps and the owner's marker
            // task finish. A pump that does not join keeps its slot, so
            // capacity never exceeds the sockets still held.
            // After a panic the fatal fence must not wait behind a pump that
            // is slow to join, so that path gives the join only a short head
            // start and finishes the wait once the node is fenced.
            let join_bound = if panicked {
                TRANSPORT_PANIC_JOIN_TIMEOUT
            } else {
                TRANSPORT_JOIN_TIMEOUT
            };
            let mut slot_released = match transport.get() {
                Some(observer) => release_transport_slot(&inventory, observer, join_bound).await,
                None => true,
            };

            if panicked {
                #[cfg(feature = "test-support")]
                if let Some(barrier) = panic_caught {
                    barrier.arrive().await;
                }
                fence_panicked_owner(&node_lifecycle, &stop);
                error!(
                    ?connection_ref,
                    "WebSocket connection owner panicked; cleanup completion is unconfirmed"
                );
                #[cfg(feature = "test-support")]
                if let Some(barrier) = panic_fenced {
                    barrier.arrive().await;
                }
                if !slot_released {
                    if let Some(observer) = transport.get() {
                        slot_released =
                            release_transport_slot(&inventory, observer, TRANSPORT_JOIN_TIMEOUT)
                                .await;
                    }
                }
            }
            if let Some(observer) = transport.get() {
                record_transport_retirement(observer, slot_released, panicked);
            }
            if !slot_released {
                error!(
                    ?connection_ref,
                    "WebSocket transport pumps did not join; the inventory slot stays held"
                );
            }
            drop(drain_guard);
        });
        async move {
            let _caller_guard = caller_guard;
            // The owner catches and reports its own panic even if this waiter
            // has already gone away. Dropping a JoinHandle detaches the task.
            let _ = owner.await;
        }
    }

    /// Run the connection through its live I/O loop and shutdown settlement.
    async fn run(
        mut self,
        caller_gone: CancellationToken,
        transport: Arc<std::sync::OnceLock<IoCustodianObserver>>,
        retirement: Arc<RetirementFence>,
    ) -> OwnerExit {
        #[cfg(feature = "test-support")]
        if let Some(barrier) = &self
            .state
            .deps
            .app_state
            .websocket_admission_test_barriers
            .in_callback
        {
            barrier.arrive().await;
        }
        if let Err(reason) = self.admission.revalidate() {
            info!(
                ?reason,
                "Closing upgraded WebSocket: admission was revoked before XMPP start"
            );
            // HTTP 101 is already on the wire. RFC 7395 therefore requires a
            // WebSocket close handshake rather than silently dropping the
            // upgraded TCP stream; no XMPP stream exists at this boundary.
            close_unstarted_upgraded_socket(self.socket.as_mut()).await;
            return OwnerExit::Settled;
        }
        if caller_gone.is_cancelled() {
            info!(
                connection_ref = ?self.admission.connection_ref(),
                "WebSocket upgrade waiter dropped before XMPP start"
            );
            close_unstarted_upgraded_socket(self.socket.as_mut()).await;
            return OwnerExit::Settled;
        }
        let limits = match (
            Limits::new(TRANSPORT_INBOUND_FRAMES, TRANSPORT_INBOUND_QUEUE_BYTES),
            Limits::new(TRANSPORT_OUTBOUND_FRAMES, TRANSPORT_OUTBOUND_QUEUE_BYTES),
        ) {
            (Ok(inbound), Ok(outbound)) => TransportLimits::new(inbound, outbound),
            _ => unreachable!("the transport queue bounds are positive and within range"),
        };
        let LiveSocketTransport {
            observer: transport_observer,
            cleanup: _transport_cleanup,
            input: ws_receiver,
            output: ws_sender,
            pump_stop,
        } = match self
            .admission
            .start_socket(
                *self.socket,
                &self.state.deps.app_state.websocket_inventory,
                limits,
                TRANSPORT_RETIREMENT_DEADLINE,
            )
            .await
        {
            Ok(transport) => transport,
            Err(error) => {
                info!(
                    %error,
                    connection_ref = ?self.admission.connection_ref(),
                    "Closing upgraded WebSocket: transport start was refused"
                );
                return OwnerExit::Settled;
            }
        };
        let _ = transport.set(transport_observer.clone());
        let domain = self.state.deps.auth_state.xmpp_domain.clone();
        let shutdown_token = self.state.deps.shutdown.stop_token();
        info!(domain = %domain, "XMPP WebSocket connection established");

        // The outbound channel receives messages routed from other
        // connections; `LiveSocketState` documents how its two halves are
        // handed over.
        let (outbound_endpoint, outbound_rx) =
            SocketOutboundEndpoint::channel_with_socket_receiver(OUTBOUND_CHANNEL_SIZE);

        // Track connection state
        let mut conn = WsConnState::new_for_connection_ref(self.admission.connection_ref().clone());
        let live_resume_control = conn.live_resume_control.clone();
        let _live_resume_owner_guard = LiveResumeOwnerGuard(live_resume_control.clone());
        let ws_sender = InterruptibleSink::new(ws_sender, conn.write_interruption.clone())
            .with_resume_control(live_resume_control.clone())
            .with_retirement(
                Arc::clone(&retirement),
                self.admission.connection_ref().clone(),
            );
        // This owner spans live I/O and shutdown settlement, including
        // cancellation/unwind. The physical halves belong to the pump tasks,
        // so the witness is theirs: dropping `socket_io` stops the pumps, and
        // they publish once both halves are gone.
        let (socket_io, _handles_dropped) = LocalSocketIo::new(ws_sender, ws_receiver);
        conn.local_io_relinquishment = Some(
            waddle_xmpp::registry::LocalIoRelinquishment::from_completion(
                transport_observer.local_io_relinquishment().completion(),
            ),
        );
        let (handoff_tx, handoff_rx) =
            crate::server::routes::interpret::ordered_relay_handoff_channel();
        conn.ordered_relay_handoff_tx = Some(handoff_tx);
        // RFC 7395 §3.8 keepalive (issue #1090): the liveness policy lives
        // in the per-connection sans-io machine, so one must exist from
        // the very first instant — a client that wedges before
        // authenticating is reaped by the same clock. `TransportReady`
        // arms the keepalive timer; the machine re-arms it on every tick.
        conn.init_prebind_state_machine(
            &domain,
            &self.state.deps.protocol.dispatcher,
            self.state.deps.ws_keepalive,
        );
        let live = Box::new(LiveSocketState {
            outbound_rx,
            superseded: false,
            timers: TransportTimers::new(),
            handoff_rx,
            socket_io: Some(socket_io),
            _transport_cleanup,
            _live_resume_owner_guard,
            live_resume_control,
            conn,
            pending_force_detach: Vec::new(),
            force_detach_rx: None,
            pending_tx: Some(outbound_endpoint),
            turn: TurnGates::default(),
            pump_stop,
            retirement,
        });
        // The actor is run on this task and dropped before `host` returns, so
        // the connection's resources are released before the admission it
        // also owns, and both before the drain guard this owner keeps.
        live_actor::host(
            live,
            live_actor::LiveOwner {
                state: self.state,
                admission: self.admission,
                domain,
                shutdown_token,
                caller_gone,
            },
        )
        .await
    }
}

impl LiveSocketState {
    /// Tell the pre-bind state machine the transport is up, which arms its
    /// first timers.
    async fn start(&mut self, ctx: &LiveOwnerCtx<'_>) {
        #[cfg(feature = "test-support")]
        if let Some(barrier) = &ctx
            .state
            .deps
            .app_state
            .websocket_admission_test_barriers
            .owner_panic_active
        {
            barrier.arrive().await;
            panic!("test WebSocket owner panic after activation");
        }
        if let Some(sm) = self.conn.state_machine.as_mut() {
            let events = sm.handle(InboundEvent::TransportReady);
            let interpret_deps = build_interpret_deps(ctx.state.as_ref(), None);
            let drive = drive_interpret_loop(events, sm, &interpret_deps).await;
            self.timers.apply(drive.timer_commands);
        }
    }

    /// Do the work that precedes every wait: apply the send-window gate, note
    /// whether deferred frames are queued, and bring a frame the socket
    /// already holds forward. `Break` ends the loop without waiting and names
    /// the retirement.
    async fn prepare_turn(&mut self, ctx: &LiveOwnerCtx<'_>) -> ControlFlow<Retire> {
        let admission = ctx.admission;
        let admission_permit = ctx.admission_permit;
        let shutdown_token = ctx.shutdown_token;
        let caller_gone = ctx.caller_gone;
        let Self {
            socket_io,
            conn,
            turn,
            pump_stop,
            ..
        } = self;
        let Some(socket_halves) = socket_io.as_mut() else {
            unreachable!("the live connection owns its socket halves");
        };
        let (mut ws_sender, ws_receiver) = socket_halves.halves_mut();

        // Loop-level XEP-0198 send-window gate (issue #1219). When the
        // outstanding unacked count has latched the pause, stop draining the
        // outbound mpsc (`next_event` guards its `recv()`): producers then
        // backpressure on the 256-slot channel instead of piling more into
        // the SM unacked queue, while `ws_receiver` keeps delivering the
        // `<a/>` acks that shrink the window. On the rising edge send one
        // forced `<r/>` (the wasm client acks only when asked) and arm a
        // deadline; on recovery, disarm. No await for this lives inside the
        // wait itself — acks flow freely and the gate is deadlock-free by
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
            if (rising_edge || conn.sm_state.last_acked != conn.send_window_last_request_acked)
                && !caller_gone.is_cancelled()
            {
                conn.send_window_last_request_acked = conn.sm_state.last_acked;
                if admission.revalidate().is_err() {
                    return ControlFlow::Break(Retire::owing(
                        revocation_cause(ctx).unwrap_or(RetirementCause::PermitRevoked),
                        OwedClose::NodeUnavailable,
                    ));
                }
                if !matches!(
                    send_ws_message_with_authority(
                        &mut ws_sender,
                        Message::Text(SmRequest::to_xml().into()),
                        "Failed to send SM <r/> at send-window loop pause",
                        Some((admission_permit, shutdown_token)),
                    )
                    .await,
                    AuthoritySendOutcome::Sent
                ) {
                    return ControlFlow::Break(Retire::settled(handler_exit_cause(
                        ctx,
                        pump_stop,
                        conn,
                        HandlerExit::Write,
                    )));
                }
                conn.sm_state.note_ack_request_sent();
            }
        } else {
            conn.send_window_pause_deadline = None;
        }
        let send_window_pause_deadline = conn.send_window_pause_deadline;

        // Frames the mid-batch ack drain (issue #1089) pulled off the
        // socket ahead of the dispatcher must be processed in arrival
        // order BEFORE the socket is polled again — so `next_event` gates
        // the socket on the queue being empty, and offers an always-ready
        // event that processes a bounded ordered chunk per turn. Deferred
        // processing stays an event of its own (not a loop here), and
        // yields between chunks, so a client streaming frames mid-flood
        // cannot monopolize the executor.
        let deferred_pending = !conn.deferred_inbound.is_empty();
        *turn = TurnGates {
            send_window_paused,
            send_window_pause_deadline,
            deferred_pending,
            durable_sm_settlement_due: conn.durable_sm.settlement_due(),
            inbound_held: conn.durable_sm.holds_inbound(),
        };
        // The wait prefers client input to its timers. A frame the socket
        // already holds must therefore be ready before the wait is polled,
        // as it would be for a task reading its own socket.
        if !deferred_pending && !conn.durable_sm.holds_inbound() {
            ws_receiver.settle_inbound().await;
        }
        ControlFlow::Continue(())
    }

    /// Wait for exactly one event, preferring the earlier source when several
    /// are ready. Nothing is handled here. Every source is safe to abandon
    /// once another one wins: each either yields its value in the same poll
    /// that takes it from its queue, or keeps its progress in the state it
    /// borrows (`timers`, the live-resume control), so no value is lost
    /// between turns.
    ///
    /// The pump-stop signal is polled first, then the other control sources.
    /// `commands` is the hosting actor's mailbox. When offered it is polled after
    /// the control sources and before the data sources and timers: a full
    /// command queue cannot delay revocation, shutdown or detach, and a
    /// client that keeps the socket and the outbound queue busy cannot keep a
    /// command waiting. [`CommandTurn`] describes the turn that withholds it
    /// so that commands cannot keep the data sources and timers waiting
    /// either. It must be safe to abandon in the same way.
    async fn next_event<C>(
        &mut self,
        ctx: &LiveOwnerCtx<'_>,
        commands: impl std::future::Future<Output = C>,
        command_turn: CommandTurn,
    ) -> TurnEvent<C> {
        let commands_offered = command_turn == CommandTurn::Offered;
        let TurnGates {
            send_window_paused,
            send_window_pause_deadline,
            deferred_pending,
            durable_sm_settlement_due,
            inbound_held,
        } = self.turn;
        let Some(socket_halves) = self.socket_io.as_mut() else {
            unreachable!("the live connection owns its socket halves");
        };
        let (_, ws_receiver) = socket_halves.halves_mut();
        let event = tokio::select! {
            biased;

            // A stopped pump ends the session whatever else is ready: the
            // socket can no longer be read or written, so no later source of
            // this wait could complete its work. The custodian reports the
            // stop directly, without a mailbox turn and without the loop
            // first reading or writing the socket.
            reason = self.pump_stop.stopped() => LoopEvent::PumpStopped(reason),

            // Observe source revision changes at turn boundaries. The
            // bound permit also fences each ordinary synchronous enqueue.
            _ = ctx.admission.identity_revoked() => LoopEvent::IdentityRevoked,

            // Lifecycle recovery mints a new generation and cannot
            // resurrect the permit that admitted this connection.
            _ = ctx.admission_permit.revoked() => LoopEvent::PermitRevoked,

            // Process stop is the highest-priority event. If cancellation and
            // a queued frame/work item are both ready, no further stanza work
            // starts after the node has begun draining or failed critically.
            _ = ctx.shutdown_token.cancelled() => LoopEvent::Shutdown,

            // A force-detach request for this connection's own stream —
            // deliberately ahead of every inbound/deferred/outbound source in
            // this biased select: a continuously busy client must not starve
            // its own termination signal (conflict kill, cross-node steal,
            // or SM claim demotion, whose fence is already removed — every
            // starved turn would process stanzas fenceless). Frames the
            // client already sent are replayed by it after resume (XEP-0198
            // retransmission), so preempting them loses nothing durable.
            request = recv_optional(&mut self.force_detach_rx) => LoopEvent::ForceDetach(request),

            // Losing the upgrade callback's waiter requests normal
            // settlement. In-flight handlers and cleanup are not raced
            // against this signal, and shutdown/detach retain priority.
            _ = ctx.caller_gone.cancelled() => LoopEvent::CallerGone,

            // Accepted detach requests stay ahead of new preflight work, so
            // a stream of rejected attempts cannot starve its own retirement.
            // Validation still precedes ordinary inbound/deferred work.
            _ = std::future::poll_fn(|cx| self.live_resume_control.poll(cx)) => {
                LoopEvent::LiveResumeServiced
            }

            command = commands, if commands_offered => return TurnEvent::Command(command),

            // An enable or resume whose commit outcome is unknown owes the
            // client its reply. The attempt is spaced by its own backoff, and
            // sits ahead of the data sources so a busy client cannot keep its
            // own stream-management outcome waiting.
            _ = async {
                match durable_sm_settlement_due {
                    Some(due) => tokio::time::sleep_until(due).await,
                    None => std::future::pending().await,
                }
            } => LoopEvent::DurableSmSettlement,

            // A bounded drain-deferred inbound chunk is due.
            _ = std::future::ready(()), if deferred_pending && !inbound_held => {
                LoopEvent::DeferredInbound
            }

            // Inbound WebSocket messages from the client.
            msg = ws_receiver.next(), if !deferred_pending && !inbound_held => {
                LoopEvent::Inbound(msg)
            }

            // Outbound messages routed from other connections. Gated by the
            // send-window pause (issue #1219): while paused no new outbound
            // work is pulled, so its producers backpressure on the mpsc
            // rather than overflowing the SM unacked queue. The inbound
            // source above stays active, so client acks keep arriving and
            // releasing the pause.
            outbound = self.outbound_rx.recv(), if !send_window_paused => {
                LoopEvent::Outbound(outbound.map(Box::new))
            }

            handoff = self.handoff_rx.recv() => LoopEvent::OrderedRelayHandoff(handoff),

            // RFC 7395 §3.8 keepalive clock (issue #1090). Fires only when
            // the state machine armed a timer.
            timer_id = self.timers.next_expired() => LoopEvent::TimerExpired(timer_id),

            // Loop-level send-window pause deadline (issue #1219). Fires
            // only while paused.
            _ = async {
                match send_window_pause_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            }, if send_window_paused => LoopEvent::SendWindowPauseDeadline,

            // Last, so a withheld turn has polled every other source first.
            _ = std::future::ready(()), if !commands_offered => return TurnEvent::Idle,
        };
        TurnEvent::Loop(event)
    }

    /// Handle the one event this turn waited for. `Break` ends the loop and
    /// names the retirement; the caller passes it to [`Self::retire`], which
    /// is the only place a retirement's close is written.
    async fn handle_event(
        &mut self,
        ctx: &LiveOwnerCtx<'_>,
        event: LoopEvent,
    ) -> ControlFlow<Retire> {
        let state = ctx.state;
        let admission = ctx.admission;
        let admission_permit = ctx.admission_permit;
        let domain = ctx.domain;
        let shutdown_token = ctx.shutdown_token;
        let caller_gone = ctx.caller_gone;
        let Self {
            outbound_rx,
            superseded,
            timers,
            socket_io,
            conn,
            pending_force_detach,
            force_detach_rx,
            pending_tx,
            pump_stop,
            ..
        } = self;
        let handler_exit = |conn: &WsConnState, exit| {
            match handler_exit_cause(ctx, pump_stop, conn, exit) {
                // The peer of a superseded binding is told the stream ended
                // in conflict, as a displaced binding is.
                cause @ RetirementCause::SessionSuperseded => {
                    Retire::owing(cause, OwedClose::Conflict)
                }
                cause => Retire::settled(cause),
            }
        };
        let Some(socket_halves) = socket_io.as_mut() else {
            unreachable!("the live connection owns its socket halves");
        };
        let (mut ws_sender, mut ws_receiver) = socket_halves.halves_mut();

        match event {
            LoopEvent::PumpStopped(reason) => {
                info!(
                    jid = ?conn.phase.bound_jid(),
                    ?reason,
                    "Transport pumps stopped: retiring live session"
                );
                return ControlFlow::Break(Retire::settled(RetirementCause::from_pump_stop(
                    reason,
                )));
            }

            LoopEvent::IdentityRevoked => {
                info!(
                    jid = ?conn.phase.bound_jid(),
                    "Node identity changed: closing live session"
                );
                return ControlFlow::Break(Retire::owing(
                    RetirementCause::IdentityRevoked,
                    OwedClose::NodeUnavailable,
                ));
            }

            LoopEvent::PermitRevoked => {
                info!(
                    jid = ?conn.phase.bound_jid(),
                    "Node lifecycle changed: closing live session"
                );
                return ControlFlow::Break(Retire::owing(
                    RetirementCause::PermitRevoked,
                    OwedClose::NodeUnavailable,
                ));
            }

            LoopEvent::Shutdown => {
                info!(
                    jid = ?conn.phase.bound_jid(),
                    "Graceful shutdown: closing live session with system-shutdown stream error"
                );
                return ControlFlow::Break(Retire::owing(
                    RetirementCause::Shutdown,
                    OwedClose::NodeUnavailable,
                ));
            }

            // Cross-node XEP-0198 resume uses the live-steal handshake; stale
            // UserActor retirement uses the same safe connection-owned close
            // path. The identity check gates the destructive close itself
            // (defense in depth against a wrong-identity `previd` forcing a
            // disconnect before rejection) — a mismatch answers inline and
            // this connection keeps serving normally; a match sends
            // `<conflict/>` (XEP-0198 "Resumption" SHOULD) and closes,
            // falling through to the SAME detach-for-resume cleanup a
            // graceful/keepalive close uses (never transitions `phase` to
            // `Closing`).
            LoopEvent::ForceDetach(request) => {
                match request {
                    Some(request) => {
                        let bound_bare = conn.phase.bound_jid().map(|jid| jid.to_bare());
                        if !request.targets_binding(conn.binding_ref.as_ref()) {
                            // Closing the acknowledgement is an unknown outcome,
                            // never proof that this socket completed cleanup.
                            debug!("force-detach ignored: binding retired after enqueue");
                        } else if bound_bare.as_ref() != Some(&request.requester_bare_jid) {
                            warn!("force-detach rejected: identity mismatch");
                            let _ = request
                                .ack
                                .send(waddle_xmpp::registry::ForceDetachOutcome::IdentityMismatch);
                        } else if shutdown_token.is_cancelled()
                            || admission_permit.revalidate().is_err()
                        {
                            // The old serving generation may no longer emit
                            // the optional `<conflict/>`. Still acknowledge
                            // only after normal detach persistence in `settle`.
                            pending_force_detach.push(request);
                            if let Some(bound_bare) = bound_bare.as_ref() {
                                pending_force_detach.extend(drain_ready_force_detach_requests(
                                    &mut *force_detach_rx,
                                    bound_bare,
                                    conn.binding_ref.as_ref(),
                                ));
                            }
                            return ControlFlow::Break(Retire::settled(
                                RetirementCause::ForceDetach,
                            ));
                        } else {
                            info!("force-detaching this session (<conflict/> close)");
                            // Deferred: acked only after this connection's own
                            // detach-for-resume cleanup in `settle` actually runs.
                            pending_force_detach.push(request);
                            if let Some(bound_bare) = bound_bare.as_ref() {
                                pending_force_detach.extend(drain_ready_force_detach_requests(
                                    &mut *force_detach_rx,
                                    bound_bare,
                                    conn.binding_ref.as_ref(),
                                ));
                            }
                            return ControlFlow::Break(Retire::owing(
                                RetirementCause::ForceDetach,
                                OwedClose::Conflict,
                            ));
                        }
                    }
                    None => {
                        // The registry entry was removed (e.g. superseded)
                        // without this channel ever being used — disable
                        // this source for the remainder of the loop.
                        *force_detach_rx = None;
                    }
                }
            }

            LoopEvent::CallerGone => {
                info!(
                    connection_ref = ?admission.connection_ref(),
                    "WebSocket upgrade waiter dropped; settling connection"
                );
                return ControlFlow::Break(Retire::settled(RetirementCause::OwnerCancelled));
            }

            LoopEvent::LiveResumeServiced => {}

            // Process a bounded drain-deferred inbound chunk.
            LoopEvent::DeferredInbound => {
                for _ in 0..DEFERRED_INBOUND_DRAIN_CHUNK {
                    let Some(text) = conn.deferred_inbound.pop_front() else {
                        break;
                    };
                    if !handle_inbound_text(
                        &text,
                        domain,
                        state,
                        &mut *conn,
                        RegistrationChannels {
                            pending_tx: &mut *pending_tx,
                            outbound_receiver: Some(&*outbound_rx),
                            force_detach_rx: &mut *force_detach_rx,
                        },
                        ConnectionIo {
                            sender: &mut ws_sender,
                            receiver: &mut ws_receiver,
                        },
                        FrameAuthority {
                            permit: admission_permit,
                            shutdown: shutdown_token,
                        },
                    )
                    .await
                    {
                        return ControlFlow::Break(handler_exit(conn, HandlerExit::Inbound));
                    }
                    if caller_gone.is_cancelled() {
                        break;
                    }
                }
                tokio::task::yield_now().await;
            }

            // Handle inbound WebSocket messages from the client
            LoopEvent::Inbound(msg) => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if !handle_inbound_text(
                            text.as_str(),
                            domain,
                            state,
                            &mut *conn,
                            RegistrationChannels {
                                pending_tx: &mut *pending_tx,
                                outbound_receiver: Some(&*outbound_rx),
                                force_detach_rx: &mut *force_detach_rx,
                            },
                            ConnectionIo {
                                sender: &mut ws_sender,
                                receiver: &mut ws_receiver,
                            },
                            FrameAuthority {
                                permit: admission_permit,
                                shutdown: shutdown_token,
                            },
                        )
                        .await
                        {
                            return ControlFlow::Break(handler_exit(conn, HandlerExit::Inbound));
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
                                Some((admission_permit, shutdown_token)),
                            )
                            .await,
                            AuthoritySendOutcome::Sent
                        ) {
                            return ControlFlow::Break(handler_exit(conn, HandlerExit::Write));
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
                        return ControlFlow::Break(Retire::settled(RetirementCause::PeerClose));
                    }
                    Some(Err(e)) => {
                        error!(error = %e, "WebSocket error");
                        return ControlFlow::Break(Retire::settled(reader_end_cause(pump_stop)));
                    }
                    None => {
                        // Stream ended
                        debug!("WebSocket stream ended");
                        return ControlFlow::Break(Retire::settled(reader_end_cause(pump_stop)));
                    }
                }
            }

            // Handle outbound messages routed from other connections.
            LoopEvent::Outbound(outbound) => {
                match outbound {
                    Some(outbound_stanza) => {
                        if !handle_outbound_stanza(
                            &mut ws_sender,
                            &mut ws_receiver,
                            state,
                            &mut *conn,
                            &mut *timers,
                            *outbound_stanza,
                            OutboundAuthority {
                                permit: admission_permit,
                                shutdown: shutdown_token,
                            },
                        )
                        .await
                        {
                            return ControlFlow::Break(handler_exit(conn, HandlerExit::Write));
                        }
                    }
                    None => {
                        // Outbound channel closed. All clones of the sender (our
                        // own outbound_tx + any copy held by the registry) have
                        // been dropped. The only path to this state after
                        // registration is a replacement register for the same
                        // FullJid: the registry drops our entry (and with it
                        // the sender) to install the new session's sender.
                        // Superseded cleanup preserves the replacement's route
                        // and does not detach this old stream. It still sweeps
                        // this stream's occupancy generation in every room.
                        info!("Outbound channel closed; session superseded by replacement");
                        *superseded = true;
                        return ControlFlow::Break(Retire::settled(RetirementCause::Replaced));
                    }
                }
            }

            LoopEvent::OrderedRelayHandoff(handoff) => match handoff {
                Some(handoff) => {
                    if !handle_ordered_relay_handoff_completion(
                        &mut ws_sender,
                        &mut ws_receiver,
                        state,
                        &mut *conn,
                        handoff,
                        admission_permit,
                        shutdown_token,
                    )
                    .await
                    {
                        return ControlFlow::Break(handler_exit(conn, HandlerExit::Write));
                    }
                }
                None => {
                    warn!("Ordered relay handoff completion channel closed");
                    return ControlFlow::Break(Retire::settled(RetirementCause::StreamClosed));
                }
            },

            // The tick is fed back into the state machine, whose policy
            // decides: quiet re-arm, probe + re-arm, or close a dead peer. A
            // keepalive close ends the loop with a graceful WS close
            // handshake and rides the normal shutdown fork in `settle` — SM
            // detach-for-resume when negotiated, full cleanup otherwise.
            LoopEvent::TimerExpired(timer_id) => {
                let session = conn.authenticated_session.clone();
                let principal = session
                    .as_ref()
                    .map(super::ResolvedPrincipal::from_authenticated_session);
                let Some(sm) = conn.state_machine.as_mut() else {
                    warn!(
                        timer_id = timer_id.0,
                        "Timer fired without a state machine; disarming"
                    );
                    return ControlFlow::Continue(());
                };
                let events = sm.handle(InboundEvent::Tick(timer_id));
                let interpret_deps = build_interpret_deps(state.as_ref(), principal);
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
                            Some((admission_permit, shutdown_token)),
                        )
                        .await,
                        AuthoritySendOutcome::Sent
                    ) {
                        ping_send_failed = true;
                        break;
                    }
                }
                if ping_send_failed {
                    return ControlFlow::Break(handler_exit(conn, HandlerExit::Write));
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
                    return ControlFlow::Break(Retire::owing(
                        RetirementCause::PeerUnresponsive,
                        OwedClose::Transport,
                    ));
                }
            }

            // If the client never acks the window down in time it is dead,
            // so end the loop WITHOUT transitioning to Closing — the cleanup
            // fork in `settle` detaches an SM session for resume, and the
            // retained queue is bounded (pacing kept it ≤ cap) so that
            // resume stays clean.
            LoopEvent::SendWindowPauseDeadline => {
                waddle_xmpp::telemetry::reliability::increment_sm_send_window_pause_timeout();
                warn!(
                    jid = ?conn.phase.bound_jid(),
                    deadline_secs = SEND_WINDOW_LOOP_PAUSE_DEADLINE.as_secs(),
                    "SM send-window loop pause timed out with no recovering ack; \
                     closing into detach-for-resume"
                );
                return ControlFlow::Break(Retire::settled(RetirementCause::PeerUnresponsive));
            }

            LoopEvent::DurableSmSettlement => {
                // Each resolution is idempotent under its operation id, so
                // the control sources may end it early; the next turn then
                // retires the connection for the control event itself.
                if conn.durable_sm.holds_inbound() {
                    let settlement = tokio::select! {
                        biased;
                        _ = pump_stop.stopped() => return ControlFlow::Continue(()),
                        _ = admission.identity_revoked() => return ControlFlow::Continue(()),
                        _ = admission_permit.revoked() => return ControlFlow::Continue(()),
                        _ = shutdown_token.cancelled() => return ControlFlow::Continue(()),
                        settlement = super::stream_management::settle_unresolved_resume(
                            state.as_ref(),
                            &mut *conn,
                        ) => settlement,
                    };
                    let responses = match settlement {
                        None => return ControlFlow::Continue(()),
                        // The staged resume is published and answered by the
                        // same steps that follow a resume handled in its
                        // own turn.
                        Some(super::stream_management::DurableResumeSettlement::Staged) => {
                            if let Some(success) = conn.sasl2_success.as_mut() {
                                success.held_resume_committed();
                            }
                            ResponseBatch::default()
                        }
                        Some(super::stream_management::DurableResumeSettlement::Failed {
                            frames,
                            inline,
                        }) => match conn.sasl2_success.as_mut() {
                            // The exchange reports the failure in its
                            // `<success/>` and goes on to its bind request.
                            Some(success) => {
                                success.held_resume_failed(inline);
                                conn.pending_finalized_resume_outcome = None;
                                ResponseBatch::default()
                            }
                            None => ResponseBatch::from(frames),
                        },
                    };
                    if !complete_inbound_turn(
                        responses,
                        domain,
                        state,
                        &mut *conn,
                        RegistrationChannels {
                            pending_tx: &mut *pending_tx,
                            outbound_receiver: Some(&*outbound_rx),
                            force_detach_rx: &mut *force_detach_rx,
                        },
                        ConnectionIo {
                            sender: &mut ws_sender,
                            receiver: &mut ws_receiver,
                        },
                        FrameAuthority {
                            permit: admission_permit,
                            shutdown: shutdown_token,
                        },
                    )
                    .await
                    {
                        return ControlFlow::Break(handler_exit(conn, HandlerExit::Inbound));
                    }
                    return ControlFlow::Continue(());
                }
                let settlement = tokio::select! {
                    biased;
                    _ = pump_stop.stopped() => return ControlFlow::Continue(()),
                    _ = admission.identity_revoked() => return ControlFlow::Continue(()),
                    _ = admission_permit.revoked() => return ControlFlow::Continue(()),
                    _ = shutdown_token.cancelled() => return ControlFlow::Continue(()),
                    settlement = super::stream_management::settle_unresolved_enable(
                        state.as_ref(),
                        &mut conn.durable_sm,
                        &mut conn.pending_sm_enable_commit,
                    ) => settlement,
                };
                let (reply, commit) = match settlement {
                    super::stream_management::EnableSettlement::Pending => {
                        return ControlFlow::Continue(());
                    }
                    super::stream_management::EnableSettlement::Enabled { reply, commit } => {
                        (reply, Some(commit))
                    }
                    super::stream_management::EnableSettlement::Failed(reply) => (reply, None),
                };
                if let Some(success) = conn.sasl2_success.as_mut() {
                    // The enable was made for a Bind 2 resource: its outcome
                    // completes the exchange's `<success/>`, which is written
                    // by the steps that follow any turn.
                    success.held_enable_settled(commit.as_ref());
                    if !complete_inbound_turn(
                        ResponseBatch::default(),
                        domain,
                        state,
                        &mut *conn,
                        RegistrationChannels {
                            pending_tx: &mut *pending_tx,
                            outbound_receiver: Some(&*outbound_rx),
                            force_detach_rx: &mut *force_detach_rx,
                        },
                        ConnectionIo {
                            sender: &mut ws_sender,
                            receiver: &mut ws_receiver,
                        },
                        FrameAuthority {
                            permit: admission_permit,
                            shutdown: shutdown_token,
                        },
                    )
                    .await
                    {
                        return ControlFlow::Break(handler_exit(conn, HandlerExit::Inbound));
                    }
                    return ControlFlow::Continue(());
                }
                if !matches!(
                    send_ws_message_with_authority(
                        &mut ws_sender,
                        Message::Text(reply.into_serialized_xml().into()),
                        "Failed to send settled SM enable reply",
                        Some((admission_permit, shutdown_token)),
                    )
                    .await,
                    AuthoritySendOutcome::Sent
                ) {
                    return ControlFlow::Break(handler_exit(conn, HandlerExit::Write));
                }
                // `<enabled/>` is on the wire: the projection starts counting
                // exactly as it does after an enable answered in its own turn.
                conn.publish_pending_sm_enable(state.as_ref());
            }
        }
        ControlFlow::Continue(())
    }

    /// The binding ledger, for a step that names `generation`. A step for a
    /// retired or foreign generation is refused.
    fn binding_ledger(
        &mut self,
        generation: &waddle_xmpp::ownership::ConnectionRef,
    ) -> Result<&mut BindingLedger, BindingRefusal> {
        self.retirement.admit(generation)?;
        Ok(&mut self.conn.bindings)
    }

    /// Retire the connection: the one way its live loop ends.
    ///
    /// The first call latches the cause, counts it and writes the close the
    /// cause still owes the peer. From then on the write sink refuses every
    /// ordinary frame for this generation. A later call does nothing and
    /// returns the cause already recorded. Settlement completes the
    /// retirement once the hosting actor has stopped.
    async fn retire(&mut self, ctx: &LiveOwnerCtx<'_>, request: Retire) -> RetirementCause {
        let cause = match self.retirement.retire(request.cause) {
            RetirementEntry::First(cause) => cause,
            RetirementEntry::Already(cause) => return cause,
        };
        waddle_xmpp::telemetry::authority::record_actor_retirement(cause.metric());
        debug!(
            connection_ref = ?ctx.admission.connection_ref(),
            ?cause,
            "Retiring connection"
        );
        let Some(socket_halves) = self.socket_io.as_mut() else {
            return cause;
        };
        let (ws_sender, _) = socket_halves.halves_mut();
        match request.close {
            OwedClose::None => {}
            OwedClose::NodeUnavailable => {
                close_live_session_for_node_unavailable(
                    &mut ws_sender.retirement_close(),
                    &self.conn,
                )
                .await;
            }
            OwedClose::Conflict => {
                close_live_session_for_force_detach(
                    ws_sender.uninterrupted(),
                    &self.conn,
                    ctx.admission_permit,
                    ctx.shutdown_token,
                )
                .await;
            }
            OwedClose::Transport => {
                let _ = close_ws_connection(
                    &mut ws_sender.retirement_close(),
                    "Failed to send WebSocket close frame after keepalive close",
                )
                .await;
            }
        }
        cause
    }

    /// Run the connection-owned shutdown once the loop has ended. The socket
    /// halves and the outbound receiver move into settlement; everything else
    /// this state owns drops in field order when settlement returns.
    async fn settle(mut self: Box<Self>, ctx: &LiveOwnerCtx<'_>) {
        #[cfg(feature = "test-support")]
        if let Some(barrier) = &ctx
            .state
            .deps
            .app_state
            .websocket_admission_test_barriers
            .owner_panic_settling
        {
            barrier.arrive().await;
            panic!("test WebSocket owner panic during settlement");
        }
        let Some(socket_io) = self.socket_io.take() else {
            unreachable!("the live connection owns its socket halves until settlement");
        };
        // A normal stop always follows `retire`. Latching here as well means
        // settlement never runs for a connection that is not retired.
        let cause = match self.retirement.retire(RetirementCause::OwnerCancelled) {
            RetirementEntry::First(cause) => {
                waddle_xmpp::telemetry::authority::record_actor_retirement(cause.metric());
                cause
            }
            RetirementEntry::Already(cause) => cause,
        };
        let mut outbound_rx = self.outbound_rx.into_receiver();
        finish_connection(
            ctx.state,
            &mut self.conn,
            socket_io,
            FrameAuthority {
                permit: ctx.admission_permit,
                shutdown: ctx.shutdown_token,
            },
            cause,
            self.superseded,
            ConnectionShutdownChannels {
                outbound_rx: &mut outbound_rx,
                handoff_rx: &mut self.handoff_rx,
                force_detach_rx: &mut self.force_detach_rx,
                pending_force_detach: &mut self.pending_force_detach,
            },
        )
        .await;
    }
}

/// Free the connection's inventory slot once its custodian reports the pumps
/// and the owner's marker task joined. Returns whether that happened in time.
async fn release_transport_slot(
    inventory: &IoCustodianInventory,
    observer: &IoCustodianObserver,
    bound: std::time::Duration,
) -> bool {
    let joined = tokio::time::timeout(bound, observer.wait_until_joined())
        .await
        .is_ok();
    if joined {
        inventory.prune_joined();
    }
    joined
}

/// Count how the transport of one finished connection retired, and which
/// pump ended it when the first stop reason names one.
fn record_transport_retirement(observer: &IoCustodianObserver, joined: bool, owner_failed: bool) {
    use waddle_xmpp::telemetry::attributes::{IoExitCause, IoPump};
    use waddle_xmpp::telemetry::authority::{record_io_custodian_retirement, record_io_pump_exit};
    let pump_exit = match observer.stop_reason() {
        Some(StopReason::PeerClosed) => Some((IoPump::Reader, IoExitCause::PeerClosed)),
        Some(StopReason::InboundIoFailure) => Some((IoPump::Reader, IoExitCause::IoError)),
        Some(StopReason::InboundOverload | StopReason::InboundQueueClosed) => {
            Some((IoPump::Reader, IoExitCause::Abandoned))
        }
        Some(StopReason::OutboundIoFailure) => Some((IoPump::Writer, IoExitCause::IoError)),
        Some(StopReason::OutboundOverload | StopReason::OutboundQueueClosed) => {
            Some((IoPump::Writer, IoExitCause::Abandoned))
        }
        // These stop both pumps from outside; neither pump ended first.
        Some(
            StopReason::Requested
            | StopReason::PumpExited
            | StopReason::ShutdownDeadline
            | StopReason::LeaseLost,
        )
        | None => None,
    };
    if let Some((pump, cause)) = pump_exit {
        record_io_pump_exit(pump, cause);
    }
    let cause = if !joined {
        IoExitCause::Deadline
    } else if owner_failed {
        IoExitCause::Panicked
    } else {
        match observer.stop_reason() {
            Some(StopReason::PeerClosed) => IoExitCause::PeerClosed,
            Some(StopReason::InboundIoFailure | StopReason::OutboundIoFailure) => {
                IoExitCause::IoError
            }
            Some(StopReason::ShutdownDeadline) => IoExitCause::Deadline,
            Some(
                StopReason::Requested
                | StopReason::InboundOverload
                | StopReason::OutboundOverload
                | StopReason::InboundQueueClosed
                | StopReason::OutboundQueueClosed
                | StopReason::PumpExited
                | StopReason::LeaseLost,
            ) => IoExitCause::Cancelled,
            None => IoExitCause::Abandoned,
        }
    };
    record_io_custodian_retirement(cause);
}

/// Channel state carried from the live loop into its connection-owned shutdown.
pub(super) struct ConnectionShutdownChannels<'a> {
    pub(super) outbound_rx: &'a mut mpsc::Receiver<OutboundStanza>,
    pub(super) handoff_rx: &'a mut OrderedRelayHandoffReceiver,
    pub(super) force_detach_rx:
        &'a mut Option<mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>>,
    pub(super) pending_force_detach: &'a mut Vec<waddle_xmpp::registry::ForceDetachRequest>,
}

/// Complete the connection-owned shutdown after the live loop stops.
/// Final socket writes bypass live-write interruption so a queued detach
/// request can still receive its bounded best-effort conflict close.
pub(super) async fn finish_connection<S, R, E>(
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    mut socket_io: LocalSocketIo<InterruptibleSink<S>, R>,
    authority: FrameAuthority<'_>,
    cause: RetirementCause,
    mut superseded: bool,
    channels: ConnectionShutdownChannels<'_>,
) where
    S: Sink<Message, Error = E> + Unpin,
    E: super::send::WebSocketWriteError,
{
    let FrameAuthority {
        permit: admission_permit,
        shutdown: shutdown_token,
    } = authority;
    let ConnectionShutdownChannels {
        outbound_rx,
        handoff_rx,
        force_detach_rx,
        pending_force_detach,
    } = channels;
    let domain = &state.deps.auth_state.xmpp_domain;

    // A blocked ready/flush can end the loop before its queued control arm
    // runs. Collect that request before any deferred old-client input can
    // mutate state that is about to be resumed elsewhere.
    if let Some(bound_bare) = conn.phase.bound_jid().map(|jid| jid.to_bare()) {
        let was_detaching = !pending_force_detach.is_empty();
        pending_force_detach.extend(drain_ready_force_detach_requests(
            force_detach_rx,
            &bound_bare,
            conn.binding_ref.as_ref(),
        ));
        if !was_detaching && !pending_force_detach.is_empty() {
            let (sender, _) = socket_io.halves_mut();
            close_live_session_for_force_detach(
                sender.uninterrupted(),
                conn,
                admission_permit,
                shutdown_token,
            )
            .await;
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
    // Skipped when superseded: running handlers from the stale session
    // could emit side effects (routing, inbound_count) against the replacement's
    // route. Terminal cleanup below still owes its generation-scoped MUC
    // departure; it does not execute these stale inbound handlers.
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
        // cleanup's superseded branch AFTER the ingress authority idle barrier
        // (`forget_terminal_ingress_stream_and_release_claim`). Deferring the
        // claim here, pre-drain, would hand the still-captured fence to the
        // release-retry janitor while admitted submissions are in flight.
    } else {
        settle_deferred_inbound_before_cleanup(
            domain,
            state,
            conn,
            admission_permit,
            shutdown_token,
            pending_force_detach,
        )
        .await;
        drain_ordered_relay_handoffs_before_cleanup(handoff_rx, conn).await;
    }

    // No later shutdown work uses either transport half. Release them before
    // potentially slow persistence and lifecycle cleanup, without treating the
    // local witness as evidence that cleanup or peer closure has completed.
    // Dropping them stops the pumps, so the transfer deadline starts here and
    // runs alongside the cleanup below.
    drop(socket_io);
    let pump_deadline = tokio::time::Instant::now() + TRANSFER_JOIN_DEADLINE;
    let pump_witness = conn.local_io_relinquishment.clone();
    // Set as soon as a waiter that asked for this binding during cleanup is
    // retained, so the transfer it implies is known before the lifecycle is
    // finished.
    let late_transfer = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Connection is ending. Decide between two paths:
    //   A. Fully clean up (unregister + remove MUC occupants) — the default
    //      for non-SM sessions and for SM sessions that didn't negotiate
    //      resume.
    //   B. Detach for resumption — for SM sessions with `resume='true'`,
    //      stash state into the SmSessionRegistry so a reconnecting client
    //      can authenticate and `<resume/>` without binding a new resource
    //      or re-joining MUC.
    //      MUC occupants stay in place during the detach window so other
    //      users continue to see this user as present.
    //
    // A superseded task preserves the replacement's registry slot but still
    // cleans its own MUC occupancy generation. If cleanup wins before a fresh
    // rejoin, peers see unavailable then available; if the rejoin wins, the
    // room rejects the stale departure. Successful SM resume retains the
    // original generation and follows the detach/resume continuity path.
    if !pending_force_detach.is_empty() {
        if let Some(bound_bare) = conn.phase.bound_jid().map(|jid| jid.to_bare()) {
            pending_force_detach.extend(drain_ready_force_detach_requests(
                force_detach_rx,
                &bound_bare,
                conn.binding_ref.as_ref(),
            ));
        }
    }
    let primary_origin = authoritative_force_detach_origin(pending_force_detach);
    release_stale_force_detach_waiters_before_cross_node_cleanup(
        pending_force_detach,
        primary_origin,
    );
    let cleanup_bare = conn.phase.cleanup_jid().map(|jid| jid.to_bare());
    let cleanup_binding = conn.binding_ref.clone();
    let cleanup = async {
        #[cfg(feature = "test-support")]
        if let Some(barrier) = &state
            .deps
            .app_state
            .websocket_admission_test_barriers
            .before_cleanup
        {
            barrier.arrive().await;
        }
        let outcome = if let Some(origin) = primary_origin {
            cleanup::cleanup_force_detach_connection_shutdown(
                state.as_ref(),
                outbound_rx,
                conn,
                superseded,
                origin,
            )
            .await
        } else {
            cleanup_connection_shutdown(state.as_ref(), outbound_rx, conn, superseded).await
        };
        finalize_replay_recorded_completions(state, conn, outcome);
        // Finishing the socket lifecycle releases a successor bind on this
        // node, and the acknowledgements below release a resumer. Neither may
        // happen for a transfer while the old pumps could still write.
        let pumps = observe_pump_termination(pump_witness.as_ref(), pump_deadline).await;
        let transfer = cause.is_transfer()
            || primary_origin.is_some()
            || superseded
            || late_transfer.load(std::sync::atomic::Ordering::Acquire);
        let fenced = pumps == PumpTermination::Missed && transfer;
        if fenced {
            fail_unjoined_transfer(state, conn);
        }
        cleanup::finish_socket_cleanup(state, conn, outcome).await;
        (outcome, pumps, fenced)
    };
    let ((shutdown_outcome, pumps, mut fenced), late_requests) =
        match (force_detach_rx.take(), cleanup_bare) {
            (Some(receiver), Some(bound_bare)) => {
                service_late_force_detach_waiters(
                    receiver,
                    bound_bare,
                    cleanup_binding,
                    primary_origin,
                    Arc::clone(&late_transfer),
                    cleanup,
                )
                .await
            }
            _ => (cleanup.await, Vec::new()),
        };
    // A waiter retained after the lifecycle check above still makes this a
    // transfer, and still must not be answered for an unobserved pump.
    if pumps == PumpTermination::Missed && !fenced && !late_requests.is_empty() {
        fail_unjoined_transfer(state, conn);
        fenced = true;
    }
    if fenced {
        // Dropping the acknowledgements reports an unknown outcome. The
        // waiters treat that as a session that is not known to be detached.
        pending_force_detach.clear();
        drop(late_requests);
    } else {
        // ADR-0017 Phase 3 Slice 6: only now — after this connection's own
        // detach-for-resume persistence has actually run above — tell the
        // cross-node resume asker it is safe to proceed with `steal_for_resume`.
        // Council-adjudicated FIX 4: ack only what actually happened — map the
        // typed `shutdown_outcome` onto the wire outcome rather than always
        // claiming `Detached`, so the asker never proceeds with
        // `steal_for_resume` against a snapshot that was never persisted (a
        // storage-error fallback, an ownership-race promotion, or any other
        // non-detach cleanup path).
        ack_force_detach_requests(std::mem::take(pending_force_detach), shutdown_outcome);
        // If ordinary settlement began without a detach request, a later request
        // cannot retroactively select force cleanup or prove its conflict close.
        let late_outcome = if primary_origin.is_some() {
            shutdown_outcome
        } else {
            cleanup::ConnectionShutdownOutcome::NotPersisted
        };
        ack_force_detach_requests(late_requests, late_outcome);
    }

    #[cfg(feature = "test-support")]
    if let Some(barrier) = &state
        .deps
        .app_state
        .websocket_admission_test_barriers
        .after_cleanup
    {
        barrier.arrive().await;
    }
    info!("XMPP WebSocket connection closed");
}

async fn close_live_session_for_node_unavailable<S, E>(ws_sender: &mut S, conn: &WsConnState)
where
    S: Sink<Message, Error = E> + Unpin,
    E: super::send::WebSocketWriteError,
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

/// Take the terminal failure path for a transfer whose old pumps were not
/// seen to stop before the deadline.
fn fail_unjoined_transfer(state: &WebSocketState, conn: &WsConnState) {
    error!(
        jid = ?conn.phase.cleanup_jid(),
        connection_ref = ?conn.connection_ref,
        deadline_ms = TRANSFER_JOIN_DEADLINE.as_millis(),
        "Transport pumps of a transferred binding were not seen to stop; failing the node"
    );
    fence_unjoined_transfer(
        &state.deps.app_state.node_lifecycle,
        &state.deps.shutdown.stop_token(),
    );
}

/// Close a live stream displaced by an XEP-0198 resume. The
/// conflict wire frames are deliberately gated on a committed live stream;
/// regardless of that gate the WebSocket transport is closed.
async fn close_live_session_for_force_detach<S, E>(
    ws_sender: &mut S,
    conn: &WsConnState,
    admission_permit: &NodeAdmissionPermit,
    shutdown_token: &tokio_util::sync::CancellationToken,
) where
    S: Sink<Message, Error = E> + Unpin,
    E: super::send::WebSocketWriteError,
{
    // XEP-0198 §5 recommends conflict closure, but a black-holed transport
    // must not delay detach persistence and recovery. The socket is discarded
    // after this attempt, so cancelling a pending write is safe.
    let _ = tokio::time::timeout(FORCE_DETACH_CONFLICT_CLOSE_TIMEOUT, async {
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
    })
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
    binding: Option<&waddle_xmpp::ownership::BindingRef>,
) -> Vec<waddle_xmpp::registry::ForceDetachRequest> {
    let mut drained = Vec::new();
    let Some(receiver) = rx.as_mut() else {
        return drained;
    };
    loop {
        match receiver.try_recv() {
            Ok(request) => {
                if !request.targets_binding(binding) {
                    debug!("force-detach ignored while draining: binding retired after enqueue");
                    continue;
                }
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

/// Fresh binding terminally retires an occupancy, even when a resume was also
/// queued. Otherwise cross-node semantics win whenever ANY request carries
/// that origin, regardless of queue order: a stale-retirement request drained
/// ahead of a cross-node-resume one must not make cleanup skip the synchronous
/// cross-node unregister fence while both waiters are acknowledged from the
/// same outcome — the remote resumer would race the still-pending removal and
/// reject a valid resume.
pub(super) fn authoritative_force_detach_origin(
    requests: &[waddle_xmpp::registry::ForceDetachRequest],
) -> Option<waddle_xmpp::registry::ForceDetachOrigin> {
    if requests.iter().any(|request| {
        request.origin == waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement
    }) {
        return Some(waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement);
    }
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
    if !matches!(
        primary_origin,
        Some(
            waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume
                | waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement
        )
    ) {
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

#[cfg(test)]
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
            let (_, requests) = service_late_force_detach_waiters(
                receiver,
                bound_bare,
                None,
                Some(primary_origin),
                Arc::default(),
                cancel_task.cancelled(),
            )
            .await;
            requests
        }),
    ))
}

/// Service the captured entry's control channel while the connection itself
/// completes cleanup. Keeping both futures in this task closes the receiver
/// if connection cleanup is cancelled rather than leaving an orphan waiter.
async fn service_late_force_detach_waiters<F>(
    mut receiver: mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>,
    bound_bare: jid::BareJid,
    binding: Option<waddle_xmpp::ownership::BindingRef>,
    primary_origin: Option<waddle_xmpp::registry::ForceDetachOrigin>,
    retained: Arc<std::sync::atomic::AtomicBool>,
    cleanup: F,
) -> (F::Output, Vec<waddle_xmpp::registry::ForceDetachRequest>)
where
    F: std::future::Future,
{
    tokio::pin!(cleanup);
    let mut pending = Vec::new();
    let outcome = loop {
        tokio::select! {
            biased;
            outcome = &mut cleanup => break outcome,
            request = receiver.recv() => {
                let Some(request) = request else {
                    break cleanup.await;
                };
                handle_late_force_detach_waiter_during_cross_node_cleanup(
                    request,
                    &bound_bare,
                    binding.as_ref(),
                    primary_origin,
                    &mut pending,
                );
                if !pending.is_empty() {
                    retained.store(true, std::sync::atomic::Ordering::Release);
                }
            }
        }
    };
    // Closing first rejects new sends. Drain what is already published, then
    // drop the receiver rather than waiting indefinitely for a held permit.
    receiver.close();
    drain_late_force_detach_waiters_during_cross_node_cleanup(
        &mut receiver,
        &bound_bare,
        binding.as_ref(),
        primary_origin,
        &mut pending,
    );
    (outcome, pending)
}

fn drain_late_force_detach_waiters_during_cross_node_cleanup(
    receiver: &mut mpsc::Receiver<waddle_xmpp::registry::ForceDetachRequest>,
    bound_bare: &jid::BareJid,
    binding: Option<&waddle_xmpp::ownership::BindingRef>,
    primary_origin: Option<waddle_xmpp::registry::ForceDetachOrigin>,
    pending: &mut Vec<waddle_xmpp::registry::ForceDetachRequest>,
) {
    loop {
        match receiver.try_recv() {
            Ok(request) => handle_late_force_detach_waiter_during_cross_node_cleanup(
                request,
                bound_bare,
                binding,
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
    binding: Option<&waddle_xmpp::ownership::BindingRef>,
    primary_origin: Option<waddle_xmpp::registry::ForceDetachOrigin>,
    pending: &mut Vec<waddle_xmpp::registry::ForceDetachRequest>,
) {
    if !request.targets_binding(binding) {
        debug!("force-detach ignored during cleanup: binding retired after enqueue");
        return;
    }
    if request.requester_bare_jid != *bound_bare {
        warn!("force-detach rejected during cleanup: identity mismatch");
        let _ = request
            .ack
            .send(waddle_xmpp::registry::ForceDetachOutcome::IdentityMismatch);
        return;
    }

    let Some(primary_origin) = primary_origin else {
        // Ordinary cleanup has already chosen its semantics. The request is
        // inconclusive, but its caller may interpret an ACK as socket
        // retirement, so retain it until lifecycle cleanup finishes.
        pending.push(request);
        return;
    };

    if matches!(
        primary_origin,
        waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume
            | waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement
    ) && request.origin == waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement
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
    use axum::body::Body;
    use axum::http::Request;
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
        use crate::websocket_transport::SocketAdmissionError;
        let state = super::super::tests::create_test_websocket_state().await;
        let app = &state.deps.app_state;
        let admission = app
            .websocket_admission_gate()
            .expect("gate")
            .reserve(state.deps.shutdown.stop_token())
            .expect("initial admission");

        app.node_lifecycle.begin_fenced_recovery();
        assert!(matches!(
            admission.revalidate(),
            Err(SocketAdmissionError::Lifecycle(
                crate::clustering::NodeAdmissionError::NotServing(
                    crate::clustering::NodeAdmission::FencedRecovering
                )
            ))
        ));
        app.node_lifecycle.serve();
        assert!(matches!(
            admission.revalidate(),
            Err(SocketAdmissionError::Lifecycle(
                crate::clustering::NodeAdmissionError::Revoked
            ))
        ));
        let recovered = app
            .websocket_admission_gate()
            .expect("recovered gate")
            .reserve(state.deps.shutdown.stop_token())
            .expect("recovered admission");
        app.node_lifecycle
            .fail(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated);
        assert!(matches!(
            recovered.revalidate(),
            Err(SocketAdmissionError::Lifecycle(
                crate::clustering::NodeAdmissionError::Revoked
            ))
        ));
    }

    #[tokio::test]
    async fn ordinary_process_stop_revokes_upgrade_without_latching_critical_failure() {
        let state = super::super::tests::create_test_websocket_state().await;
        let admission = state
            .deps
            .app_state
            .websocket_admission_gate()
            .expect("gate")
            .reserve(state.deps.shutdown.stop_token())
            .expect("initial admission");
        state.deps.shutdown.stop_token().cancel();
        assert!(matches!(
            admission.revalidate(),
            Err(crate::websocket_transport::SocketAdmissionError::Cancelled)
        ));
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
pub(super) async fn handle_inbound_text<S, SE, R, RE>(
    text: &str,
    domain: &str,
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    channels: RegistrationChannels<'_>,
    io: ConnectionIo<'_, S, R>,
    authority: FrameAuthority<'_>,
) -> bool
where
    S: futures::Sink<Message, Error = SE> + Unpin,
    SE: super::send::WebSocketWriteError,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin + SettleInbound,
    RE: std::fmt::Display,
{
    #[cfg(feature = "test-support")]
    if let Some(barrier) = &state
        .deps
        .app_state
        .websocket_admission_test_barriers
        .in_inbound_handler
    {
        barrier.arrive().await;
    }
    let RegistrationChannels {
        pending_tx,
        outbound_receiver,
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
    let responses = handle_xmpp_frame_with_admission(
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

    complete_inbound_turn(
        responses,
        domain,
        state,
        conn,
        RegistrationChannels {
            pending_tx,
            outbound_receiver,
            force_detach_rx,
        },
        ConnectionIo {
            sender: ws_sender,
            receiver: ws_receiver,
        },
        FrameAuthority {
            permit: admission_permit,
            shutdown: shutdown_token,
        },
    )
    .await
}

/// Finish a turn whose handler produced `responses`: register a connection
/// the handler bound or resumed, finalize stream management for it, write
/// the batch and settle what it carried. Returns whether the loop continues.
async fn complete_inbound_turn<S, SE, R, RE>(
    mut responses: ResponseBatch,
    domain: &str,
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    channels: RegistrationChannels<'_>,
    io: ConnectionIo<'_, S, R>,
    authority: FrameAuthority<'_>,
) -> bool
where
    S: futures::Sink<Message, Error = SE> + Unpin,
    SE: super::send::WebSocketWriteError,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin + SettleInbound,
    RE: std::fmt::Display,
{
    let RegistrationChannels {
        pending_tx,
        outbound_receiver,
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
    // A SASL2 exchange that authenticated with inline requests runs its
    // resume, or binds its resource, before registration looks at the phase.
    super::sasl::advance_sasl2_inline(state.as_ref(), conn).await;

    // Mirror any phase transition `handle_xmpp_frame` performed (most
    // importantly Ready → Closing on SASL failure / stream error)
    // into the per-connection state machine. Without this, late
    // `PeerStanza` dispatches from the outbound channel would still
    // go through the recipient pipeline even though the legacy phase
    // tracker has marked the connection Closing.
    conn.sync_state_machine_phase();

    // What the frame changed of the resource's presence, interests or
    // stream-management reference reaches the account coordinator before
    // the next frame is read.
    super::account_binding::sync_snapshot(state.as_ref(), conn).await;

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
        outbound_receiver,
    )
    .await
    {
        RegistrationAfterFrame::Unchanged => {}
        failure @ (RegistrationAfterFrame::SessionInitializationFailed
        | RegistrationAfterFrame::RegistrationTemporarilyUnavailable) => {
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
            let stream_error = if matches!(
                failure,
                RegistrationAfterFrame::RegistrationTemporarilyUnavailable
            ) {
                // RFC 6120 §4.9.3.17: the actor/claim handoff exhausted its
                // bounded capacity for this stream. A fresh stream may retry.
                super::transport_xml::element_to_xml(
                    xmpp_parsers::stream_error::StreamError::new(
                        xmpp_parsers::stream_error::DefinedCondition::ResourceConstraint,
                        "en",
                        "Session registration is temporarily unavailable; please reconnect.",
                    )
                    .into(),
                )
            } else {
                build_internal_server_error_stream_error(
                    "Session initialization failed; please reconnect.",
                )
            };
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
            // Owner-gated: a superseded older registration must not steal
            // the REPLACEMENT entry's receiver — signals for the new socket
            // would be consumed by (or die with) the old one.
            if let (Some(jid), Some(owner)) = (conn.phase.bound_jid(), conn.registry_owner.as_ref())
            {
                if let Some(entry) = state
                    .deps
                    .protocol
                    .connection_registry
                    .entry_if_owner(jid, owner)
                {
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
                    responses.frames.extend(super::resume_replay::replay_frames(
                        &conn.sm_state,
                        replay_after_h,
                        state.deps.auth_state.xmpp_domain.as_str(),
                    ));
                    // W1.6c seam: queue a live stanza behind the unwritten replay.
                    #[cfg(feature = "test-support")]
                    super::characterization_seams::hold_until_live_stanza_queued(
                        state,
                        conn.phase.bound_jid(),
                        conn.registry_owner.as_ref(),
                        super::characterization_seams::HeldBatch::SmResumed,
                    )
                    .await;
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

    // A durable resume that committed but whose endpoint registration did
    // not run is returned to the authority, and the client is told it failed.
    if super::stream_management::abandon_unpublished_resume(state.as_ref(), conn).await {
        responses = ResponseBatch::from_frames(vec![
            waddle_xmpp::stream_management::SmFailed::with_condition("internal-server-error")
                .to_element(),
        ]);
        conn.pending_finalized_resume_outcome =
            Some(waddle_xmpp::telemetry::attributes::SmResumeOutcome::Internal);
    }

    // A staged SASL2 `<success/>` is written once its inline results are
    // known: after the resume it reports was completed, or after the resource
    // it reports as bound was registered.
    super::sasl::finish_sasl2_inline(state.as_ref(), conn, &mut responses).await;

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
    let write_report = write_ingress_response_batch_with_admission(
        ws_sender,
        ws_receiver,
        state.as_ref(),
        conn,
        &mut responses,
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
            // The batch that carried a bind result or `<resumed/>` is on the
            // wire, so the binding it answered is acknowledged.
            conn.confirm_installed_binding();
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
            inbound_h = conn.sm_state.inbound_count(),
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
    // A durable session this binding no longer owns ends the loop; the
    // retirement names the cause and writes the conflict close.
    !conn.durable_sm.is_superseded()
}

async fn close_if_frame_authority_revoked<S, E>(
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    ws_sender: &mut S,
    permit: &NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> bool
where
    S: futures::Sink<Message, Error = E> + Unpin,
    E: super::send::WebSocketWriteError,
{
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
    // never send `<enabled/>`. A session the durable authority already
    // committed for that reply is closed, since no client can name it.
    if conn.pending_sm_enable_commit.take().is_some() {
        super::stream_management::close_unannounced_session(&mut conn.durable_sm).await;
    }
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
    completion: OrderedRelayHandoffCompletion,
    permit: &NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> bool
where
    S: futures::Sink<Message, Error = SE> + Unpin,
    SE: super::send::WebSocketWriteError,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin + SettleInbound,
    RE: std::fmt::Display,
{
    if shutdown.is_cancelled() || permit.revalidate().is_err() {
        conn.sm_inbound_completion
            .abandon(completion.inbound_sequence);
        return false;
    }
    conn.sm_inbound_completion
        .complete(completion.inbound_sequence, &mut conn.sm_state);
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
    handoff_rx: &mut OrderedRelayHandoffReceiver,
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
    completion: OrderedRelayHandoffCompletion,
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

/// Settle parked frames before cleanup using the force-detach operation that
/// owns the teardown and the connection's current dispatch authority.
async fn settle_deferred_inbound_before_cleanup(
    domain: &str,
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    permit: &NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
    pending_force_detach: &[waddle_xmpp::registry::ForceDetachRequest],
) {
    // A claim-loss teardown (the stream's live fence is already gone —
    // demotion or terminal release) gets the same treatment as node
    // authority revocation: dispatching the parked frames would run
    // them fenceless, and the terminal recovery below rejects
    // resumption, so the client retransmits them anyway (XEP-0198) —
    // processing here would double their non-idempotent effects. No
    // pending force-detach prerequisite: an ordinary or keepalive close
    // can win the race before the demotion signal is consumed.
    let fenceless_teardown = conn.sm_state.is_resumable()
        && conn.sm_state.stream_id.as_deref().is_some_and(|stream_id| {
            state
                .deps
                .protocol
                .sm_session_registry
                .current_sm_claim_fence(stream_id)
                .is_none()
        });
    // Only resume takeovers leave these unhandled frames for client replay.
    // Fresh binds and retirement still owe their already-received input, even
    // when a resume request is also queued behind an authoritative fresh bind.
    if authoritative_force_detach_origin(pending_force_detach)
        == Some(waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume)
        || shutdown.is_cancelled()
        || permit.revalidate().is_err()
        || fenceless_teardown
    {
        let dropped = discard_deferred_inbound(conn);
        if dropped > 0 {
            info!(
                dropped,
                "Dropping deferred inbound frames for resume takeover or after authority or claim loss; sender may replay them"
            );
        }
    } else {
        process_deferred_inbound_after_transport_loss(domain, state, conn, permit, shutdown).await;
    }
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
    permit: &NodeAdmissionPermit,
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
        BatchSmPolicy::Record | BatchSmPolicy::RecordAt(_) => {
            BatchCompletionOutcome::RetainedForRecovery
        }
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
) -> Vec<(RoomEffectCompletion, usize)> {
    responses
        .completions
        .into_iter()
        .zip(responses.completion_frame_indices)
        .collect()
}

fn complete_batch_completions(state: &Arc<WebSocketState>, completions: Vec<RoomEffectCompletion>) {
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
    completion_frames: Vec<(RoomEffectCompletion, usize)>,
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
    completions: Vec<RoomEffectCompletion>,
) {
    let mut queued: HashSet<_> = conn
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
    completion_frames: Vec<(RoomEffectCompletion, usize)>,
) -> (Vec<RoomEffectCompletion>, usize) {
    let accepted_frame_indices: HashSet<_> = accepted_frame_indices.iter().copied().collect();
    let mut grouped = HashMap::new();
    for (completion, frame_index) in completion_frames {
        let completion_key = (completion.key.clone(), completion.lease.clone());
        grouped
            .entry(completion_key)
            .and_modify(
                |(_, all_frames_accepted): &mut (RoomEffectCompletion, bool)| {
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
    use minidom::Element;

    use super::super::batch_write::{
        write_response_batch_with_admission, BatchAuthority, BatchSmPolicy, BatchWriteOutcome,
    };
    use super::super::cleanup::cleanup_connection_shutdown;
    use super::super::state::TERMINAL_RECOVERY_QUEUE_CAP;
    use super::super::transport_xml::websocket_stream_open_xml;
    use super::*;
    use jid::{BareJid, FullJid};
    use std::pin::Pin;
    use std::str::FromStr;
    use std::task::{Context, Poll};
    use tokio::sync::oneshot;
    use waddle_xmpp::stream_management::SmSessionRegistry;

    /// The pump queues are bounded by the frame limits the connection
    /// already has, and every bound is one the pump accepts.
    #[test]
    fn pump_queue_byte_bounds_follow_the_frame_limits() {
        let inbound = Limits::new(TRANSPORT_INBOUND_FRAMES, TRANSPORT_INBOUND_QUEUE_BYTES)
            .expect("inbound bound");
        let outbound = Limits::new(TRANSPORT_OUTBOUND_FRAMES, TRANSPORT_OUTBOUND_QUEUE_BYTES)
            .expect("outbound bound");
        // One frame of the largest message the WebSocket layer delivers.
        assert_eq!(inbound.max_count(), 1);
        assert_eq!(inbound.max_bytes(), WEBSOCKET_MAX_MESSAGE_BYTES);
        // One data frame of that size, and room for the terminal sequence.
        assert_eq!(
            outbound.max_bytes(),
            WEBSOCKET_MAX_MESSAGE_BYTES
                + (outbound.max_count() - 1) * crate::server::routes::websocket::MAX_FRAME_SIZE
        );
        // The pump charges a frame's bytes as a 32-bit permit count.
        assert!(u32::try_from(inbound.max_bytes()).is_ok());
        assert!(u32::try_from(outbound.max_bytes()).is_ok());
    }

    #[tokio::test]
    async fn panic_fence_signals_stop_before_waiting_for_admission_lock() {
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let probe = permit.clone();
        let (entered, entered_rx) = oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let active_send = std::thread::spawn(move || {
            permit
                .with_current(|| {
                    let _ = entered.send(());
                    release_rx.recv().expect("release send");
                })
                .expect("admitted send");
        });
        entered_rx.await.expect("send holds lifecycle read lock");

        let stop = CancellationToken::new();
        let signal_lifecycle = lifecycle.clone();
        let signal_stop = stop.clone();
        let fence = tokio::task::spawn_blocking(move || {
            fence_panicked_owner(&signal_lifecycle, &signal_stop);
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), stop.cancelled())
            .await
            .expect("stop is signalled while the send holds its lock");
        assert!(lifecycle.fatal_fence_token().is_cancelled());
        assert_eq!(
            lifecycle.critical_failure(),
            Some(crate::clustering::CriticalNodeFailure::WebSocketOwnerPanicked)
        );
        assert_eq!(
            lifecycle.admission(),
            crate::clustering::NodeAdmission::Failed(
                crate::clustering::CriticalNodeFailure::WebSocketOwnerPanicked
            )
        );
        assert!(!lifecycle.is_ready());
        assert!(lifecycle.admit().is_err());
        assert!(probe.revalidate().is_err());
        let started = std::sync::atomic::AtomicBool::new(false);
        assert!(probe
            .start_send_if_current(|| {
                started.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<(), ()>(())
            })
            .is_err());
        assert!(!started.load(std::sync::atomic::Ordering::SeqCst));
        tokio::time::timeout(std::time::Duration::from_secs(1), probe.revoked())
            .await
            .expect("fatal fence wakes an existing permit");
        let status_lifecycle = lifecycle.clone();
        let status = tokio::task::spawn_blocking(move || {
            crate::server::lifecycle::validate_node_shutdown(&status_lifecycle)
        });
        let status = tokio::time::timeout(std::time::Duration::from_secs(1), status)
            .await
            .expect("final status does not wait for the pending failure latch")
            .expect("status task")
            .expect_err("panic cannot report successful shutdown");
        assert!(status.to_string().contains("WebSocketOwnerPanicked"));
        assert!(
            !fence.is_finished(),
            "failure latch still waits for the send"
        );
        release.send(()).expect("release send");
        active_send.join().expect("send thread");
        fence.await.expect("panic fence completes");
        lifecycle.serve();
        assert!(!lifecycle.is_ready());
        assert_eq!(
            lifecycle.critical_failure(),
            Some(crate::clustering::CriticalNodeFailure::WebSocketOwnerPanicked)
        );
    }

    #[test]
    fn shutdown_status_keeps_existing_critical_causes() {
        for failure in [
            crate::clustering::CriticalNodeFailure::RoomRegistryTerminated,
            crate::clustering::CriticalNodeFailure::UserRegistryTerminated,
        ] {
            let lifecycle = crate::clustering::NodeLifecycle::new();
            lifecycle.fail(failure);
            let error = crate::server::lifecycle::validate_node_shutdown(&lifecycle)
                .expect_err("critical node failure");
            assert!(error.to_string().contains(&format!("{failure:?}")));
        }
        let lifecycle = crate::clustering::NodeLifecycle::new();
        assert!(crate::server::lifecycle::validate_node_shutdown(&lifecycle).is_ok());
        lifecycle.fatal_fence_token().cancel();
        assert!(crate::server::lifecycle::validate_node_shutdown(&lifecycle).is_err());
    }

    #[tokio::test]
    async fn unfinished_live_resume_returns_when_socket_owner_drops() {
        let conn = WsConnState::new();
        let (outbound, _outbound_rx) = mpsc::channel(1);
        let binding_ref = waddle_xmpp::ownership::BindingRef::fresh(
            waddle_xmpp::ownership::ConnectionRef::fresh(
                waddle_xmpp::ownership::NodeLeaseIdentity::local_process(),
            ),
        );
        let entry =
            waddle_xmpp::registry::ConnectionEntry::new_for_binding(outbound, binding_ref.clone());
        let owner: FullJid = "alice@example.test/old".parse().expect("full JID");
        assert!(entry.wire_resume_control(owner.clone(), conn.live_resume_control.clone()));
        let guard = LiveResumeOwnerGuard(conn.live_resume_control.clone());
        let request = conn.live_resume_control.request(
            binding_ref,
            waddle_xmpp::pending_delivery::SmSessionId::try_from_wire("old-stream")
                .expect("valid stream ID"),
            owner.to_bare(),
            0,
        );
        tokio::pin!(request);
        assert!(futures::poll!(request.as_mut()).is_pending());

        drop(guard);
        assert_eq!(
            request.await,
            waddle_xmpp::registry::LocalResumeReply::NotPersisted
        );
        assert!(!conn.write_interruption.is_cancelled());
    }

    #[tokio::test]
    async fn stale_binding_requests_do_not_enter_initial_or_late_cleanup() {
        use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
        use waddle_xmpp::registry::{ForceDetachOrigin, ForceDetachRequest};

        let connection = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
        let old = BindingRef::fresh(connection.clone());
        let current = BindingRef::fresh(connection);
        let bare: BareJid = "alice@example.com".parse().expect("bare JID");
        let (sender, receiver) = mpsc::channel(2);
        let mut receiver = Some(receiver);
        let (ack, reply) = oneshot::channel();
        sender
            .try_send(ForceDetachRequest {
                origin: ForceDetachOrigin::FreshBindReplacement,
                target_binding: Some(old.clone()),
                requester_bare_jid: bare.clone(),
                ack,
            })
            .expect("queue old request");
        assert!(drain_ready_force_detach_requests(&mut receiver, &bare, Some(&current)).is_empty());
        assert!(
            reply.await.is_err(),
            "stale request must not acknowledge cleanup"
        );

        let (ack, reply) = oneshot::channel();
        sender
            .try_send(ForceDetachRequest {
                origin: ForceDetachOrigin::FreshBindReplacement,
                target_binding: Some(old),
                requester_bare_jid: bare.clone(),
                ack,
            })
            .expect("queue old late request");
        let (finish, finished) = oneshot::channel::<()>();
        let task = tokio::spawn(service_late_force_detach_waiters(
            receiver.take().expect("receiver"),
            bare,
            Some(current),
            Some(ForceDetachOrigin::FreshBindReplacement),
            Arc::default(),
            async move { finished.await.expect("finish cleanup") },
        ));
        assert!(
            reply.await.is_err(),
            "stale late request must not acknowledge cleanup"
        );
        finish.send(()).expect("finish cleanup");
        let (_, pending) = task.await.expect("late waiter task");
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn late_waiter_cutoff_is_bounded_with_an_outstanding_permit() {
        use waddle_xmpp::registry::{ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest};

        let bare: BareJid = "alice@example.com".parse().expect("bare JID");
        let (sender, receiver) = mpsc::channel(2);
        let (queued_ack, queued_reply) = oneshot::channel();
        sender
            .send(ForceDetachRequest {
                origin: ForceDetachOrigin::CrossNodeResume,
                target_binding: None,
                requester_bare_jid: bare.clone(),
                ack: queued_ack,
            })
            .await
            .expect("queue before cutoff");
        let permit = sender.reserve().await.expect("reserve before cutoff");

        let (_, pending) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service_late_force_detach_waiters(
                receiver,
                bare.clone(),
                None,
                None,
                Arc::default(),
                async {},
            ),
        )
        .await
        .expect("cutoff must not wait for an outstanding permit");
        assert!(sender.is_closed());
        ack_force_detach_requests(pending, cleanup::ConnectionShutdownOutcome::NotPersisted);
        assert_eq!(
            queued_reply.await.expect("queued reply"),
            ForceDetachOutcome::NotPersisted
        );

        let (late_ack, late_reply) = oneshot::channel();
        permit.send(ForceDetachRequest {
            origin: ForceDetachOrigin::CrossNodeResume,
            target_binding: None,
            requester_bare_jid: bare,
            ack: late_ack,
        });
        // Tokio may retain a value sent through a pre-close permit until the
        // last sender is dropped, even though no receiver can observe it.
        drop(sender);
        assert!(
            late_reply.await.is_err(),
            "unobserved permit cannot claim completion"
        );
    }

    #[tokio::test]
    async fn late_waiters_in_ordinary_cleanup_retain_each_origin_until_completion() {
        use waddle_xmpp::registry::{ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest};

        let bare: BareJid = "alice@example.com".parse().expect("bare JID");
        let (sender, receiver) = mpsc::channel(8);
        let (finish, finished) = oneshot::channel::<()>();
        let task = tokio::spawn(service_late_force_detach_waiters(
            receiver,
            bare.clone(),
            None,
            None,
            Arc::default(),
            async move { finished.await.expect("complete cleanup") },
        ));
        let mut replies = Vec::new();
        for origin in [
            ForceDetachOrigin::CrossNodeResume,
            ForceDetachOrigin::FreshBindReplacement,
            ForceDetachOrigin::RegistryStaleActorRetirement,
            ForceDetachOrigin::OwnerManagedRetirement,
        ] {
            let (ack, reply) = oneshot::channel();
            sender
                .send(ForceDetachRequest {
                    origin,
                    target_binding: None,
                    requester_bare_jid: bare.clone(),
                    ack,
                })
                .await
                .expect("queue late request");
            replies.push(reply);
        }
        let (mismatch_ack, mismatch_reply) = oneshot::channel();
        sender
            .send(ForceDetachRequest {
                origin: ForceDetachOrigin::CrossNodeResume,
                target_binding: None,
                requester_bare_jid: "other@example.com".parse().expect("other bare JID"),
                ack: mismatch_ack,
            })
            .await
            .expect("queue mismatch after matching requests");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), mismatch_reply)
                .await
                .expect("mismatch is serviced")
                .expect("mismatch reply"),
            ForceDetachOutcome::IdentityMismatch
        );
        for reply in &mut replies {
            assert!(matches!(
                reply.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
        }

        finish.send(()).expect("finish cleanup");
        let (_, pending) = task.await.expect("service completes");
        assert_eq!(pending.len(), 4);
        ack_force_detach_requests(pending, cleanup::ConnectionShutdownOutcome::NotPersisted);
        for reply in replies {
            assert_eq!(
                reply.await.expect("late reply"),
                ForceDetachOutcome::NotPersisted
            );
        }
    }

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
        crate::room_effect_outbox::RoomEffectOriginInstanceId::try_new(
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

        close_unstarted_upgraded_socket(&mut socket).await;

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
    async fn force_detach_does_not_wait_for_a_blocked_conflict_write() {
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.begin_server_stream_open_response();
        conn.commit_server_stream_open_response();
        let mut sender = Box::pin(futures::sink::unfold((), |(), _message: Message| async {
            futures::future::pending::<Result<(), &'static str>>().await
        }));

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            close_live_session_for_force_detach(&mut sender, &conn, &permit, &shutdown),
        )
        .await
        .expect("optional conflict writes must not block SM detach cleanup");
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
            target_binding: None,
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
    async fn late_stale_waiter_is_released_during_fresh_replacement_cleanup() {
        use waddle_xmpp::registry::{ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest};

        let bare_jid = BareJid::from_str("late-fresh@example.com").unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut rx = Some(rx);
        let (cancel, task) = start_late_force_detach_waiter_service(
            &mut rx,
            bare_jid.clone(),
            ForceDetachOrigin::FreshBindReplacement,
        )
        .expect("late waiter service");
        let (resume_tx, resume_rx) = oneshot::channel();
        tx.send(ForceDetachRequest {
            origin: ForceDetachOrigin::CrossNodeResume,
            target_binding: None,
            requester_bare_jid: bare_jid.clone(),
            ack: resume_tx,
        })
        .await
        .unwrap();
        let (stale_tx, stale_rx) = oneshot::channel();
        tx.send(ForceDetachRequest {
            origin: ForceDetachOrigin::RegistryStaleActorRetirement,
            target_binding: None,
            requester_bare_jid: bare_jid,
            ack: stale_tx,
        })
        .await
        .unwrap();

        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), stale_rx)
                .await
                .expect("release stale actor turn during fresh cleanup")
                .unwrap(),
            ForceDetachOutcome::NotPersisted,
        );
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), resume_rx)
                .await
                .expect("late resume fails closed during terminal cleanup")
                .unwrap(),
            ForceDetachOutcome::NotPersisted,
            "fresh replacement cannot promise a resumable detached snapshot",
        );
        cancel.cancel();
        assert!(
            task.await.unwrap().is_empty(),
            "both late waiters were answered inline"
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
            target_binding: None,
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
            target_binding: None,
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
            target_binding: None,
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
            target_binding: None,
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
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("stream-timeout".to_string(), true, Some(300));
        let abandoned = conn.sm_inbound_completion.reserve(&conn.sm_state);
        conn.sm_inbound_completion.abandon(abandoned);

        // Keep the sender alive: the abandoned sequence itself must not remain
        // pending and make cleanup wait for a completion that cannot arrive.
        let (_tx, mut rx) = crate::server::routes::interpret::ordered_relay_handoff_channel();
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            drain_ordered_relay_handoffs_before_cleanup(&mut rx, &mut conn),
        )
        .await
        .expect("abandoned sequence must not block cleanup");

        assert_eq!(conn.sm_state.inbound_count(), 0);
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
        let (handoff_tx, mut handoff_rx) =
            crate::server::routes::interpret::ordered_relay_handoff_channel();
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
            drain_ordered_relay_handoffs_before_cleanup(&mut handoff_rx, &mut conn),
        )
        .await
        .expect("abandoned consumed completion must not block cleanup");

        assert_eq!(conn.sm_state.inbound_count(), 0);
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

        let (tx, mut rx) = crate::server::routes::interpret::ordered_relay_handoff_channel();
        let completion = crate::server::routes::interpret::OrderedRelayHandoffCompletion {
            inbound_sequence: earlier,
            replies: Vec::new(),
        };
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            tx.try_send(completion)
                .expect("cleanup receiver remains open");
        });

        drain_ordered_relay_handoffs_before_cleanup(&mut rx, &mut conn).await;

        assert_eq!(
            conn.sm_state.inbound_count(),
            1,
            "the contiguous pre-hole stanza must be acknowledged before detach"
        );
        assert!(!conn.sm_inbound_completion.has_pending());
    }

    #[test]
    fn terminal_recovery_handoff_replies_bypass_the_capped_sm_queue() {
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
            conn.sm_state.stanzas_to_resend(0).len(),
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
        {
            let registry = &state.deps.protocol.sm_session_registry;
            let _authority = registry
                .ensure_session_claim("terminal-cap")
                .await
                .expect("enabled stream claim");
            conn.sm_ingress_fence = registry.current_sm_claim_fence("terminal-cap");
        }
        conn.begin_terminal_sm_recovery();
        for sequence in 0..TERMINAL_RECOVERY_QUEUE_CAP {
            let mut prefix = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.clone())));
            prefix.from = Some("bob@example.com/phone".parse().expect("peer sender"));
            prefix.type_ = xmpp_parsers::message::MessageType::Chat;
            prefix.bodies.insert(
                xmpp_parsers::message::Lang::new(),
                "recorded peer prefix".to_owned(),
            );
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
        let (_tx, mut rx) = crate::server::routes::interpret::ordered_relay_handoff_channel();
        let mut drain = Box::pin(drain_ordered_relay_handoffs_before_cleanup(
            &mut rx, &mut conn,
        ));
        assert!(futures::poll!(drain.as_mut()).is_pending());

        tokio::time::advance(ORDERED_RELAY_HANDOFF_CLEANUP_DEADLINE).await;
        drain.await;

        assert_eq!(conn.sm_state.inbound_count(), 0);
        assert!(!conn.sm_inbound_completion.has_pending());
    }

    #[tokio::test]
    async fn cleanup_work_cap_bounds_ready_pre_hole_backlog() {
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("stream-timeout".to_string(), true, Some(300));
        // A queue wider than a connection's own bound, so the drain's work cap
        // is what stops it.
        let (tx, mut rx) =
            tokio::sync::mpsc::channel(ORDERED_RELAY_HANDOFF_CLEANUP_MAX_COMPLETIONS + 1);

        for _ in 0..=ORDERED_RELAY_HANDOFF_CLEANUP_MAX_COMPLETIONS {
            let sequence = conn.sm_inbound_completion.reserve(&conn.sm_state);
            tx.try_send(
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
            conn.sm_state.inbound_count(),
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
        assert_eq!(conn.sm_state.inbound_count(), 0);
    }

    async fn assert_deferred_force_detach_handling(
        origins: &[waddle_xmpp::registry::ForceDetachOrigin],
        resumable: bool,
        expected_handled: u32,
    ) {
        let state = super::super::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.phase = ConnectionPhase::ready(
            "alice@example.com/deferred".parse().expect("bound JID"),
            false,
        );
        conn.sm_state
            .enable("deferred-force-detach".to_string(), resumable, Some(300));
        drop(
            state
                .deps
                .protocol
                .sm_session_registry
                .ensure_session_claim("deferred-force-detach")
                .await
                .expect("live SM claim"),
        );
        let ping = xmpp_parsers::iq::Iq::from_get("deferred-ping", xmpp_parsers::ping::Ping);
        conn.deferred_inbound.push_back(
            waddle_xmpp::parser::stanza_to_string(ping)
                .expect("serialize deferred ping")
                .into(),
        );
        let requests: Vec<_> = origins
            .iter()
            .map(|&origin| waddle_xmpp::registry::ForceDetachRequest {
                origin,
                target_binding: None,
                requester_bare_jid: "alice@example.com".parse().expect("requester JID"),
                ack: tokio::sync::oneshot::channel().0,
            })
            .collect();

        settle_deferred_inbound_before_cleanup(
            "example.com",
            &state,
            &mut conn,
            &permit,
            &shutdown,
            &requests,
        )
        .await;

        assert!(conn.deferred_inbound.is_empty());
        assert_eq!(conn.sm_state.inbound_count(), expected_handled);
        let responses = conn.sm_state.stanzas_to_resend(0);
        assert_eq!(responses.len(), expected_handled as usize);
        if let Some(response) = responses.first() {
            let response = Element::from_str(&response.stanza_xml).expect("replay IQ XML");
            assert_eq!(response.name(), "iq");
            assert_eq!(response.attr("id"), Some("deferred-ping"));
            assert_eq!(response.attr("type"), Some("result"));
        }
    }

    #[tokio::test]
    async fn deferred_force_detach_fresh_bind_processes_parked_frames_without_resume() {
        assert_deferred_force_detach_handling(
            &[waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement],
            false,
            1,
        )
        .await;
    }

    #[tokio::test]
    async fn deferred_force_detach_resume_leaves_parked_frames_for_replay() {
        assert_deferred_force_detach_handling(
            &[waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume],
            true,
            0,
        )
        .await;
    }

    #[tokio::test]
    async fn deferred_force_detach_retirement_processes_parked_frames() {
        for origin in [
            waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement,
            waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
        ] {
            assert_deferred_force_detach_handling(&[origin], true, 1).await;
        }
    }

    #[tokio::test]
    async fn deferred_force_detach_fresh_bind_precedes_resume_for_parked_frames() {
        use waddle_xmpp::registry::ForceDetachOrigin::{CrossNodeResume, FreshBindReplacement};

        for origins in [
            [FreshBindReplacement, CrossNodeResume],
            [CrossNodeResume, FreshBindReplacement],
        ] {
            assert_deferred_force_detach_handling(&origins, true, 1).await;
        }
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
        assert_eq!(conn.sm_state.inbound_count(), 0);
        assert!(conn.sm_state.stanzas_to_resend(0).is_empty());
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
