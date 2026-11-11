//! Dark Axum-to-actor bridge. This fixture does not register a binding, run
//! authentication, own durable work, or exercise the production `/ws` route.
#![cfg(feature = "test-support")]

use std::collections::VecDeque;
use std::future;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequest, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::serve::Listener;
use axum::Router;
use futures::{SinkExt, StreamExt};
use tokio::io::DuplexStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::WebSocketStream;
use waddle_server::clustering::NodeAdmissionError;
use waddle_server::config::MaxWebSocketConnections;
use waddle_server::test_support::connection_actor::{
    canonical_actor_gate, ConnectionActorController, ConnectionActorError,
    ConnectionActorGateBarrier, ConnectionActorStartBarrier, ConnectionActorStartError,
    ConnectionActorStartHooks, CountableEffectDisposition, CountableStanzaEffect,
    OutputAdmissionPause, PanicConnectionActor, PauseConnectionActor, RequestAcknowledgement,
    SnapshotConnection,
};
use waddle_server::test_support::supervised_io::{
    AdmissionError, ReservedReaperExitPoint, ReservedStartError,
};
use waddle_server::test_support::{
    websocket_state_with_admission, WebSocketAdmissionTestConfig, WebSocketState,
};
use waddle_server::websocket_transport::{
    ActorWriteProbe, AdmissionTestBarriers, CustodianInventoryError, CustodianState, Limits,
    SocketAdmissionError, StopReason, TransportLimits,
};
use waddle_xmpp::ownership::{ConnectionRef, NodeIdentity, SharedNodeIdentity};
use waddle_xmpp::stream_management::SmAck;

const TIMEOUT: Duration = Duration::from_secs(5);
const RETIREMENT: Duration = Duration::from_millis(100);
const REQUEST: &str = "<r xmlns='urn:xmpp:sm:3'/>";

type Peer = WebSocketStream<DuplexStream>;

struct DuplexListener(mpsc::UnboundedReceiver<DuplexStream>);

impl Listener for DuplexListener {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.recv().await {
            Some(io) => (io, ()),
            None => future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
}

struct Gate {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

#[derive(Default)]
struct RouteOptions {
    before_reserve: Option<Gate>,
    before_response: Option<Gate>,
    hooks: ConnectionActorStartHooks,
    probe: Option<Arc<ActorWriteProbe>>,
    reaper_exit: Option<ReservedReaperExitPoint>,
    retirement: Option<Duration>,
    effect: Option<CountableStanzaEffect>,
}

enum Event {
    Reserved(ConnectionRef),
    Starting(AbortHandle, tokio_util::sync::CancellationToken),
    Finished(
        Result<
            Result<ConnectionActorController, ConnectionActorStartError>,
            tokio::task::JoinError,
        >,
    ),
}

struct RouteState {
    websocket: Arc<WebSocketState>,
    options: Mutex<VecDeque<RouteOptions>>,
    events: mpsc::UnboundedSender<Event>,
    limits: TransportLimits,
}

async fn handler(State(state): State<Arc<RouteState>>, request: Request) -> Response {
    let mut options = state
        .options
        .lock()
        .expect("options")
        .pop_front()
        .unwrap_or_default();
    let gate = match canonical_actor_gate(&state.websocket) {
        Ok(gate) => gate,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let upgrade = match WebSocketUpgrade::from_request(request, &state).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    if let Some(gate) = options.before_reserve.take() {
        let _ = gate.entered.send(());
        let _ = gate.release.await;
    }
    let admission = match gate.reserve(options.probe.take()) {
        Ok(admission) => admission,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let Some(exit) = options.reaper_exit.take() {
        admission.inject_reaper_exit(exit);
    }
    let _ = state
        .events
        .send(Event::Reserved(admission.connection_ref().clone()));
    if let Some(gate) = options.before_response.take() {
        let _ = gate.entered.send(());
        let _ = gate.release.await;
    }
    if admission.revalidate().is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    upgrade
        .protocols(["xmpp"])
        .on_upgrade(move |socket| async move {
            let effect = options.effect.unwrap_or_else(|| {
                Arc::new(|_| Box::pin(async { CountableEffectDisposition::Handled }))
            });
            let caller_gone = admission.caller_gone_for_test();
            let start = tokio::spawn(admission.start_synthetic_sm_fixture_with_effect(
                socket,
                state.limits,
                options.retirement.unwrap_or(RETIREMENT),
                TIMEOUT,
                options.hooks,
                move |input| effect(input),
            ));
            let _ = state
                .events
                .send(Event::Starting(start.abort_handle(), caller_gone));
            let _ = state.events.send(Event::Finished(start.await));
        })
}

struct Harness {
    state: Arc<WebSocketState>,
    route: Arc<RouteState>,
    events: mpsc::UnboundedReceiver<Event>,
    connections: mpsc::UnboundedSender<DuplexStream>,
    server: JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Harness {
    async fn new(identity: Option<SharedNodeIdentity>, inbound: Limits, outbound: Limits) -> Self {
        let state = websocket_state_with_admission(WebSocketAdmissionTestConfig {
            capacity: MaxWebSocketConnections::from_raw(Some("1")).expect("capacity"),
            clustered_identity: identity.map(Some),
            barriers: AdmissionTestBarriers::default(),
            live_connection_host: None,
            ws_keepalive: None,
        })
        .await;
        let (event_tx, events) = mpsc::unbounded_channel();
        let route = Arc::new(RouteState {
            websocket: Arc::clone(&state),
            options: Mutex::new(VecDeque::new()),
            events: event_tx,
            limits: TransportLimits::new(inbound, outbound),
        });
        let app = Router::new()
            .route("/bridge", get(handler))
            .with_state(Arc::clone(&route));
        let (connections, incoming) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let _ = axum::serve(DuplexListener(incoming), app).await;
        });
        Self {
            state,
            route,
            events,
            connections,
            server,
        }
    }

    fn queue(&self, options: RouteOptions) {
        self.route
            .options
            .lock()
            .expect("options")
            .push_back(options);
    }

    async fn open(&self) -> Result<Peer, Error> {
        let (server, peer) = tokio::io::duplex(64 * 1024);
        self.connections.send(server).expect("listener");
        tokio_tungstenite::client_async("ws://localhost/bridge", peer)
            .await
            .map(|(socket, _)| socket)
    }

    async fn event(&mut self) -> Event {
        timeout(TIMEOUT, self.events.recv())
            .await
            .expect("event timeout")
            .expect("event stream")
    }

    async fn reserved(&mut self) -> ConnectionRef {
        match self.event().await {
            Event::Reserved(reference) => reference,
            _ => panic!("expected reservation"),
        }
    }

    async fn starting(&mut self) -> (AbortHandle, tokio_util::sync::CancellationToken) {
        match self.event().await {
            Event::Starting(handle, caller_gone) => (handle, caller_gone),
            _ => panic!("expected startup handle"),
        }
    }

    async fn started(&mut self) -> ConnectionActorController {
        match self.event().await {
            Event::Finished(Ok(Ok(controller))) => controller,
            _ => panic!("expected started actor"),
        }
    }
}

fn limits(count: usize, bytes: usize) -> Limits {
    Limits::new(count, bytes).expect("limits")
}

fn assert_unavailable<T>(result: Result<T, Error>) {
    assert!(matches!(result, Err(Error::Http(response)) if response.status() == 503));
}

fn make_gate() -> (Gate, oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (entered, wait_entered) = oneshot::channel();
    let (release, wait_release) = oneshot::channel();
    (
        Gate {
            entered,
            release: wait_release,
        },
        wait_entered,
        release,
    )
}

fn make_start_barrier() -> (
    ConnectionActorStartBarrier,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
) {
    let (entered, wait_entered) = oneshot::channel();
    let (release, wait_release) = oneshot::channel();
    (
        ConnectionActorStartBarrier::new(entered, wait_release),
        wait_entered,
        release,
    )
}

async fn wait_gate(receiver: oneshot::Receiver<()>) {
    timeout(TIMEOUT, receiver)
        .await
        .expect("gate timeout")
        .expect("gate entered");
}

async fn wait_state(
    observer: &waddle_server::websocket_transport::IoCustodianObserver,
    expected: CustodianState,
) {
    timeout(TIMEOUT, async {
        while observer.state() != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("custodian state");
}

async fn send_request(peer: &mut Peer) {
    peer.send(Message::Text(REQUEST.into()))
        .await
        .expect("request");
}

async fn recv_ack(peer: &mut Peer) -> SmAck {
    loop {
        match timeout(TIMEOUT, peer.next())
            .await
            .expect("frame timeout")
            .expect("socket open")
            .expect("frame")
        {
            Message::Text(text) => {
                if let Some(ack) = SmAck::parse(text.as_str()) {
                    return ack;
                }
            }
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

struct ReleaseProbe(Arc<ActorWriteProbe>);
impl Drop for ReleaseProbe {
    fn drop(&mut self) {
        self.0.release();
        self.0.release_close();
    }
}

struct ReleaseAdmissionBarrier(Arc<ConnectionActorGateBarrier>);
impl Drop for ReleaseAdmissionBarrier {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_ack_stop_wins_before_admission_and_does_not_affect_successor() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    let _peer = h.open().await.expect("101");
    let original = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    let barrier = Arc::new(ConnectionActorGateBarrier::new());
    let _release = ReleaseAdmissionBarrier(Arc::clone(&barrier));
    let (entered_tx, entered_rx) = oneshot::channel();
    let (result_tx, result_rx) = oneshot::channel();
    controller.pause_next_output_admission(
        OutputAdmissionPause::BeforeGate,
        entered_tx,
        Arc::clone(&barrier),
        result_tx,
    );
    controller
        .actor()
        .tell(RequestAcknowledgement)
        .try_send()
        .expect("queue acknowledgment");
    wait_gate(entered_rx).await;
    controller.request_stop(StopReason::LeaseLost);
    barrier.release();
    assert_eq!(
        timeout(TIMEOUT, result_rx)
            .await
            .expect("admission decision")
            .expect("admission observation"),
        Err(ConnectionActorError::ConnectionStopping)
    );
    assert_eq!(
        timeout(TIMEOUT, controller.wait_until_joined())
            .await
            .expect("original joins"),
        Some(CustodianState::Joined)
    );

    let mut successor_peer = h.open().await.expect("successor 101");
    let successor = h.reserved().await;
    assert_ne!(successor, original);
    let _ = h.starting().await;
    let next = h.started().await;
    controller.request_stop(StopReason::Requested);
    send_request(&mut successor_peer).await;
    assert_eq!(recv_ack(&mut successor_peer).await.h, 0);
    assert_eq!(next.stop_reason(), None);
    next.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, next.wait_until_joined())
            .await
            .expect("successor joins"),
        Some(CustodianState::Joined)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_ack_admission_wins_before_stop_callback() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    let _peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    let controller = Arc::new(h.started().await);
    let barrier = Arc::new(ConnectionActorGateBarrier::new());
    let _release = ReleaseAdmissionBarrier(Arc::clone(&barrier));
    let (entered_tx, entered_rx) = oneshot::channel();
    let (result_tx, mut result_rx) = oneshot::channel();
    controller.pause_next_output_admission(
        OutputAdmissionPause::InsideGate,
        entered_tx,
        Arc::clone(&barrier),
        result_tx,
    );
    controller
        .actor()
        .tell(RequestAcknowledgement)
        .try_send()
        .expect("queue acknowledgment");
    wait_gate(entered_rx).await;
    let stopping = Arc::clone(&controller);
    let stop = tokio::task::spawn_blocking(move || stopping.request_stop(StopReason::LeaseLost));
    assert!(
        timeout(TIMEOUT, controller.wait_for_stop_signal_callbacks(1))
            .await
            .expect("stop reaches the lifecycle callback")
    );
    assert!(matches!(
        result_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    barrier.release();
    assert_eq!(
        timeout(TIMEOUT, result_rx)
            .await
            .expect("admission decision")
            .expect("admission observation"),
        Ok(())
    );
    timeout(TIMEOUT, stop)
        .await
        .expect("stop returns after gate release")
        .expect("stop task");
    assert_eq!(
        timeout(TIMEOUT, controller.wait_until_joined())
            .await
            .expect("original joins"),
        Some(CustodianState::Joined)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closed_websocket_output_receiver_stops_after_releasing_lifecycle_gate() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    let _peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    assert!(
        timeout(TIMEOUT, controller.close_output_receiver_for_test())
            .await
            .expect("output receiver closes")
    );
    let result = timeout(
        TIMEOUT,
        controller.actor().ask(RequestAcknowledgement).send(),
    )
    .await
    .expect("closed-queue stop callback does not deadlock")
    .expect("actor reply")
    .0;
    assert_eq!(
        result,
        Err(ConnectionActorError::Admission(AdmissionError::Closed))
    );
    assert_eq!(
        controller.stop_reason(),
        Some(StopReason::OutboundQueueClosed)
    );
    assert_eq!(
        timeout(TIMEOUT, controller.wait_until_joined())
            .await
            .expect("closed receiver joins"),
        Some(CustodianState::Joined)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_websocket_output_queue_stops_after_releasing_lifecycle_gate() {
    let probe = Arc::new(ActorWriteProbe::default());
    let _release = ReleaseProbe(Arc::clone(&probe));
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    h.queue(RouteOptions {
        probe: Some(Arc::clone(&probe)),
        ..Default::default()
    });
    let _peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    probe.pause_next();
    assert_eq!(
        controller
            .actor()
            .ask(RequestAcknowledgement)
            .send()
            .await
            .expect("first")
            .0,
        Ok(())
    );
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("writer stalls");
    for _ in 0..4 {
        assert_eq!(
            controller
                .actor()
                .ask(RequestAcknowledgement)
                .send()
                .await
                .expect("queued")
                .0,
            Ok(())
        );
    }
    let result = timeout(
        TIMEOUT,
        controller.actor().ask(RequestAcknowledgement).send(),
    )
    .await
    .expect("overload stop callback does not deadlock")
    .expect("actor reply")
    .0;
    assert_eq!(
        result,
        Err(ConnectionActorError::Admission(AdmissionError::Overloaded))
    );
    assert_eq!(controller.stop_reason(), Some(StopReason::OutboundOverload));
    probe.release();
    assert_eq!(
        timeout(TIMEOUT, controller.wait_until_joined())
            .await
            .expect("overloaded connection joins"),
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn exact_app_state_identity_capacity_join_and_automatic_reclamation() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    let mut peer = h.open().await.expect("first 101");
    let exact = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    assert_eq!(controller.connection_ref(), &exact);
    let snapshot = controller
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("snapshot");
    assert_eq!(snapshot.connection_ref, exact);
    let observer = controller.observer().expect("exact observer");
    assert!(observer.is_for(&exact));
    assert!(observer.local_io_relinquishment().is_for(&exact));
    send_request(&mut peer).await;
    assert_eq!(recv_ack(&mut peer).await.h, 0);
    assert_unavailable(h.open().await);
    controller.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, observer.wait_until_joined())
            .await
            .expect("join"),
        CustodianState::Joined
    );
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1
    );
    let peer2 = h
        .open()
        .await
        .expect("joined capacity reclaimed on admission");
    let successor = h.reserved().await;
    assert_ne!(successor, exact);
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .observe(&exact)
        .is_none());
    assert_eq!(observer.state(), CustodianState::Joined);
    let _ = h.starting().await;
    let next = h.started().await;
    next.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, next.wait_until_joined())
            .await
            .expect("join"),
        Some(CustodianState::Joined)
    );
    let peer3 = h.open().await.expect("capacity recycles again");
    let third = h.reserved().await;
    assert_ne!(third, successor);
    let _ = h.starting().await;
    let last = h.started().await;
    last.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, last.wait_until_joined())
            .await
            .expect("third join"),
        Some(CustodianState::Joined)
    );
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        1
    );
    drop((peer, peer2, peer3));
}

#[tokio::test]
async fn source_aba_before_101_rejects_and_releases_the_reservation() {
    let identity_value = NodeIdentity::new("node", "epoch-a");
    let identity = SharedNodeIdentity::new(identity_value.clone());
    let mut h = Harness::new(Some(identity.clone()), limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_gate();
    h.queue(RouteOptions {
        before_response: Some(barrier),
        ..Default::default()
    });
    let (server, peer) = tokio::io::duplex(64 * 1024);
    h.connections.send(server).expect("listener");
    let request = tokio::spawn(async move {
        tokio_tungstenite::client_async("ws://localhost/bridge", peer).await
    });
    let _ = h.reserved().await;
    wait_gate(entered).await;
    identity.rotate(NodeIdentity::new("node", "epoch-b")).await;
    identity.rotate(identity_value).await;
    release.send(()).expect("release");
    let result = timeout(TIMEOUT, request)
        .await
        .expect("response")
        .expect("request task")
        .map(|(peer, _)| peer);
    assert_unavailable(result);
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn lifecycle_recovery_before_101_rejects_the_original_generation() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_gate();
    h.queue(RouteOptions {
        before_response: Some(barrier),
        ..Default::default()
    });
    let (server, peer) = tokio::io::duplex(64 * 1024);
    h.connections.send(server).expect("listener");
    let request = tokio::spawn(async move {
        tokio_tungstenite::client_async("ws://localhost/bridge", peer).await
    });
    let _ = h.reserved().await;
    wait_gate(entered).await;
    h.state
        .deps
        .app_state
        .node_lifecycle
        .begin_fenced_recovery();
    h.state.deps.app_state.node_lifecycle.serve();
    release.send(()).expect("release");
    let result = timeout(TIMEOUT, request)
        .await
        .expect("response")
        .expect("request task")
        .map(|(peer, _)| peer);
    assert_unavailable(result);
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn revoked_after_101_before_split_sends_bounded_close() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "epoch-a"));
    let mut h = Harness::new(Some(identity.clone()), limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_start_barrier();
    h.queue(RouteOptions {
        hooks: ConnectionActorStartHooks {
            before_factory: Some(barrier),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    wait_gate(entered).await;
    identity.disable().await;
    release.send(()).expect("release");
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("close"),
        Some(Ok(Message::Close(_)))
    ));
    assert!(matches!(
        h.event().await,
        Event::Finished(Ok(Err(ConnectionActorStartError::Socket(
            SocketAdmissionError::IdentityRevoked
        ))))
    ));
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn lifecycle_recovery_after_101_before_split_sends_close() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_start_barrier();
    h.queue(RouteOptions {
        hooks: ConnectionActorStartHooks {
            before_factory: Some(barrier),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    wait_gate(entered).await;
    h.state
        .deps
        .app_state
        .node_lifecycle
        .begin_fenced_recovery();
    h.state.deps.app_state.node_lifecycle.serve();
    release.send(()).expect("release");
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("close"),
        Some(Ok(Message::Close(_)))
    ));
    assert!(matches!(h.event().await, Event::Finished(Ok(Err(_)))));
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn revoked_after_101_after_split_sends_bounded_close() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "epoch-a"));
    let mut h = Harness::new(Some(identity.clone()), limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_start_barrier();
    h.queue(RouteOptions {
        hooks: ConnectionActorStartHooks {
            after_socket_split: Some(barrier),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    wait_gate(entered).await;
    identity.disable().await;
    release.send(()).expect("release");
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("close"),
        Some(Ok(Message::Close(_)))
    ));
    assert!(matches!(
        h.event().await,
        Event::Finished(Ok(Err(ConnectionActorStartError::Socket(
            SocketAdmissionError::IdentityRevoked
        ))))
    ));
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn lifecycle_recovery_after_101_after_split_sends_close() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_start_barrier();
    h.queue(RouteOptions {
        hooks: ConnectionActorStartHooks {
            after_socket_split: Some(barrier),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    wait_gate(entered).await;
    h.state
        .deps
        .app_state
        .node_lifecycle
        .begin_fenced_recovery();
    h.state.deps.app_state.node_lifecycle.serve();
    release.send(()).expect("release");
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("close"),
        Some(Ok(Message::Close(_)))
    ));
    assert!(matches!(
        h.event().await,
        Event::Finished(Ok(Err(ConnectionActorStartError::Socket(
            SocketAdmissionError::Lifecycle(NodeAdmissionError::Revoked)
        ))))
    ));
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn cancelled_post_split_close_retains_capacity_until_timeout() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "epoch-a"));
    let probe = Arc::new(ActorWriteProbe::default());
    let _release_on_drop = ReleaseProbe(Arc::clone(&probe));
    probe.pause_close();
    let mut h = Harness::new(Some(identity), limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_start_barrier();
    h.queue(RouteOptions {
        hooks: ConnectionActorStartHooks {
            after_socket_split: Some(barrier),
            ..Default::default()
        },
        probe: Some(Arc::clone(&probe)),
        retirement: Some(Duration::from_millis(150)),
        ..Default::default()
    });
    let _peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let (_start, caller_gone) = h.starting().await;
    wait_gate(entered).await;
    caller_gone.cancel();
    release.send(()).expect("release");
    timeout(TIMEOUT, probe.wait_for_close())
        .await
        .expect("split writer Close attempt entered");
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1,
        "the split writer keeps its capacity reservation while Close is pending"
    );
    assert_unavailable(h.open().await);
    assert!(matches!(
        h.event().await,
        Event::Finished(Ok(Err(ConnectionActorStartError::Socket(
            SocketAdmissionError::Cancelled
        ))))
    ));
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn dropped_post_split_start_keeps_capacity_until_bounded_close_ends() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "epoch-a"));
    let probe = Arc::new(ActorWriteProbe::default());
    let _release_on_drop = ReleaseProbe(Arc::clone(&probe));
    probe.pause_close();
    let mut h = Harness::new(Some(identity), limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_start_barrier();
    h.queue(RouteOptions {
        hooks: ConnectionActorStartHooks {
            after_socket_split: Some(barrier),
            panic_after_socket_split: true,
            ..Default::default()
        },
        probe: Some(Arc::clone(&probe)),
        retirement: Some(Duration::from_millis(150)),
        ..Default::default()
    });
    let _peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    wait_gate(entered).await;
    release.send(()).expect("release");
    timeout(TIMEOUT, probe.wait_for_close())
        .await
        .expect("Drop cleanup entered bounded Close");
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1,
        "Drop cleanup keeps its reservation through the Close deadline"
    );
    assert!(matches!(
        h.event().await,
        Event::Finished(Ok(Err(ConnectionActorStartError::StartupPanicked)))
    ));
    timeout(TIMEOUT, async {
        while !h
            .state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded Close ended and released its reservation");
}

#[tokio::test]
async fn cancellation_after_pump_handoff_or_actor_start_keeps_cleanup_indexed() {
    for stage in 0..4 {
        let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
        let (barrier, entered, release) = make_start_barrier();
        let hooks = match stage {
            0 => ConnectionActorStartHooks {
                before_inbound_spawn: Some(barrier),
                ..Default::default()
            },
            1 => ConnectionActorStartHooks {
                between_pump_spawns: Some(barrier),
                ..Default::default()
            },
            2 => ConnectionActorStartHooks {
                after_pumps_handed_off: Some(barrier),
                ..Default::default()
            },
            _ => ConnectionActorStartHooks {
                after_actor_started: Some(barrier),
                ..Default::default()
            },
        };
        h.queue(RouteOptions {
            hooks,
            ..Default::default()
        });
        let peer = h.open().await.expect("101");
        let exact = h.reserved().await;
        let (start, caller_gone) = h.starting().await;
        wait_gate(entered).await;
        let observer = h
            .state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .observe(&exact)
            .expect("indexed");
        start.abort();
        timeout(TIMEOUT, caller_gone.cancelled())
            .await
            .expect("independent owner observed cancellation");
        assert!(matches!(h.event().await, Event::Finished(Err(error)) if error.is_cancelled()));
        assert_eq!(
            h.state
                .deps
                .app_state
                .websocket_inventory_for_test()
                .prune_joined(),
            0
        );
        release.send(()).expect("release");
        let terminal = timeout(TIMEOUT, observer.wait_until_joined())
            .await
            .unwrap_or_else(|_| panic!("join at startup stage {stage}"));
        assert!(matches!(
            terminal,
            CustodianState::Joined | CustodianState::PartialStartJoined
        ));
        assert_eq!(
            h.state
                .deps
                .app_state
                .websocket_inventory_for_test()
                .prune_joined(),
            1
        );
        drop(peer);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_aba_fences_a_queued_write_and_retains_noncooperative_cleanup() {
    let initial = NodeIdentity::new("node", "epoch-a");
    let identity = SharedNodeIdentity::new(initial.clone());
    let probe = Arc::new(ActorWriteProbe::default());
    let _release_on_drop = ReleaseProbe(Arc::clone(&probe));
    let mut h = Harness::new(Some(identity.clone()), limits(4, 1024), limits(4, 1024)).await;
    h.queue(RouteOptions {
        probe: Some(Arc::clone(&probe)),
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let exact = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    let observer = h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .observe(&exact)
        .expect("indexed");
    probe.pause_next();
    send_request(&mut peer).await;
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("writer enters probe");
    let snapshot = timeout(TIMEOUT, controller.actor().ask(SnapshotConnection).send())
        .await
        .expect("actor progress")
        .expect("snapshot");
    assert_eq!(snapshot.connection_ref, exact);
    identity.rotate(NodeIdentity::new("node", "epoch-b")).await;
    identity.rotate(initial).await;
    wait_state(&observer, CustodianState::CleanupPending).await;
    assert_eq!(probe.attempts(), 0);
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        0
    );
    probe.release();
    assert_eq!(
        timeout(TIMEOUT, observer.wait_until_joined())
            .await
            .expect("join"),
        CustodianState::Joined
    );
    assert_eq!(
        probe.attempts(),
        0,
        "old revision never reaches start_send after ABA"
    );
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actor_panic_preserves_cleanup_pending_until_fenced_writer_returns() {
    let probe = Arc::new(ActorWriteProbe::default());
    let _release_on_drop = ReleaseProbe(Arc::clone(&probe));
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    h.queue(RouteOptions {
        probe: Some(Arc::clone(&probe)),
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let exact = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    let observer = h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .observe(&exact)
        .expect("indexed");
    probe.pause_next();
    send_request(&mut peer).await;
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("writer entered");
    controller
        .actor()
        .tell(PanicConnectionActor)
        .send()
        .await
        .expect("panic queued");
    wait_state(&observer, CustodianState::CleanupPending).await;
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        0
    );
    probe.release();
    assert_eq!(
        timeout(TIMEOUT, observer.wait_until_joined())
            .await
            .expect("join"),
        CustodianState::Joined
    );
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        1
    );
}

#[tokio::test]
async fn full_actor_mailbox_and_inbound_budget_do_not_block_independent_stop() {
    let mut h = Harness::new(None, limits(1, 1024), limits(4, 1024)).await;
    let mut peer = h.open().await.expect("101");
    let exact = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    let (entered, entered_rx) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    controller
        .actor()
        .tell(PauseConnectionActor {
            entered,
            release: release_rx,
        })
        .send()
        .await
        .expect("pause");
    wait_gate(entered_rx).await;
    send_request(&mut peer).await;
    assert!(timeout(TIMEOUT, controller.wait_for_inbound_forwarded(1))
        .await
        .expect("first queued"));
    send_request(&mut peer).await;
    assert!(
        timeout(
            Duration::from_millis(50),
            controller.wait_for_inbound_forwarded(2)
        )
        .await
        .is_err(),
        "first item retains the count permit"
    );
    for _ in 0..255 {
        controller
            .actor()
            .tell(SnapshotConnection)
            .try_send()
            .expect("fill mailbox");
    }
    assert!(controller
        .actor()
        .tell(SnapshotConnection)
        .try_send()
        .is_err());
    controller.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, controller.wait_until_joined())
            .await
            .expect("join"),
        Some(CustodianState::Joined)
    );
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .observe(&exact)
        .is_some());
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        1
    );
    let _ = release.send(());
}

#[tokio::test]
async fn countable_effects_keep_each_message_stanza_language() {
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let first_entered = Arc::new(tokio::sync::Semaphore::new(0));
    let first_release = Arc::new(tokio::sync::Semaphore::new(0));
    let effect_entered = Arc::clone(&first_entered);
    let effect_release = Arc::clone(&first_release);
    let effect: CountableStanzaEffect = Arc::new(move |input| {
        let observed = observed_tx.clone();
        let entered = Arc::clone(&effect_entered);
        let release = Arc::clone(&effect_release);
        Box::pin(async move {
            if input.sequence.sequence_number() == 1 {
                entered.add_permits(1);
                release.acquire().await.expect("release effect").forget();
            }
            observed.send(input).expect("effect observation");
            CountableEffectDisposition::Handled
        })
    });
    let mut h = Harness::new(None, limits(16, 4096), limits(4, 1024)).await;
    h.queue(RouteOptions {
        effect: Some(effect),
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    let cases = [
        (
            "<message xmlns='jabber:client' id='fr' xml:lang='fr'><body xml:lang='en'>hello</body></message>",
            "message",
            "fr",
            Some("fr"),
        ),
        (
            "<message xmlns='jabber:client' id='child'><body xml:lang='de'>hallo</body></message>",
            "message",
            "child",
            None,
        ),
        (
            "<message xmlns='jabber:client' id='ja' xml:lang='ja'><body>hello</body></message>",
            "message",
            "ja",
            Some("ja"),
        ),
        (
            "<message xmlns='jabber:client' id='empty' xml:lang=''><body>hello</body></message>",
            "message",
            "empty",
            Some(""),
        ),
        (
            "<message xmlns='jabber:client' id='plain'><body>hello</body></message>",
            "message",
            "plain",
            None,
        ),
        (
            "<presence xmlns='jabber:client' id='presence' xml:lang='fr'/>",
            "presence",
            "presence",
            None,
        ),
        (
            "<iq xmlns='jabber:client' id='iq' type='get'><query xmlns='jabber:iq:version'/></iq>",
            "iq",
            "iq",
            None,
        ),
    ];
    peer.send(Message::Text(cases[0].0.into()))
        .await
        .expect("first stanza");
    timeout(TIMEOUT, first_entered.acquire())
        .await
        .expect("first effect entered")
        .expect("semaphore")
        .forget();
    for (xml, _, _, _) in &cases[1..] {
        peer.send(Message::Text((*xml).into()))
            .await
            .expect("queued stanza");
    }
    send_request(&mut peer).await;
    assert_eq!(recv_ack(&mut peer).await.h, 0);
    first_release.add_permits(1);

    for (index, (_, kind, id, expected_lang)) in cases.iter().enumerate() {
        let observed = timeout(TIMEOUT, observed_rx.recv())
            .await
            .expect("effect observation timeout")
            .expect("effect observation channel");
        assert_eq!(observed.stanza.name(), *kind);
        assert_eq!(observed.stanza.to_element().attr("id"), Some(*id));
        assert_eq!(
            observed
                .message_stanza_lang
                .as_ref()
                .map(|lang| lang.0.as_str()),
            *expected_lang
        );
        assert_eq!(observed.sequence.sequence_number(), index as u32 + 1);
    }
    assert!(
        timeout(TIMEOUT, controller.wait_for_effects_completed(cases.len()))
            .await
            .expect("effects complete")
    );
    send_request(&mut peer).await;
    assert_eq!(recv_ack(&mut peer).await.h, 7);
    controller.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, controller.wait_until_joined())
            .await
            .expect("join"),
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn inbound_bytes_remain_reserved_through_the_synthetic_effect() {
    const STANZA: &str =
        "<message xmlns='jabber:client' type='chat'><body>held-effect</body></message>";
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let effect_entered = Arc::clone(&entered);
    let effect_release = Arc::clone(&release);
    let effect: CountableStanzaEffect = Arc::new(move |_| {
        let entered = Arc::clone(&effect_entered);
        let release = Arc::clone(&effect_release);
        Box::pin(async move {
            entered.add_permits(1);
            release.acquire().await.expect("release effect").forget();
            CountableEffectDisposition::Handled
        })
    });
    let mut h = Harness::new(None, limits(8, STANZA.len()), limits(4, 1024)).await;
    h.queue(RouteOptions {
        effect: Some(effect),
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    peer.send(Message::Text(STANZA.into()))
        .await
        .expect("stanza");
    timeout(TIMEOUT, entered.acquire())
        .await
        .expect("effect entered")
        .expect("semaphore")
        .forget();
    send_request(&mut peer).await;
    assert!(
        timeout(
            Duration::from_millis(50),
            controller.wait_for_inbound_forwarded(2)
        )
        .await
        .is_err(),
        "the held effect retains the first frame's byte budget"
    );
    release.add_permits(1);
    assert!(timeout(TIMEOUT, controller.wait_for_inbound_forwarded(2))
        .await
        .expect("ack forwarded"));
    assert_eq!(recv_ack(&mut peer).await.h, 1);
    controller.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, controller.wait_until_joined())
            .await
            .expect("join"),
        Some(CustodianState::Joined)
    );
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        1
    );
}

#[tokio::test]
async fn source_aba_between_gate_and_reservation_returns_503_without_a_slot() {
    let initial = NodeIdentity::new("node", "epoch-a");
    let identity = SharedNodeIdentity::new(initial.clone());
    let h = Harness::new(Some(identity.clone()), limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_gate();
    h.queue(RouteOptions {
        before_reserve: Some(barrier),
        ..Default::default()
    });
    let (server, peer) = tokio::io::duplex(64 * 1024);
    h.connections.send(server).expect("listener");
    let request = tokio::spawn(async move {
        tokio_tungstenite::client_async("ws://localhost/bridge", peer).await
    });
    wait_gate(entered).await;
    identity.rotate(NodeIdentity::new("node", "epoch-b")).await;
    identity.rotate(initial).await;
    release.send(()).expect("release");
    let result = timeout(TIMEOUT, request)
        .await
        .expect("response")
        .expect("request task")
        .map(|(peer, _)| peer);
    assert_unavailable(result);
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn cancelled_start_before_split_retains_slot_until_bounded_close_attempt() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_start_barrier();
    h.queue(RouteOptions {
        hooks: ConnectionActorStartHooks {
            before_factory: Some(barrier),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let (start, caller_gone) = h.starting().await;
    wait_gate(entered).await;
    start.abort();
    timeout(TIMEOUT, caller_gone.cancelled())
        .await
        .expect("independent owner observed cancellation");
    assert!(matches!(h.event().await, Event::Finished(Err(error)) if error.is_cancelled()));
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1
    );
    release.send(()).expect("release");
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("close"),
        Some(Ok(Message::Close(_)))
    ));
    timeout(TIMEOUT, async {
        while !h
            .state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("unused slot released");
}

#[tokio::test]
async fn lost_reaper_registers_cleanup_even_without_an_actor() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    h.queue(RouteOptions {
        reaper_exit: Some(ReservedReaperExitPoint::BeforeReady),
        ..Default::default()
    });
    let peer = h.open().await.expect("101");
    let exact = h.reserved().await;
    let _ = h.starting().await;
    assert!(matches!(
        h.event().await,
        Event::Finished(Ok(Err(ConnectionActorStartError::Socket(
            SocketAdmissionError::Start(ReservedStartError::ReaperUnavailable)
        ))))
    ));
    let observer = h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .observe(&exact)
        .expect("indexed failed start");
    wait_state(&observer, CustodianState::CleanupPending).await;
    assert!(
        observer.auxiliary_registered(),
        "no-actor error registers its cleanup owner"
    );
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        0
    );
    drop(peer);
}

#[tokio::test]
async fn rejected_unsplit_socket_retains_capacity_during_close() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "epoch-a"));
    let probe = Arc::new(ActorWriteProbe::default());
    let _release_on_drop = ReleaseProbe(Arc::clone(&probe));
    probe.pause_close();
    let mut h = Harness::new(Some(identity.clone()), limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_start_barrier();
    h.queue(RouteOptions {
        probe: Some(Arc::clone(&probe)),
        retirement: Some(Duration::from_secs(2)),
        hooks: ConnectionActorStartHooks {
            before_factory: Some(barrier),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let _ = h.reserved().await;
    let _ = h.starting().await;
    wait_gate(entered).await;
    identity.rotate(NodeIdentity::new("node", "epoch-b")).await;
    release.send(()).expect("release startup");
    timeout(TIMEOUT, probe.wait_for_close())
        .await
        .expect("close attempt entered");
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1,
        "the unsplit socket still owns its reservation while Close is pending"
    );
    // The current source remains active, so capacity rejects the next gate.
    assert_unavailable(h.open().await);
    probe.release_close();
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("close"),
        Some(Ok(Message::Close(_)))
    ));
    assert!(matches!(
        h.event().await,
        Event::Finished(Ok(Err(ConnectionActorStartError::Socket(
            SocketAdmissionError::IdentityRevoked
        ))))
    ));
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn cleanup_registration_failure_keeps_the_hold_inconclusive() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    let (barrier, entered, release) = make_start_barrier();
    h.queue(RouteOptions {
        reaper_exit: Some(ReservedReaperExitPoint::BeforeReady),
        hooks: ConnectionActorStartHooks {
            after_pumps_handed_off: Some(barrier),
            ..Default::default()
        },
        ..Default::default()
    });
    let peer = h.open().await.expect("101");
    let exact = h.reserved().await;
    let _ = h.starting().await;
    wait_gate(entered).await;
    let inventory = h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .clone();
    let observer = inventory.observe(&exact).expect("active exact slot");
    let competing = tokio::spawn(std::future::pending::<()>());
    let abort_competing = competing.abort_handle();
    inventory
        .register_auxiliary_task(&exact, competing)
        .expect("inject competing registration");
    release.send(()).expect("release");
    assert!(matches!(
        h.event().await,
        Event::Finished(Ok(Err(ConnectionActorStartError::Inventory(
            CustodianInventoryError::AuxiliaryTaskAlreadyRegistered
        ))))
    ));
    wait_state(&observer, CustodianState::CleanupPending).await;
    assert_eq!(
        inventory.prune_joined(),
        0,
        "failed registration never publishes cleanup completion"
    );
    abort_competing.abort();
    drop(peer);
}

#[tokio::test]
async fn panic_after_pump_handoff_registers_and_joins_no_actor_cleanup() {
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    h.queue(RouteOptions {
        hooks: ConnectionActorStartHooks {
            panic_after_pumps_handed_off: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let peer = h.open().await.expect("101");
    let exact = h.reserved().await;
    let _ = h.starting().await;
    assert!(matches!(
        h.event().await,
        Event::Finished(Ok(Err(ConnectionActorStartError::StartupPanicked)))
    ));
    let observer = h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .observe(&exact)
        .expect("indexed");
    assert!(observer.auxiliary_registered());
    let terminal = timeout(TIMEOUT, observer.wait_until_joined())
        .await
        .expect("join");
    assert!(matches!(
        terminal,
        CustodianState::Joined | CustodianState::PartialStartJoined
    ));
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        1
    );
    drop(peer);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_stop_survives_full_mailbox_and_a_stalled_physical_writer() {
    let probe = Arc::new(ActorWriteProbe::default());
    let _release_on_drop = ReleaseProbe(Arc::clone(&probe));
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    h.queue(RouteOptions {
        probe: Some(Arc::clone(&probe)),
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let exact = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    let observer = h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .observe(&exact)
        .expect("indexed");
    probe.pause_next();
    send_request(&mut peer).await;
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("writer entered");
    let (entered, entered_rx) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    controller
        .actor()
        .tell(PauseConnectionActor {
            entered,
            release: release_rx,
        })
        .send()
        .await
        .expect("pause");
    wait_gate(entered_rx).await;
    for _ in 0..256 {
        controller
            .actor()
            .tell(SnapshotConnection)
            .try_send()
            .expect("fill mailbox");
    }
    assert!(controller
        .actor()
        .tell(SnapshotConnection)
        .try_send()
        .is_err());
    controller.request_stop(StopReason::Requested);
    wait_state(&observer, CustodianState::CleanupPending).await;
    assert_unavailable(h.open().await);
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        0
    );
    probe.release();
    assert_eq!(
        timeout(TIMEOUT, observer.wait_until_joined())
            .await
            .expect("join"),
        CustodianState::Joined
    );
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        1
    );
    let _ = release.send(());
}

struct BlockingEffectGate {
    released: Mutex<bool>,
    wake: std::sync::Condvar,
    entered: tokio::sync::Semaphore,
}

impl BlockingEffectGate {
    fn new() -> Self {
        Self {
            released: Mutex::new(false),
            wake: std::sync::Condvar::new(),
            entered: tokio::sync::Semaphore::new(0),
        }
    }

    fn block(&self) {
        self.entered.add_permits(1);
        let mut released = self.released.lock().expect("gate lock");
        while !*released {
            released = self.wake.wait(released).expect("gate wait");
        }
    }

    fn release(&self) {
        *self.released.lock().expect("gate lock") = true;
        self.wake.notify_all();
    }
}

struct ReleaseEffect(Arc<BlockingEffectGate>);
impl Drop for ReleaseEffect {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn noncooperative_effect_keeps_auxiliary_cleanup_pending() {
    const STANZA: &str =
        "<message xmlns='jabber:client' type='chat'><body>blocked</body></message>";
    let gate = Arc::new(BlockingEffectGate::new());
    let _release_on_drop = ReleaseEffect(Arc::clone(&gate));
    let effect_gate = Arc::clone(&gate);
    let effect: CountableStanzaEffect = Arc::new(move |_| {
        let gate = Arc::clone(&effect_gate);
        Box::pin(async move {
            gate.block();
            CountableEffectDisposition::Handled
        })
    });
    let mut h = Harness::new(None, limits(4, 1024), limits(4, 1024)).await;
    h.queue(RouteOptions {
        effect: Some(effect),
        ..Default::default()
    });
    let mut peer = h.open().await.expect("101");
    let exact = h.reserved().await;
    let _ = h.starting().await;
    let controller = h.started().await;
    let observer = h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .observe(&exact)
        .expect("indexed");
    peer.send(Message::Text(STANZA.into()))
        .await
        .expect("stanza");
    timeout(TIMEOUT, gate.entered.acquire())
        .await
        .expect("effect entered")
        .expect("semaphore")
        .forget();
    controller.request_stop(StopReason::Requested);
    wait_state(&observer, CustodianState::CleanupPending).await;
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        0
    );
    assert_unavailable(h.open().await);
    gate.release();
    assert_eq!(
        timeout(TIMEOUT, observer.wait_until_joined())
            .await
            .expect("join"),
        CustodianState::Joined
    );
    let peer2 = h.open().await.expect("joined cleanup permits reconnection");
    let successor = h.reserved().await;
    assert_ne!(successor, exact);
    let _ = h.starting().await;
    let next = h.started().await;
    next.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, next.wait_until_joined())
            .await
            .expect("successor joins"),
        Some(CustodianState::Joined)
    );
    assert_eq!(
        h.state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .prune_joined(),
        1
    );
    drop(peer2);
}
