//! Dark admission and pump-handoff tests. No protocol owner is activated.
#![cfg(feature = "test-support")]

use axum::extract::ws::Message;
use axum::serve::Listener;
use futures::task::AtomicWaker;
use futures::{Sink, Stream};
use std::future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::{timeout, Instant};
use tokio_util::sync::CancellationToken;
use waddle_server::clustering::NodeLifecycle;
use waddle_server::websocket_transport::{
    axum_send_after_close_for_test, start_websocket_transport, start_websocket_transport_with_hook,
    CustodianState, IoCustodianInventory, Limits, ReservedStartStage, SocketAdmissionError,
    TestSocketAdmission, TransportLimits, WriteProgress,
};
use waddle_xmpp::ownership::{ConnectionRef, NodeIdentity, NodeLeaseIdentity, SharedNodeIdentity};

const TIMEOUT: Duration = Duration::from_secs(5);

struct DuplexListener(Option<tokio::io::DuplexStream>);

impl Listener for DuplexListener {
    type Io = tokio::io::DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.take() {
            Some(io) => (io, ()),
            None => future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
}

#[derive(Default)]
struct SinkState {
    ready_stalled: AtomicBool,
    ready_polled: AtomicBool,
    reader_dropped: AtomicBool,
    writer_dropped: AtomicBool,
    frames: Mutex<Vec<Message>>,
    waker: AtomicWaker,
}

struct Reader(Arc<SinkState>);

impl Stream for Reader {
    type Item = io::Result<Message>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.0.reader_dropped.store(true, Ordering::Release);
    }
}

struct Writer(Arc<SinkState>);

impl Sink<Message> for Writer {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.ready_polled.store(true, Ordering::Release);
        self.0.waker.register(cx.waker());
        if self.0.ready_stalled.load(Ordering::Acquire) {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn start_send(self: Pin<&mut Self>, frame: Message) -> io::Result<()> {
        self.0.frames.lock().expect("frames").push(frame);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.0.writer_dropped.store(true, Ordering::Release);
    }
}

fn limits() -> TransportLimits {
    TransportLimits::new(
        Limits::new(4, 1024).expect("inbound"),
        Limits::new(4, 1024).expect("outbound"),
    )
}

fn clustered(
    lifecycle: &NodeLifecycle,
    source: &SharedNodeIdentity,
    inventory: &IoCustodianInventory,
) -> TestSocketAdmission {
    TestSocketAdmission::clustered(
        lifecycle,
        source,
        inventory,
        CancellationToken::new(),
        CancellationToken::new(),
    )
    .expect("clustered admission")
}

fn halves(state: &Arc<SinkState>) -> (Reader, Writer) {
    (Reader(Arc::clone(state)), Writer(Arc::clone(state)))
}

async fn spin_until(mut condition: impl FnMut() -> bool) {
    timeout(TIMEOUT, async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition");
}

#[tokio::test]
async fn exact_reservations_distinguish_sockets_under_the_same_lease() {
    let lifecycle = NodeLifecycle::new();
    let source = SharedNodeIdentity::new(NodeIdentity::new("node", "epoch"));
    let inventory = IoCustodianInventory::new(2).expect("inventory");
    let a = clustered(&lifecycle, &source, &inventory);
    let b = clustered(&lifecycle, &source, &inventory);
    assert_eq!(
        a.connection_ref().node_lease(),
        b.connection_ref().node_lease()
    );
    assert_ne!(a.connection_ref(), b.connection_ref());
    assert_eq!(inventory.len(), 2);
    drop((a, b));
    assert!(inventory.is_empty());
}

#[tokio::test]
async fn wrong_full_reference_releases_both_slots_and_never_calls_factory() {
    let lifecycle = NodeLifecycle::new();
    let source = SharedNodeIdentity::new(NodeIdentity::new("node", "epoch"));
    let inventory = IoCustodianInventory::new(2).expect("inventory");
    let admission = clustered(&lifecycle, &source, &inventory);
    let foreign = ConnectionRef::fresh(admission.connection_ref().node_lease().clone());
    assert_ne!(admission.connection_ref(), &foreign);
    let reservation = inventory.reserve(foreign).expect("foreign slot");
    let invoked = Arc::new(AtomicBool::new(false));
    let result = admission.replace_reservation(reservation);
    match result {
        Err(SocketAdmissionError::WrongReservation) => {}
        Err(other) => panic!("unexpected rejection: {other}"),
        Ok(admission) => {
            let invoked = Arc::clone(&invoked);
            let state = Arc::new(SinkState::default());
            let _ = start_websocket_transport(
                admission,
                limits(),
                TIMEOUT,
                |_: &io::Error| false,
                move |_| {
                    invoked.store(true, Ordering::Release);
                    future::ready(halves(&state))
                },
            )
            .await;
            panic!("foreign reservation accepted");
        }
    }
    assert!(!invoked.load(Ordering::Acquire));
    assert!(inventory.is_empty());
}

#[tokio::test]
async fn revoked_revision_and_cancelled_start_never_invoke_factory() {
    for disabled in [false, true] {
        let lifecycle = NodeLifecycle::new();
        let identity = NodeIdentity::new("node", "a");
        let source = SharedNodeIdentity::new(identity.clone());
        let inventory = IoCustodianInventory::new(1).expect("inventory");
        let admission = clustered(&lifecycle, &source, &inventory);
        if disabled {
            source.disable().await;
        } else {
            source.rotate(NodeIdentity::new("node", "b")).await;
            source.rotate(identity).await;
        }
        let invoked = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&invoked);
        let state = Arc::new(SinkState::default());
        let result = start_websocket_transport(
            admission,
            limits(),
            TIMEOUT,
            |_: &io::Error| false,
            move |_| {
                seen.store(true, Ordering::Release);
                future::ready(halves(&state))
            },
        )
        .await;
        assert!(matches!(result, Err(SocketAdmissionError::IdentityRevoked)));
        assert!(!invoked.load(Ordering::Acquire));
        assert!(inventory.is_empty());
    }

    let lifecycle = NodeLifecycle::new();
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let stop = CancellationToken::new();
    let admission = TestSocketAdmission::standalone(
        &lifecycle,
        &inventory,
        stop.clone(),
        CancellationToken::new(),
    )
    .expect("admission");
    stop.cancel();
    let invoked = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&invoked);
    let state = Arc::new(SinkState::default());
    let result = start_websocket_transport(
        admission,
        limits(),
        TIMEOUT,
        |_: &io::Error| false,
        move |_| {
            seen.store(true, Ordering::Release);
            future::ready(halves(&state))
        },
    )
    .await;
    assert!(matches!(result, Err(SocketAdmissionError::Cancelled)));
    assert!(!invoked.load(Ordering::Acquire));
    assert!(inventory.is_empty());
}

#[tokio::test]
async fn standalone_identity_is_process_scoped_and_simulated_restart_is_distinct() {
    let lifecycle = NodeLifecycle::new();
    let inventory = IoCustodianInventory::new(3).expect("inventory");
    let one = TestSocketAdmission::standalone(
        &lifecycle,
        &inventory,
        CancellationToken::new(),
        CancellationToken::new(),
    )
    .expect("one");
    let two = TestSocketAdmission::standalone(
        &lifecycle,
        &inventory,
        CancellationToken::new(),
        CancellationToken::new(),
    )
    .expect("two");
    let mut other: NodeLeaseIdentity = serde_json::from_str(
        r#"{"LocalProcess":{"process_incarnation":"00000000-0000-4000-8000-000000000001"}}"#,
    )
    .expect("simulated process");
    if &other == one.connection_ref().node_lease() {
        other = serde_json::from_str(
            r#"{"LocalProcess":{"process_incarnation":"00000000-0000-4000-8000-000000000002"}}"#,
        )
        .expect("alternate simulated process");
    }
    let restart = TestSocketAdmission::standalone_with_identity(
        &lifecycle,
        other,
        &inventory,
        CancellationToken::new(),
        CancellationToken::new(),
    )
    .expect("restart");
    assert_eq!(
        one.connection_ref().node_lease(),
        two.connection_ref().node_lease()
    );
    assert_ne!(one.connection_ref(), two.connection_ref());
    assert_ne!(
        one.connection_ref().node_lease(),
        restart.connection_ref().node_lease()
    );
    drop((one, two, restart));
    assert!(inventory.is_empty());
}

#[tokio::test]
async fn stale_identity_never_reauthorizes_a_parked_data_frame() {
    for disabled in [false, true] {
        let lifecycle = NodeLifecycle::new();
        let identity = NodeIdentity::new("node", "a");
        let source = SharedNodeIdentity::new(identity.clone());
        let inventory = IoCustodianInventory::new(1).expect("inventory");
        let admission = clustered(&lifecycle, &source, &inventory);
        let state = Arc::new(SinkState::default());
        state.ready_stalled.store(true, Ordering::Release);
        let factory_state = Arc::clone(&state);
        let (custodian, _input, mut output) = start_websocket_transport(
            admission,
            limits(),
            TIMEOUT,
            |_: &io::Error| false,
            move |_| future::ready(halves(&factory_state)),
        )
        .await
        .expect("start");
        let frame = output.data_frame(Message::text("parked")).expect("frame");
        let receipt = output.submit(frame).await.expect("submit");
        spin_until(|| state.ready_polled.load(Ordering::Acquire)).await;
        if disabled {
            source.disable().await;
        } else {
            source.rotate(NodeIdentity::new("node", "b")).await;
            source.rotate(identity).await;
        }
        state.ready_stalled.store(false, Ordering::Release);
        state.waker.wake();
        assert_eq!(
            timeout(TIMEOUT, receipt.wait_for_terminal())
                .await
                .expect("receipt"),
            WriteProgress::NotAttempted
        );
        assert!(state.frames.lock().expect("frames").is_empty());
        custodian.request_stop(waddle_server::websocket_transport::StopReason::Requested);
        assert_eq!(
            timeout(TIMEOUT, custodian.wait_until_joined())
                .await
                .expect("joined"),
            CustodianState::Joined
        );
    }
}

#[tokio::test(start_paused = true)]
async fn revocation_fences_data_allows_terminal_and_forces_idle_pumps_at_deadline() {
    let lifecycle = NodeLifecycle::new();
    let source = SharedNodeIdentity::new(NodeIdentity::new("node", "a"));
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let admission = clustered(&lifecycle, &source, &inventory);
    let exact = admission.connection_ref().clone();
    let foreign = ConnectionRef::fresh(exact.node_lease().clone());
    let state = Arc::new(SinkState::default());
    let factory_state = Arc::clone(&state);
    let (custodian, _input, mut output) = start_websocket_transport(
        admission,
        limits(),
        Duration::from_secs(1),
        |_: &io::Error| false,
        move |_| future::ready(halves(&factory_state)),
    )
    .await
    .expect("start");
    let witness = custodian.observer().local_io_relinquishment();
    assert!(witness.is_for(&exact));
    assert!(!witness.is_for(&foreign));
    let stale = output.data_frame(Message::text("stale")).expect("data");
    let revoked_at = Instant::now();
    source.rotate(NodeIdentity::new("node", "b")).await;
    source.rotate(NodeIdentity::new("node", "a")).await;
    output.begin_shutdown().expect("shutdown");
    let receipt = output.submit(stale).await.expect("queue stale");
    assert_eq!(
        receipt.wait_for_terminal().await,
        WriteProgress::NotAttempted
    );
    let terminal = output.shutdown_error_frame().expect("terminal frame");
    let receipt = output.submit(terminal).await.expect("queue terminal");
    assert_eq!(receipt.wait_for_terminal().await, WriteProgress::Completed);
    assert_eq!(state.frames.lock().expect("frames").len(), 1);
    assert_eq!(
        timeout(Duration::from_secs(2), custodian.wait_until_joined())
            .await
            .expect("bounded stop"),
        CustodianState::Joined,
    );
    assert!(Instant::now().duration_since(revoked_at) >= Duration::from_secs(1));
    assert!(state.reader_dropped.load(Ordering::Acquire));
    assert!(state.writer_dropped.load(Ordering::Acquire));
    assert_eq!(
        witness.wait().await,
        waddle_xmpp::registry::LocalIoRelinquishmentOutcome::Relinquished
    );
}

#[tokio::test]
async fn revocation_during_each_pump_handoff_stops_the_exact_custodian() {
    for stage in [
        ReservedStartStage::BeforeInboundSpawn,
        ReservedStartStage::BetweenPumpSpawns,
    ] {
        for revoke_lifecycle in [false, true] {
            let lifecycle = NodeLifecycle::new();
            let source = SharedNodeIdentity::new(NodeIdentity::new("node", "a"));
            let inventory = IoCustodianInventory::new(1).expect("inventory");
            let admission = clustered(&lifecycle, &source, &inventory);
            let exact = admission.connection_ref().clone();
            let state = Arc::new(SinkState::default());
            let factory_state = Arc::clone(&state);
            let gate = Arc::new(Semaphore::new(0));
            let hook_gate = Arc::clone(&gate);
            let (reached, mut reached_rx) = mpsc::unbounded_channel();
            let starting = tokio::spawn(start_websocket_transport_with_hook(
                admission,
                limits(),
                Duration::from_secs(1),
                |_: &io::Error| false,
                move |_| future::ready(halves(&factory_state)),
                move |current| {
                    let reached = reached.clone();
                    let gate = Arc::clone(&hook_gate);
                    async move {
                        if current == stage {
                            reached.send(()).expect("stage receiver");
                            gate.acquire().await.expect("gate").forget();
                        }
                    }
                },
            ));
            timeout(TIMEOUT, reached_rx.recv())
                .await
                .expect("stage timeout")
                .expect("stage");
            let observer = inventory.observe(&exact).expect("active observer");
            assert!(observer.local_io_relinquishment().is_for(&exact));
            if revoke_lifecycle {
                lifecycle.begin_fenced_recovery();
                lifecycle.serve();
            } else {
                source.rotate(NodeIdentity::new("node", "b")).await;
            }
            spin_until(|| observer.stop_reason().is_some()).await;
            gate.add_permits(1);
            let _ = timeout(TIMEOUT, starting)
                .await
                .expect("starter timeout")
                .expect("starter task");
            let terminal = timeout(TIMEOUT, observer.wait_until_joined())
                .await
                .expect("terminal");
            assert!(matches!(
                terminal,
                CustodianState::Joined
                    | CustodianState::PartialStartJoined
                    | CustodianState::StartupAbandoned
            ));
            assert!(state.reader_dropped.load(Ordering::Acquire));
            assert!(state.writer_dropped.load(Ordering::Acquire));
            assert_eq!(inventory.prune_joined(), 1);
            assert!(inventory.is_empty());
        }
    }
}

#[test]
fn axum_classifier_only_accepts_the_concrete_send_after_close_error() {
    use tungstenite::error::ProtocolError;
    let expected = axum::Error::new(tungstenite::Error::Protocol(
        ProtocolError::SendAfterClosing,
    ));
    let other = axum::Error::new(tungstenite::Error::Protocol(
        ProtocolError::ResetWithoutClosingHandshake,
    ));
    let unrelated = axum::Error::new(io::Error::other("different error"));
    assert!(axum_send_after_close_for_test(&expected));
    assert!(!axum_send_after_close_for_test(&other));
    assert!(!axum_send_after_close_for_test(&unrelated));
}

#[tokio::test]
async fn revoked_axum_upgrade_over_duplex_observes_close_before_splitting() {
    use axum::extract::ws::WebSocketUpgrade;
    use axum::routing::get;
    use axum::Router;
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message as PeerMessage;

    let lifecycle = NodeLifecycle::new();
    let source = SharedNodeIdentity::new(NodeIdentity::new("node", "a"));
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let admission = clustered(&lifecycle, &source, &inventory);
    source.rotate(NodeIdentity::new("node", "b")).await;
    let slot = Arc::new(Mutex::new(Some(admission)));
    let (done, completed) = tokio::sync::oneshot::channel();
    let completion = Arc::new(Mutex::new(Some(done)));
    let app = Router::new().route(
        "/ws",
        get(move |ws: WebSocketUpgrade| {
            let slot = Arc::clone(&slot);
            let completion = Arc::clone(&completion);
            async move {
                ws.on_upgrade(move |socket| async move {
                    let admission = slot.lock().expect("slot").take().expect("admission");
                    let result = admission
                        .start_socket(socket, limits(), Duration::from_secs(1))
                        .await;
                    let rejected = matches!(result, Err(SocketAdmissionError::IdentityRevoked));
                    if let Some(done) = completion.lock().expect("completion").take() {
                        let _ = done.send(rejected);
                    }
                })
            }
        }),
    );
    let (server_io, peer_io) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        axum::serve(DuplexListener(Some(server_io)), app)
            .await
            .expect("serve")
    });
    let (mut peer, response) = tokio_tungstenite::client_async("ws://localhost/ws", peer_io)
        .await
        .expect("upgrade");
    assert_eq!(response.status(), 101);
    assert!(matches!(
        timeout(TIMEOUT, peer.next())
            .await
            .expect("peer close timeout"),
        Some(Ok(PeerMessage::Close(_)))
    ));
    assert!(timeout(TIMEOUT, completed)
        .await
        .expect("server timeout")
        .expect("server result"));
    assert!(inventory.is_empty());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn revocation_during_factory_drops_pending_work_and_releases_reservation() {
    struct DropMarker(Arc<AtomicBool>);
    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    let lifecycle = NodeLifecycle::new();
    let source = SharedNodeIdentity::new(NodeIdentity::new("node", "a"));
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let admission = clustered(&lifecycle, &source, &inventory);
    let dropped = Arc::new(AtomicBool::new(false));
    let marker = DropMarker(Arc::clone(&dropped));
    let (entered, started) = tokio::sync::oneshot::channel();
    let starting = tokio::spawn(start_websocket_transport(
        admission,
        limits(),
        TIMEOUT,
        |_: &io::Error| false,
        move |_| {
            let _ = entered.send(());
            async move {
                let _marker = marker;
                future::pending::<(Reader, Writer)>().await
            }
        },
    ));
    timeout(TIMEOUT, started)
        .await
        .expect("factory timeout")
        .expect("factory entered");
    source.rotate(NodeIdentity::new("node", "b")).await;
    assert!(matches!(
        timeout(TIMEOUT, starting)
            .await
            .expect("start timeout")
            .expect("starter"),
        Err(SocketAdmissionError::Cancelled)
    ));
    assert!(dropped.load(Ordering::Acquire));
    assert!(inventory.is_empty());
}

#[test]
fn dropping_unpolled_start_releases_reservation_without_calling_factory() {
    let lifecycle = NodeLifecycle::new();
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let admission = TestSocketAdmission::standalone(
        &lifecycle,
        &inventory,
        CancellationToken::new(),
        CancellationToken::new(),
    )
    .expect("admission");
    let invoked = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&invoked);
    let state = Arc::new(SinkState::default());
    let pending = start_websocket_transport(
        admission,
        limits(),
        TIMEOUT,
        |_: &io::Error| false,
        move |_| {
            seen.store(true, Ordering::Release);
            future::ready(halves(&state))
        },
    );
    drop(pending);
    assert!(!invoked.load(Ordering::Acquire));
    assert!(inventory.is_empty());
}

#[tokio::test]
async fn lifecycle_recovery_does_not_revive_startup_authority() {
    let lifecycle = NodeLifecycle::new();
    let source = SharedNodeIdentity::new(NodeIdentity::new("node", "a"));
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let admission = clustered(&lifecycle, &source, &inventory);
    lifecycle.begin_fenced_recovery();
    lifecycle.serve();
    let invoked = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&invoked);
    let state = Arc::new(SinkState::default());
    let result = start_websocket_transport(
        admission,
        limits(),
        TIMEOUT,
        |_: &io::Error| false,
        move |_| {
            seen.store(true, Ordering::Release);
            future::ready(halves(&state))
        },
    )
    .await;
    assert!(matches!(result, Err(SocketAdmissionError::Lifecycle(_))));
    assert!(!invoked.load(Ordering::Acquire));
    assert!(inventory.is_empty());
}

#[tokio::test]
async fn axum_upgrade_over_duplex_binds_both_halves_to_the_admitted_reference() {
    use axum::extract::ws::WebSocketUpgrade;
    use axum::routing::get;
    use axum::Router;
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as PeerMessage;

    let lifecycle = NodeLifecycle::new();
    let source = SharedNodeIdentity::new(NodeIdentity::new("node", "a"));
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let admission = clustered(&lifecycle, &source, &inventory);
    let exact = admission.connection_ref().clone();
    let foreign = ConnectionRef::fresh(exact.node_lease().clone());
    let slot = Arc::new(Mutex::new(Some(admission)));
    let (started, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(started)));
    let app = Router::new().route(
        "/ws",
        get(move |ws: WebSocketUpgrade| {
            let slot = Arc::clone(&slot);
            let sender = Arc::clone(&sender);
            async move {
                ws.on_upgrade(move |socket| async move {
                    let admission = slot.lock().expect("slot").take().expect("admission");
                    let result = admission
                        .start_socket(socket, limits(), Duration::from_secs(1))
                        .await;
                    if let Some(started) = sender.lock().expect("sender").take() {
                        let _ = started.send(result);
                    }
                })
            }
        }),
    );
    let (server_io, peer_io) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        axum::serve(DuplexListener(Some(server_io)), app)
            .await
            .expect("serve")
    });
    let (mut peer, response) = tokio_tungstenite::client_async("ws://localhost/ws", peer_io)
        .await
        .expect("upgrade");
    assert_eq!(response.status(), 101);
    let (custodian, mut input, mut output) = timeout(TIMEOUT, receiver)
        .await
        .expect("startup timeout")
        .expect("callback")
        .expect("socket startup");
    assert!(custodian.is_for(&exact));
    let witness = custodian.observer().local_io_relinquishment();
    assert!(witness.is_for(&exact));
    assert!(!witness.is_for(&foreign));

    peer.send(PeerMessage::Text("from-peer".into()))
        .await
        .expect("peer send");
    assert_eq!(
        timeout(TIMEOUT, input.next())
            .await
            .expect("input timeout")
            .expect("input")
            .expect("frame"),
        Message::text("from-peer")
    );
    let frame = output.data_frame(Message::text("to-peer")).expect("frame");
    let receipt = output.submit(frame).await.expect("submit");
    assert_eq!(
        timeout(TIMEOUT, peer.next())
            .await
            .expect("peer timeout")
            .expect("peer frame")
            .expect("peer read"),
        PeerMessage::Text("to-peer".into())
    );
    assert_eq!(receipt.wait_for_terminal().await, WriteProgress::Completed);
    custodian.request_stop(waddle_server::websocket_transport::StopReason::Requested);
    assert_eq!(
        timeout(TIMEOUT, custodian.wait_until_joined())
            .await
            .expect("joined"),
        CustodianState::Joined
    );
    assert_eq!(
        witness.wait().await,
        waddle_xmpp::registry::LocalIoRelinquishmentOutcome::Relinquished
    );
    assert_eq!(inventory.prune_joined(), 1);
    server.abort();
    let _ = server.await;
}
