//! Production upgrade admission exercised with deterministic local barriers.
#![cfg(feature = "test-support")]

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error, Message};
use waddle_server::config::MaxWebSocketConnections;
use waddle_server::server::LiveConnectionHost;
use waddle_server::test_support::{
    router, websocket_state_with_admission, WebSocketAdmissionTestConfig, WebSocketState,
};
use waddle_server::websocket_transport::{
    AdmissionTestBarrier, AdmissionTestBarriers, CallbackWaiterTestControl, LiveActorTestTap,
};
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};
use waddle_xmpp::protocol::KeepaliveConfig;
use waddle_xmpp::registry::{ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest};

const DEADLINE: Duration = Duration::from_secs(10);

struct Harness {
    state: Arc<WebSocketState>,
    url: String,
    addr: std::net::SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start(
    capacity: usize,
    identity: Option<Option<SharedNodeIdentity>>,
    barriers: AdmissionTestBarriers,
) -> Harness {
    start_hosted(capacity, identity, barriers, None, None).await
}

/// `host` pins the live connection host; `None` keeps the one the test
/// process selected.
async fn start_hosted(
    capacity: usize,
    identity: Option<Option<SharedNodeIdentity>>,
    barriers: AdmissionTestBarriers,
    host: Option<LiveConnectionHost>,
    keepalive: Option<KeepaliveConfig>,
) -> Harness {
    let state = websocket_state_with_admission(WebSocketAdmissionTestConfig {
        capacity: MaxWebSocketConnections::from_raw(Some(&capacity.to_string())).expect("capacity"),
        clustered_identity: identity,
        barriers,
        live_connection_host: host,
        ws_keepalive: keepalive,
    })
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().expect("address");
    let app = router(Arc::clone(&state));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Harness {
        state,
        url: format!("ws://{addr}/ws"),
        addr,
        task,
    }
}

async fn connect(
    url: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Error,
> {
    let mut request = url.into_client_request().expect("request");
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "xmpp".parse().expect("protocol"));
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, _)| socket)
}

fn assert_unavailable<T>(result: Result<T, Error>) {
    assert!(matches!(result, Err(Error::Http(response)) if response.status() == 503));
}

async fn wait_for_slots(state: &WebSocketState, count: usize) {
    tokio::time::timeout(DEADLINE, async {
        while state.deps.app_state.websocket_inventory_for_test().len() != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("inventory count");
}

async fn wait_entered(barrier: &AdmissionTestBarrier) {
    tokio::time::timeout(DEADLINE, barrier.wait_until_entered())
        .await
        .expect("barrier entered");
}

async fn wait_for_drain(state: &WebSocketState) {
    tokio::time::timeout(DEADLINE, async {
        while !state
            .deps
            .app_state
            .websocket_inventory_for_test()
            .is_empty()
            || state.deps.shutdown.active_connections() != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("connection and admission drained");
}

async fn wait_for_waiter_drop(control: &CallbackWaiterTestControl) {
    tokio::time::timeout(DEADLINE, control.waiter_dropped.cancelled())
        .await
        .expect("callback waiter dropped");
}

fn assert_connection_owned(state: &WebSocketState) {
    assert_eq!(state.deps.app_state.websocket_inventory_for_test().len(), 1);
    assert_eq!(state.deps.shutdown.active_connections(), 1);
}

#[test]
fn capacity_config_rejects_zero_and_invalid_input() {
    assert_eq!(
        MaxWebSocketConnections::from_raw(None)
            .expect("default")
            .get(),
        4096
    );
    assert_eq!(
        MaxWebSocketConnections::from_raw(Some("1"))
            .expect("one")
            .get(),
        1
    );
    for input in [
        "",
        "0",
        "-1",
        "1.5",
        "1 ",
        "abc",
        "9999999999999999999999999999999999",
    ] {
        assert!(
            MaxWebSocketConnections::from_raw(Some(input)).is_err(),
            "{input:?}"
        );
    }
}

#[tokio::test]
async fn malformed_upgrade_does_not_reserve_and_capacity_is_released_on_close() {
    let h = start(1, None, AdmissionTestBarriers::default()).await;
    let mut malformed = tokio::net::TcpStream::connect(h.addr)
        .await
        .expect("HTTP connection");
    malformed
        .write_all(b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("malformed upgrade");
    let mut response = Vec::new();
    malformed
        .read_to_end(&mut response)
        .await
        .expect("response");
    assert!(!response.starts_with(b"HTTP/1.1 101"));
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());

    let first = connect(&h.url).await.expect("first upgrade");
    wait_for_slots(&h.state, 1).await;
    assert_unavailable(connect(&h.url).await);
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1
    );
    drop(first);
    wait_for_slots(&h.state, 0).await;
    let second = connect(&h.url).await.expect("reconnect after cleanup");
    wait_for_slots(&h.state, 1).await;
    drop(second);
    wait_for_slots(&h.state, 0).await;
}

#[tokio::test]
async fn clustering_requires_the_app_state_identity_source() {
    let h = start(1, Some(None), AdmissionTestBarriers::default()).await;
    assert_unavailable(connect(&h.url).await);
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn fence_and_recovery_during_parse_cannot_promote_an_old_request() {
    let barrier = Arc::new(AdmissionTestBarrier::default());
    let h = start(
        1,
        None,
        AdmissionTestBarriers {
            before_parse: Some(Arc::clone(&barrier)),
            ..Default::default()
        },
    )
    .await;
    let url = h.url.clone();
    let request = tokio::spawn(async move { connect(&url).await });
    wait_entered(&barrier).await;
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
    h.state
        .deps
        .app_state
        .node_lifecycle
        .begin_fenced_recovery();
    h.state.deps.app_state.node_lifecycle.serve();
    barrier.release();
    assert_unavailable(
        tokio::time::timeout(DEADLINE, request)
            .await
            .expect("HTTP response")
            .expect("request task"),
    );
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn identity_aba_during_parse_cannot_promote_an_old_request() {
    let first = NodeIdentity::new("node", "first");
    let identity = SharedNodeIdentity::new(first.clone());
    let barrier = Arc::new(AdmissionTestBarrier::default());
    let h = start(
        1,
        Some(Some(identity.clone())),
        AdmissionTestBarriers {
            before_parse: Some(Arc::clone(&barrier)),
            ..Default::default()
        },
    )
    .await;
    let url = h.url.clone();
    let request = tokio::spawn(async move { connect(&url).await });
    wait_entered(&barrier).await;
    identity.rotate(NodeIdentity::new("node", "second")).await;
    identity.rotate(first).await;
    barrier.release();
    assert_unavailable(
        tokio::time::timeout(DEADLINE, request)
            .await
            .expect("HTTP response")
            .expect("request task"),
    );
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
}

#[tokio::test]
async fn source_revocation_before_response_rejects_and_releases_reservation() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "first"));
    let barrier = Arc::new(AdmissionTestBarrier::default());
    let h = start(
        1,
        Some(Some(identity.clone())),
        AdmissionTestBarriers {
            before_response: Some(Arc::clone(&barrier)),
            ..Default::default()
        },
    )
    .await;
    let url = h.url.clone();
    let request = tokio::spawn(async move { connect(&url).await });
    wait_entered(&barrier).await;
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1
    );
    identity.disable().await;
    barrier.release();
    assert_unavailable(
        tokio::time::timeout(DEADLINE, request)
            .await
            .expect("HTTP response")
            .expect("request task"),
    );
    wait_for_slots(&h.state, 0).await;
}

#[tokio::test]
async fn source_revocation_in_callback_sends_close_and_releases_reservation() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "first"));
    let barrier = Arc::new(AdmissionTestBarrier::default());
    let h = start(
        1,
        Some(Some(identity.clone())),
        AdmissionTestBarriers {
            in_callback: Some(Arc::clone(&barrier)),
            ..Default::default()
        },
    )
    .await;
    let mut socket = connect(&h.url).await.expect("HTTP 101");
    wait_entered(&barrier).await;
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1
    );
    identity.disable().await;
    barrier.release();
    let frame = tokio::time::timeout(DEADLINE, socket.next())
        .await
        .expect("close frame");
    assert!(matches!(frame, Some(Ok(Message::Close(_)))), "{frame:?}");
    wait_for_slots(&h.state, 0).await;
}

#[tokio::test]
async fn lifecycle_revocation_in_callback_sends_close_and_releases_reservation() {
    let barrier = Arc::new(AdmissionTestBarrier::default());
    let h = start(
        1,
        None,
        AdmissionTestBarriers {
            in_callback: Some(Arc::clone(&barrier)),
            ..Default::default()
        },
    )
    .await;
    let mut socket = connect(&h.url).await.expect("HTTP 101");
    wait_entered(&barrier).await;
    h.state
        .deps
        .app_state
        .node_lifecycle
        .begin_fenced_recovery();
    h.state.deps.app_state.node_lifecycle.serve();
    barrier.release();
    let frame = tokio::time::timeout(DEADLINE, socket.next())
        .await
        .expect("close frame");
    assert!(matches!(frame, Some(Ok(Message::Close(_)))), "{frame:?}");
    wait_for_slots(&h.state, 0).await;
}

#[tokio::test]
async fn capacity_remains_held_until_connection_cleanup_finishes() {
    let barrier = Arc::new(AdmissionTestBarrier::default());
    let h = start(
        1,
        None,
        AdmissionTestBarriers {
            before_cleanup: Some(Arc::clone(&barrier)),
            ..Default::default()
        },
    )
    .await;
    let first = connect(&h.url).await.expect("first upgrade");
    drop(first);
    wait_entered(&barrier).await;
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1
    );
    assert_unavailable(connect(&h.url).await);
    barrier.release();
    wait_for_slots(&h.state, 0).await;
    let second = connect(&h.url).await.expect("capacity returned");
    drop(second);
    wait_entered(&barrier).await;
    barrier.release();
    wait_for_slots(&h.state, 0).await;
}

#[tokio::test]
async fn unpolled_upgrade_waiter_drop_keeps_owner_until_unstarted_socket_closes() {
    let in_callback = Arc::new(AdmissionTestBarrier::default());
    let control = Arc::new(CallbackWaiterTestControl {
        drop_before_poll: true,
        ..Default::default()
    });
    let h = start(
        1,
        None,
        AdmissionTestBarriers {
            in_callback: Some(Arc::clone(&in_callback)),
            callback_waiter: Some(Arc::clone(&control)),
            ..Default::default()
        },
    )
    .await;
    let mut peer = connect(&h.url).await.expect("upgrade");
    wait_for_waiter_drop(&control).await;
    wait_entered(&in_callback).await;
    assert_connection_owned(&h.state);
    assert_unavailable(connect(&h.url).await);

    in_callback.release();
    let frame = tokio::time::timeout(DEADLINE, peer.next())
        .await
        .expect("close frame");
    assert!(matches!(frame, Some(Ok(Message::Close(_)))), "{frame:?}");
    wait_for_drain(&h.state).await;
}

#[tokio::test]
async fn dropping_waiter_during_handler_finishes_handler_then_cleanup() {
    let handler = Arc::new(AdmissionTestBarrier::default());
    let cleanup = Arc::new(AdmissionTestBarrier::default());
    let after_cleanup = Arc::new(AdmissionTestBarrier::default());
    let control = Arc::new(CallbackWaiterTestControl::default());
    let h = start(
        1,
        None,
        AdmissionTestBarriers {
            in_inbound_handler: Some(Arc::clone(&handler)),
            before_cleanup: Some(Arc::clone(&cleanup)),
            after_cleanup: Some(Arc::clone(&after_cleanup)),
            callback_waiter: Some(Arc::clone(&control)),
            ..Default::default()
        },
    )
    .await;
    let mut peer = connect(&h.url).await.expect("upgrade");
    peer.send(Message::Text(
        "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' to='example.com' version='1.0'/>".into(),
    ))
    .await
    .expect("open frame");
    wait_entered(&handler).await;
    control.drop_waiter.cancel();
    wait_for_waiter_drop(&control).await;
    assert_connection_owned(&h.state);

    handler.release();
    let response = tokio::time::timeout(DEADLINE, peer.next())
        .await
        .expect("handler response")
        .expect("response frame")
        .expect("websocket response");
    assert!(matches!(response, Message::Text(text) if text.contains("<open")));
    wait_entered(&cleanup).await;
    assert_connection_owned(&h.state);
    cleanup.release();
    wait_entered(&after_cleanup).await;
    assert_connection_owned(&h.state);
    after_cleanup.release();
    wait_for_drain(&h.state).await;
}

#[tokio::test]
async fn dropping_waiter_during_cleanup_does_not_interrupt_cleanup() {
    let cleanup = Arc::new(AdmissionTestBarrier::default());
    let after_cleanup = Arc::new(AdmissionTestBarrier::default());
    let control = Arc::new(CallbackWaiterTestControl::default());
    let h = start(
        1,
        None,
        AdmissionTestBarriers {
            before_cleanup: Some(Arc::clone(&cleanup)),
            after_cleanup: Some(Arc::clone(&after_cleanup)),
            callback_waiter: Some(Arc::clone(&control)),
            ..Default::default()
        },
    )
    .await;
    let peer = connect(&h.url).await.expect("upgrade");
    drop(peer);
    wait_entered(&cleanup).await;
    control.drop_waiter.cancel();
    wait_for_waiter_drop(&control).await;
    assert_connection_owned(&h.state);

    cleanup.release();
    wait_entered(&after_cleanup).await;
    assert_connection_owned(&h.state);
    after_cleanup.release();
    wait_for_drain(&h.state).await;
    assert_eq!(
        h.state.deps.app_state.node_lifecycle.critical_failure(),
        None
    );
}

async fn owner_panic_keeps_guard_until_fenced(
    drop_waiter: bool,
    stopping: bool,
    host: Option<LiveConnectionHost>,
) {
    let active = Arc::new(AdmissionTestBarrier::default());
    let caught = Arc::new(AdmissionTestBarrier::default());
    let fenced = Arc::new(AdmissionTestBarrier::default());
    let control = Arc::new(CallbackWaiterTestControl::default());
    let h = start_hosted(
        1,
        None,
        AdmissionTestBarriers {
            owner_panic_active: Some(Arc::clone(&active)),
            owner_panic_caught: Some(Arc::clone(&caught)),
            owner_panic_fenced: Some(Arc::clone(&fenced)),
            callback_waiter: Some(Arc::clone(&control)),
            ..Default::default()
        },
        host,
        None,
    )
    .await;
    let permit = h
        .state
        .deps
        .app_state
        .node_lifecycle
        .admit()
        .expect("initial serving permit");
    let _peer = connect(&h.url).await.expect("upgrade");
    wait_entered(&active).await;
    assert_connection_owned(&h.state);

    if drop_waiter {
        control.drop_waiter.cancel();
        wait_for_waiter_drop(&control).await;
    }
    if stopping {
        h.state.deps.shutdown.stop_token().cancel();
    }
    active.release();
    wait_entered(&caught).await;
    // The original owner has unwound, but its external guard still keeps
    // shutdown from treating this connection as drained.
    assert_eq!(h.state.deps.shutdown.active_connections(), 1);
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
    assert!(
        !h.state
            .deps
            .shutdown
            .wait_for_connections_drained_for(Duration::from_millis(20))
            .await
    );
    caught.release();
    wait_entered(&fenced).await;
    assert_eq!(h.state.deps.shutdown.active_connections(), 1);
    assert!(h.state.deps.shutdown.stop_token().is_cancelled());
    assert_eq!(
        h.state.deps.app_state.node_lifecycle.critical_failure(),
        Some(waddle_server::clustering::CriticalNodeFailure::WebSocketOwnerPanicked)
    );
    assert!(permit.revalidate().is_err());
    assert_unavailable(connect(&h.url).await);
    assert!(
        !h.state
            .deps
            .shutdown
            .wait_for_connections_drained_for(Duration::from_millis(20))
            .await
    );
    fenced.release();
    wait_for_drain(&h.state).await;
}

#[tokio::test]
async fn owner_panic_fences_before_releasing_drain_guard() {
    owner_panic_keeps_guard_until_fenced(false, false, None).await;
}

#[tokio::test]
async fn owner_panic_during_shutdown_fences_after_waiter_loss() {
    owner_panic_keeps_guard_until_fenced(true, true, None).await;
}

#[tokio::test]
async fn lifecycle_revocation_before_response_rejects_and_releases_reservation() {
    let barrier = Arc::new(AdmissionTestBarrier::default());
    let h = start(
        1,
        None,
        AdmissionTestBarriers {
            before_response: Some(Arc::clone(&barrier)),
            ..Default::default()
        },
    )
    .await;
    let url = h.url.clone();
    let request = tokio::spawn(async move { connect(&url).await });
    wait_entered(&barrier).await;
    assert_eq!(
        h.state.deps.app_state.websocket_inventory_for_test().len(),
        1
    );
    h.state
        .deps
        .app_state
        .node_lifecycle
        .begin_fenced_recovery();
    h.state.deps.app_state.node_lifecycle.serve();
    barrier.release();
    assert_unavailable(
        tokio::time::timeout(DEADLINE, request)
            .await
            .expect("HTTP response")
            .expect("request task"),
    );
    wait_for_slots(&h.state, 0).await;
}

/// The actor's mailbox bound. Kept in step with the server by the flood
/// assertions below, which fail if the mailbox accepts more or fewer.
const LIVE_ACTOR_MAILBOX: usize = 256;
const DOMAIN: &str = "example.com";
const PASSWORD: &str = "live-actor-password";
const SM_NS: &str = "urn:xmpp:sm:3";

type Peer =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Default)]
struct ActorSetup {
    identity: Option<Option<SharedNodeIdentity>>,
    barriers: AdmissionTestBarriers,
    keepalive: Option<KeepaliveConfig>,
}

/// A server whose connections are hosted by the actor, and the tap that
/// observes the first of them.
async fn start_actor(setup: ActorSetup) -> (Harness, Arc<LiveActorTestTap>) {
    let tap = Arc::new(LiveActorTestTap::default());
    let h = start_hosted(
        4,
        setup.identity,
        AdmissionTestBarriers {
            live_actor: Some(Arc::clone(&tap)),
            ..setup.barriers
        },
        Some(LiveConnectionHost::Actor),
        setup.keepalive,
    )
    .await;
    (h, tap)
}

/// Start one actor-hosted connection and wait until its loop has handled a
/// frame, so the actor is past startup and parked in its wait.
async fn start_actor_hosted(setup: ActorSetup) -> (Harness, Arc<LiveActorTestTap>, Peer) {
    let (h, tap) = start_actor(setup).await;
    let mut peer = connect(&h.url).await.expect("upgrade");
    tokio::time::timeout(DEADLINE, tap.wait_until_hosted())
        .await
        .expect("actor hosted");
    round_trip(&mut peer).await;
    (h, tap, peer)
}

async fn round_trip(peer: &mut Peer) {
    peer.send(Message::Ping(vec![7].into()))
        .await
        .expect("ping sent");
    tokio::time::timeout(DEADLINE, async {
        loop {
            match peer.next().await {
                Some(Ok(Message::Pong(_))) => break,
                Some(Ok(_)) => {}
                other => panic!("no pong: {other:?}"),
            }
        }
    })
    .await
    .expect("pong");
}

async fn add_account(h: &Harness, localpart: &str) {
    waddle_server::test_support::seed_local_account(&h.state, localpart).await;
    waddle_server::auth::NativeUserStore::new(
        h.state.deps.app_state.db_pool.global_actor().clone(),
    )
    .register(waddle_server::auth::RegisterRequest {
        username: localpart.to_owned(),
        domain: DOMAIN.to_owned(),
        password: PASSWORD.to_owned(),
        email: None,
    })
    .await
    .expect("register native SCRAM account");
}

async fn authenticated(h: &Harness, localpart: &str) -> WsXmppClient {
    let mut client = WsXmppClient::connect(&h.url).await.expect("connect");
    client
        .authenticate(DOMAIN, localpart, PASSWORD)
        .await
        .expect("authentication");
    client
}

async fn bound(h: &Harness, localpart: &str, resource: &str) -> WsXmppClient {
    let mut client = authenticated(h, localpart).await;
    client.bind(resource).await.expect("bind");
    client
}

async fn send_element(client: &mut WsXmppClient, element: minidom::Element) {
    client
        .send(&String::from(&element))
        .await
        .expect("send XML");
}

fn sm_element(name: &str) -> minidom::ElementBuilder {
    minidom::Element::builder(name, SM_NS)
}

fn full_jid(localpart: &str, resource: &str) -> jid::FullJid {
    format!("{localpart}@{DOMAIN}/{resource}")
        .parse()
        .expect("full JID")
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    tokio::time::timeout(DEADLINE, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
}

/// Park the hosted loop at the top of a turn and fill its mailbox. Returns
/// the number of messages handled so far, which includes the one that woke
/// the loop into the parked turn.
async fn park_with_full_mailbox(tap: &LiveActorTestTap) -> usize {
    tokio::time::timeout(DEADLINE, tap.park())
        .await
        .expect("loop parked");
    assert_eq!(tap.flood(LIVE_ACTOR_MAILBOX + 1), LIVE_ACTOR_MAILBOX);
    tap.pings_handled()
}

/// The suites that spawn a server process name the host selection through
/// the harness crate, which cannot depend on the server.
#[test]
fn harness_and_server_agree_on_the_host_selection_variable() {
    assert_eq!(
        waddle_ws_test_support::LIVE_CONNECTION_HOST_ENV,
        waddle_server::test_support::LIVE_CONNECTION_HOST_ENV
    );
}

fn assert_not_failed(state: &WebSocketState) {
    assert_eq!(state.deps.app_state.node_lifecycle.critical_failure(), None);
}

fn assert_fenced_as_owner_panic(state: &WebSocketState) {
    assert!(state.deps.shutdown.stop_token().is_cancelled());
    assert_eq!(
        state.deps.app_state.node_lifecycle.critical_failure(),
        Some(waddle_server::clustering::CriticalNodeFailure::WebSocketOwnerPanicked)
    );
}

#[tokio::test]
async fn actor_host_settles_when_the_peer_closes() {
    let after_cleanup = Arc::new(AdmissionTestBarrier::default());
    let (h, tap, peer) = start_actor_hosted(ActorSetup {
        barriers: AdmissionTestBarriers {
            after_cleanup: Some(Arc::clone(&after_cleanup)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    assert_connection_owned(&h.state);
    drop(peer);
    // Reaching the barrier proves the connection-owned cleanup ran.
    wait_entered(&after_cleanup).await;
    assert_connection_owned(&h.state);
    after_cleanup.release();
    wait_for_drain(&h.state).await;
    assert_not_failed(&h.state);
    assert_eq!(tap.pings_handled(), 0);
}

#[tokio::test]
async fn actor_host_owner_panic_fences_before_releasing_drain_guard() {
    owner_panic_keeps_guard_until_fenced(false, false, Some(LiveConnectionHost::Actor)).await;
}

#[tokio::test]
async fn actor_host_owner_panic_during_shutdown_fences_after_waiter_loss() {
    owner_panic_keeps_guard_until_fenced(true, true, Some(LiveConnectionHost::Actor)).await;
}

/// The checks every unsettled stop shares: the slot is free before the fence,
/// the drain guard is held until after it, and cleanup never ran.
async fn assert_fenced_without_settling(
    h: &Harness,
    caught: &AdmissionTestBarrier,
    fenced: &AdmissionTestBarrier,
    after_cleanup: &AdmissionTestBarrier,
) {
    wait_entered(caught).await;
    // The slot is free, so the socket halves and the cleanup guard were
    // dropped, but nothing is fenced or drained yet.
    assert_eq!(h.state.deps.shutdown.active_connections(), 1);
    assert!(h
        .state
        .deps
        .app_state
        .websocket_inventory_for_test()
        .is_empty());
    assert_not_failed(&h.state);
    caught.release();
    wait_entered(fenced).await;
    assert_eq!(h.state.deps.shutdown.active_connections(), 1);
    assert_fenced_as_owner_panic(&h.state);
    fenced.release();
    wait_for_drain(&h.state).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            after_cleanup.wait_until_entered()
        )
        .await
        .is_err(),
        "an unsettled owner must not reach connection cleanup"
    );
}

struct PanicBarriers {
    caught: Arc<AdmissionTestBarrier>,
    fenced: Arc<AdmissionTestBarrier>,
    after_cleanup: Arc<AdmissionTestBarrier>,
}

impl PanicBarriers {
    fn new() -> Self {
        Self {
            caught: Arc::default(),
            fenced: Arc::default(),
            after_cleanup: Arc::default(),
        }
    }

    fn barriers(&self) -> AdmissionTestBarriers {
        AdmissionTestBarriers {
            owner_panic_caught: Some(Arc::clone(&self.caught)),
            owner_panic_fenced: Some(Arc::clone(&self.fenced)),
            after_cleanup: Some(Arc::clone(&self.after_cleanup)),
            ..Default::default()
        }
    }
}

#[tokio::test]
async fn killing_the_idle_hosting_actor_fences_as_an_owner_panic() {
    let panic = PanicBarriers::new();
    let (h, tap, _peer) = start_actor_hosted(ActorSetup {
        barriers: panic.barriers(),
        ..Default::default()
    })
    .await;
    assert_connection_owned(&h.state);

    tap.kill();
    assert_fenced_without_settling(&h, &panic.caught, &panic.fenced, &panic.after_cleanup).await;
}

#[tokio::test]
async fn killing_the_hosting_actor_inside_a_handler_fences_as_an_owner_panic() {
    let panic = PanicBarriers::new();
    let in_handler = Arc::new(AdmissionTestBarrier::default());
    let (h, tap, mut peer) = start_actor_hosted(ActorSetup {
        barriers: AdmissionTestBarriers {
            in_inbound_handler: Some(Arc::clone(&in_handler)),
            ..panic.barriers()
        },
        ..Default::default()
    })
    .await;
    // The handler of this frame stops at the barrier, inside `handle_event`,
    // and is never released: the kill aborts it at that await.
    let open = minidom::Element::builder("open", "urn:ietf:params:xml:ns:xmpp-framing")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), DOMAIN)
        .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1.0")
        .build();
    peer.send(Message::Text(String::from(&open).into()))
        .await
        .expect("open sent");
    wait_entered(&in_handler).await;
    assert_connection_owned(&h.state);

    tap.kill();
    assert_fenced_without_settling(&h, &panic.caught, &panic.fenced, &panic.after_cleanup).await;
}

#[tokio::test]
async fn killing_the_hosting_actor_during_settlement_lets_settlement_finish() {
    let before_cleanup = Arc::new(AdmissionTestBarrier::default());
    let after_cleanup = Arc::new(AdmissionTestBarrier::default());
    let (h, tap, peer) = start_actor_hosted(ActorSetup {
        barriers: AdmissionTestBarriers {
            before_cleanup: Some(Arc::clone(&before_cleanup)),
            after_cleanup: Some(Arc::clone(&after_cleanup)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    drop(peer);
    wait_entered(&before_cleanup).await;

    tap.kill();
    before_cleanup.release();
    wait_entered(&after_cleanup).await;
    assert_connection_owned(&h.state);
    after_cleanup.release();
    wait_for_drain(&h.state).await;
    assert_not_failed(&h.state);
    assert!(!h.state.deps.shutdown.stop_token().is_cancelled());
}

async fn panic_during_settlement_fences(host: LiveConnectionHost) {
    let settling = Arc::new(AdmissionTestBarrier::default());
    let panic = PanicBarriers::new();
    let h = start_hosted(
        1,
        None,
        AdmissionTestBarriers {
            owner_panic_settling: Some(Arc::clone(&settling)),
            ..panic.barriers()
        },
        Some(host),
        None,
    )
    .await;
    let mut peer = connect(&h.url).await.expect("upgrade");
    round_trip(&mut peer).await;
    drop(peer);
    wait_entered(&settling).await;
    assert_connection_owned(&h.state);
    assert_not_failed(&h.state);

    settling.release();
    assert_fenced_without_settling(&h, &panic.caught, &panic.fenced, &panic.after_cleanup).await;
}

#[tokio::test]
async fn panic_during_settlement_fences_and_releases_the_slot() {
    panic_during_settlement_fences(LiveConnectionHost::PlainTask).await;
}

#[tokio::test]
async fn actor_host_panic_during_settlement_fences_and_releases_the_slot() {
    panic_during_settlement_fences(LiveConnectionHost::Actor).await;
}

/// What is left of one live connection after its runtime, and with it the
/// owner task, was dropped mid-loop.
#[derive(Debug, PartialEq, Eq)]
struct AfterCancellation {
    slots: usize,
    active_connections: usize,
    failed: bool,
    stopping: bool,
}

fn cancel_owner_mid_loop(host: LiveConnectionHost) -> AfterCancellation {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (h, peer) = runtime.block_on(async {
        let h = start_hosted(1, None, AdmissionTestBarriers::default(), Some(host), None).await;
        let mut peer = connect(&h.url).await.expect("upgrade");
        round_trip(&mut peer).await;
        assert_connection_owned(&h.state);
        (h, peer)
    });
    drop(runtime);
    let after = AfterCancellation {
        slots: h.state.deps.app_state.websocket_inventory_for_test().len(),
        active_connections: h.state.deps.shutdown.active_connections(),
        failed: h
            .state
            .deps
            .app_state
            .node_lifecycle
            .critical_failure()
            .is_some(),
        stopping: h.state.deps.shutdown.stop_token().is_cancelled(),
    };
    drop(peer);
    after
}

#[test]
fn cancelling_the_owner_task_leaves_the_same_state_under_either_host() {
    let plain = cancel_owner_mid_loop(LiveConnectionHost::PlainTask);
    let actor = cancel_owner_mid_loop(LiveConnectionHost::Actor);
    assert_eq!(actor, plain);
    // Cancellation is not a panic: the drain guard is released and nothing
    // is fenced.
    assert_eq!(plain.active_connections, 0);
    assert!(!plain.failed);
    assert!(!plain.stopping);
}

/// With the loop parked and the mailbox full, make a control source ready and
/// let the loop wait. Control must win that wait: the connection ends before
/// any queued message is handled.
async fn assert_control_precedes_full_mailbox(
    h: &Harness,
    tap: &LiveActorTestTap,
    control: impl std::future::Future<Output = ()>,
    slots_after: usize,
) {
    let handled = park_with_full_mailbox(tap).await;
    control.await;
    tap.resume();
    wait_for_slots(&h.state, slots_after).await;
    assert_eq!(tap.pings_handled(), handled);
}

#[tokio::test]
async fn full_actor_mailbox_does_not_delay_shutdown() {
    let (h, tap, _peer) = start_actor_hosted(ActorSetup::default()).await;
    assert_control_precedes_full_mailbox(
        &h,
        &tap,
        async { h.state.deps.shutdown.stop_token().cancel() },
        0,
    )
    .await;
    assert_not_failed(&h.state);
}

#[tokio::test]
async fn full_actor_mailbox_does_not_delay_permit_revocation() {
    let (h, tap, _peer) = start_actor_hosted(ActorSetup::default()).await;
    assert_control_precedes_full_mailbox(
        &h,
        &tap,
        async {
            h.state
                .deps
                .app_state
                .node_lifecycle
                .begin_fenced_recovery();
            h.state.deps.app_state.node_lifecycle.serve();
        },
        0,
    )
    .await;
    assert_not_failed(&h.state);
}

#[tokio::test]
async fn full_actor_mailbox_does_not_delay_identity_revocation() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "first"));
    let (h, tap, _peer) = start_actor_hosted(ActorSetup {
        identity: Some(Some(identity.clone())),
        ..Default::default()
    })
    .await;
    assert_control_precedes_full_mailbox(&h, &tap, identity.disable(), 0).await;
    assert_not_failed(&h.state);
}

#[tokio::test]
async fn full_actor_mailbox_does_not_delay_settlement_after_waiter_loss() {
    let control = Arc::new(CallbackWaiterTestControl::default());
    let (h, tap, _peer) = start_actor_hosted(ActorSetup {
        barriers: AdmissionTestBarriers {
            callback_waiter: Some(Arc::clone(&control)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    assert_control_precedes_full_mailbox(
        &h,
        &tap,
        async {
            control.drop_waiter.cancel();
            wait_for_waiter_drop(&control).await;
        },
        0,
    )
    .await;
    assert_not_failed(&h.state);
}

#[tokio::test]
async fn full_actor_mailbox_does_not_delay_force_detach() {
    let (h, tap) = start_actor(ActorSetup::default()).await;
    add_account(&h, "alice").await;
    let _alice = bound(&h, "alice", "phone").await;
    let jid = full_jid("alice", "phone");
    let (ack, detached) = tokio::sync::oneshot::channel();

    assert_control_precedes_full_mailbox(
        &h,
        &tap,
        async {
            let entry = h
                .state
                .deps
                .protocol
                .connection_registry
                .entry(&jid)
                .expect("registered connection");
            entry
                .try_force_detach_for_jid(
                    &jid,
                    ForceDetachRequest {
                        origin: ForceDetachOrigin::OwnerManagedRetirement,
                        requester_bare_jid: jid.to_bare(),
                        target_binding: None,
                        ack,
                    },
                )
                .expect("force-detach queued");
        },
        0,
    )
    .await;
    // Without stream management there is nothing to persist for a resumer;
    // either answer shows the owner finished its own cleanup first.
    let outcome = tokio::time::timeout(DEADLINE, detached)
        .await
        .expect("force-detach acknowledged")
        .expect("acknowledgement sent");
    assert!(
        matches!(
            outcome,
            ForceDetachOutcome::Detached | ForceDetachOutcome::NotPersisted
        ),
        "{outcome:?}"
    );
    assert_not_failed(&h.state);
}

#[tokio::test]
async fn full_actor_mailbox_does_not_delay_a_live_resume() {
    let (h, tap) = start_actor(ActorSetup::default()).await;
    add_account(&h, "alice").await;
    let mut original = bound(&h, "alice", "phone").await;
    send_element(
        &mut original,
        sm_element("enable")
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled: minidom::Element = original
        .recv()
        .await
        .expect("enabled")
        .parse()
        .expect("XML");
    assert!(enabled.is("enabled", SM_NS), "{enabled:?}");
    let stream = enabled.attr("id").expect("stream id").to_owned();

    let mut resumer = authenticated(&h, "alice").await;
    let handled = park_with_full_mailbox(&tap).await;
    send_element(
        &mut resumer,
        sm_element("resume")
            .attr(minidom::rxml::xml_ncname!("previd").to_owned(), stream)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
            .build(),
    )
    .await;
    // The resumer's request reaches the parked owner's control without any
    // signal this test can observe, so give it time to be queued.
    tokio::time::sleep(Duration::from_millis(500)).await;
    tap.resume();

    let resumed: minidom::Element = resumer
        .recv()
        .await
        .expect("resume answer")
        .parse()
        .expect("XML");
    assert!(resumed.is("resumed", SM_NS), "{resumed:?}");
    wait_for_slots(&h.state, 1).await;
    assert_eq!(tap.pings_handled(), handled);
    assert_not_failed(&h.state);
}

#[tokio::test]
async fn full_actor_mailbox_drains_while_the_connection_keeps_serving() {
    let (h, tap, mut peer) = start_actor_hosted(ActorSetup::default()).await;

    assert_eq!(tap.flood(LIVE_ACTOR_MAILBOX + 1), LIVE_ACTOR_MAILBOX);
    assert!(tokio::time::timeout(DEADLINE, tap.ping())
        .await
        .expect("queued messages drain"));
    assert_eq!(tap.pings_handled(), LIVE_ACTOR_MAILBOX + 1);
    round_trip(&mut peer).await;
    assert_connection_owned(&h.state);
    assert_not_failed(&h.state);
}

/// With the loop parked, queue inbound frames in the socket, stanzas in the
/// outbound queue and messages in the mailbox, then let the loop run. Every
/// wait then finds a command and connection data ready together, so the order
/// in which they are handled shows the scheduling: they must alternate.
#[tokio::test]
async fn commands_and_connection_data_take_turns_while_both_stay_ready() {
    const INBOUND: usize = 30;
    const OUTBOUND: usize = 30;
    const COMMANDS: usize = 100;
    // The outbound stanzas are queued before the loop resumes, so at least
    // this many of its first waits have data ready whatever the socket does.
    const CHECKED: usize = 10;

    let (h, tap) = start_actor(ActorSetup::default()).await;
    add_account(&h, "alice").await;
    add_account(&h, "bob").await;
    let mut alice = bound(&h, "alice", "phone").await;
    let mut bob = bound(&h, "bob", "laptop").await;

    tokio::time::timeout(DEADLINE, tap.park())
        .await
        .expect("loop parked");
    let pings_before = tap.pings_handled();
    let events_before = tap.pings_before_each_loop_event().len();

    alice
        .send_raw_burst((0..INBOUND).map(|n| {
            let ping = xmpp_parsers::iq::Iq::from_get(format!("in-{n}"), xmpp_parsers::ping::Ping);
            Message::Text(String::from(&minidom::Element::from(ping)).into())
        }))
        .await
        .expect("inbound burst");
    for n in 0..OUTBOUND {
        let mut message =
            xmpp_parsers::message::Message::new(Some(full_jid("alice", "phone").into()));
        message.id = Some(xmpp_parsers::message::Id(format!("out-{n}")));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), format!("out-{n}"));
        send_element(&mut bob, message.into()).await;
    }
    // Bob's own server ping is answered after his messages were handled.
    send_element(
        &mut bob,
        xmpp_parsers::iq::Iq::from_get("bob-done", xmpp_parsers::ping::Ping).into(),
    )
    .await;
    bob.recv_matching(|frame| frame.contains("bob-done"))
        .await
        .expect("bob's stanzas were handled");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(tap.flood(COMMANDS), COMMANDS);
    tap.resume();

    let mut inbound_answered = 0;
    let mut outbound_delivered = 0;
    while inbound_answered < INBOUND || outbound_delivered < OUTBOUND {
        let frame = alice.recv().await.expect("alice's stream keeps flowing");
        let element: minidom::Element = frame.parse().expect("XML");
        match element.attr("id") {
            Some(id) if id.starts_with("in-") => inbound_answered += 1,
            Some(id) if id.starts_with("out-") => outbound_delivered += 1,
            _ => {}
        }
    }
    wait_until("every queued command was handled", || {
        tap.pings_handled() == pings_before + COMMANDS
    })
    .await;

    // A busy connection delays a command by one event at most.
    let events_before_ping = tap.loop_events_before_each_ping();
    for (nth, events) in events_before_ping[pings_before..]
        .iter()
        .take(CHECKED)
        .enumerate()
    {
        assert!(
            events - events_before <= nth + 1,
            "command {nth} waited behind {} connection events",
            events - events_before
        );
    }
    // A mailbox that stays full delays a connection event by one command at
    // most.
    let pings_before_event = tap.pings_before_each_loop_event();
    for (nth, pings) in pings_before_event[events_before..]
        .iter()
        .take(CHECKED)
        .enumerate()
    {
        assert!(
            pings - pings_before <= nth + 2,
            "connection event {nth} waited behind {} commands",
            pings - pings_before
        );
    }
    assert_not_failed(&h.state);
}

/// A producer that keeps the mailbox from ever running dry must not stop the
/// keepalive timer from firing or the socket from being read.
#[tokio::test]
async fn refilled_actor_mailbox_does_not_delay_keepalive_or_inbound() {
    let (h, tap) = start_actor(ActorSetup {
        keepalive: Some(KeepaliveConfig {
            interval_ms: 200,
            miss_limit: 1_000,
        }),
        ..Default::default()
    })
    .await;
    add_account(&h, "alice").await;
    let mut alice = bound(&h, "alice", "phone").await;
    let refill = tokio::spawn({
        let tap = Arc::clone(&tap);
        async move {
            loop {
                tap.flood(LIVE_ACTOR_MAILBOX);
                tokio::task::yield_now().await;
            }
        }
    });

    // Two server probes: the timer fires and is re-armed under the refill.
    let mut probes = 0;
    while probes < 2 {
        match alice.recv_raw_timeout(Duration::from_secs(3)).await {
            Ok(Some(Message::Ping(_))) => probes += 1,
            Ok(Some(_)) => {}
            other => panic!("no keepalive probe under a refilled mailbox: {other:?}"),
        }
    }
    send_element(
        &mut alice,
        xmpp_parsers::iq::Iq::from_get("under-refill", xmpp_parsers::ping::Ping).into(),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match alice.recv_raw_timeout(Duration::from_secs(3)).await {
                Ok(Some(Message::Text(frame))) if frame.contains("under-refill") => break,
                Ok(Some(_)) => {}
                other => panic!("stream ended under a refilled mailbox: {other:?}"),
            }
        }
    })
    .await
    .expect("inbound stanza answered under a refilled mailbox");

    refill.abort();
    assert!(tap.pings_handled() > LIVE_ACTOR_MAILBOX);
    assert_connection_owned(&h.state);
    assert_not_failed(&h.state);
}
