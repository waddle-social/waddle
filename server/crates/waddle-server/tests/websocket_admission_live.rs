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
use waddle_server::test_support::{
    router, websocket_state_with_admission, WebSocketAdmissionTestConfig, WebSocketState,
};
use waddle_server::websocket_transport::{
    AdmissionTestBarrier, AdmissionTestBarriers, CallbackWaiterTestControl,
};
use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

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
    let state = websocket_state_with_admission(WebSocketAdmissionTestConfig {
        capacity: MaxWebSocketConnections::from_raw(Some(&capacity.to_string())).expect("capacity"),
        clustered_identity: identity,
        barriers,
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
