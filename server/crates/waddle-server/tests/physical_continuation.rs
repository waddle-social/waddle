#![cfg(feature = "test-support")]

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::serve::Listener;
use axum::Router;
use futures::{SinkExt, StreamExt};
use kameo::actor::Spawn;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use waddle_server::clustering::NodeLifecycle;
use waddle_server::ingress::IngressRetirementOutcome;
use waddle_server::test_support::actor_local_endpoint::{
    ActorEndpointCleanupIndex, ActorLocalEndpointError, ActorLocalEndpointOwner,
    ConnectionActorDriverTestGate, ConnectionActorDriverTestHandle,
    ConnectionActorDriverTestSnapshot, ConnectionActorDriverTestUnknownReason, PhysicalEnrollment,
    PhysicalLimits, PhysicalReplayPayload,
};
use waddle_server::test_support::connection_actor::ContinuationPhase;
use waddle_server::test_support::supervised_io::{
    CustodianState, IoCustodianInventory, Limits, StopReason, TransportLimits,
};
use waddle_server::test_support::websocket_state;
use waddle_server::websocket_transport::{
    ActorWriteProbe, TestSocketAdmission, WebSocketInput, WebSocketOutput,
};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef};
use waddle_xmpp::registry::user_actor::UserActor;
use waddle_xmpp::registry::{
    ActorOutboundStage, ConnectionEntry, ConnectionRegistry, LocalConnectionBindingId,
    LocalRetryDispatchOutcome, OutboundStanza, TrySendDirectWithCustody,
};
use waddle_xmpp::stream_management::{
    SmAck, SmIngressAppendKey, SmIngressReceiptKind, SmRelayedAppendObligation, SmRequest,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Lang, Message as XmppMessage};

const DEADLINE: Duration = Duration::from_secs(5);

struct DuplexListener(Option<DuplexStream>);

impl Listener for DuplexListener {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.take() {
            Some(stream) => (stream, ()),
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(())
    }
}

type Started = (
    ActorLocalEndpointOwner,
    WebSocketInput<axum::Error>,
    WebSocketOutput<axum::Error>,
);

struct Route {
    lifecycle: NodeLifecycle,
    inventory: IoCustodianInventory,
    index: ActorEndpointCleanupIndex,
    probe: Option<Arc<ActorWriteProbe>>,
    retirement_deadline: Duration,
    started: mpsc::Sender<(ConnectionRef, Result<Started, ActorLocalEndpointError>)>,
}

async fn route(State(route): State<Arc<Route>>, upgrade: WebSocketUpgrade) -> impl IntoResponse {
    let admission = TestSocketAdmission::standalone(
        &route.lifecycle,
        &route.inventory,
        tokio_util::sync::CancellationToken::new(),
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("socket admission");
    let admission = match &route.probe {
        Some(probe) => admission.with_actor_write_probe(Arc::clone(probe)),
        None => admission,
    };
    let connection = admission.connection_ref().clone();
    upgrade
        .protocols(["xmpp"])
        .on_upgrade(move |socket| async move {
            let limits = TransportLimits::new(
                Limits::new(8, 8192).expect("input limits"),
                Limits::new(2, 8192).expect("output limits"),
            );
            let started = admission
                .start_actor_local_endpoint(
                    socket,
                    limits,
                    route.retirement_deadline,
                    &route.index,
                    2,
                    8192,
                )
                .await;
            let _ = route.started.send((connection, started)).await;
        })
}

struct Fixture {
    owner: ActorLocalEndpointOwner,
    driver: ConnectionActorDriverTestHandle,
    peer: WebSocketStream<DuplexStream>,
    server: tokio::task::JoinHandle<()>,
    inventory: IoCustodianInventory,
    index: ActorEndpointCleanupIndex,
    connection: ConnectionRef,
}

struct UnattachedFixture {
    owner: ActorLocalEndpointOwner,
    input: WebSocketInput<axum::Error>,
    output: WebSocketOutput<axum::Error>,
    peer: WebSocketStream<DuplexStream>,
    server: tokio::task::JoinHandle<()>,
    inventory: IoCustodianInventory,
    index: ActorEndpointCleanupIndex,
    connection: ConnectionRef,
}

async fn fixture(probe: Option<Arc<ActorWriteProbe>>, replay_frames: usize) -> Fixture {
    fixture_with_retirement(probe, replay_frames, DEADLINE).await
}

async fn fixture_with_retirement(
    probe: Option<Arc<ActorWriteProbe>>,
    replay_frames: usize,
    retirement_deadline: Duration,
) -> Fixture {
    let UnattachedFixture {
        owner,
        input,
        output,
        peer,
        server,
        inventory,
        index,
        connection,
    } = unattached_fixture(probe, retirement_deadline).await;
    let state = websocket_state().await;
    let driver = owner
        .attach_physical_driver_for_test(
            Arc::clone(&state),
            tokio_util::sync::CancellationToken::new(),
            input,
            output,
            physical_limits(replay_frames),
        )
        .expect("exact physical driver");
    Fixture {
        owner,
        driver,
        peer,
        server,
        inventory,
        index,
        connection,
    }
}

fn physical_limits(replay_frames: usize) -> PhysicalLimits {
    PhysicalLimits {
        max_frame_bytes: 2048,
        max_replay_frames: replay_frames,
        max_replay_bytes: 2048 * replay_frames,
    }
}

async fn unattached_fixture(
    probe: Option<Arc<ActorWriteProbe>>,
    retirement_deadline: Duration,
) -> UnattachedFixture {
    let inventory = IoCustodianInventory::new(1).expect("pump inventory");
    let index = ActorEndpointCleanupIndex::new(1).expect("cleanup inventory");
    let (started, mut ready) = mpsc::channel(1);
    let route_state = Arc::new(Route {
        lifecycle: NodeLifecycle::new(),
        inventory: inventory.clone(),
        index: index.clone(),
        probe,
        retirement_deadline,
        started,
    });
    let app = Router::new()
        .route("/physical", get(route))
        .with_state(route_state);
    let (server_socket, client_socket) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let _ = axum::serve(DuplexListener(Some(server_socket)), app).await;
    });
    let (peer, _) = tokio_tungstenite::client_async("ws://localhost/physical", client_socket)
        .await
        .expect("real loopback WebSocket");
    let (connection, started) = timeout(DEADLINE, ready.recv())
        .await
        .expect("startup deadline")
        .expect("startup result");
    let (owner, input, output) = started.expect("physical owner");
    UnattachedFixture {
        owner,
        input,
        output,
        peer,
        server,
        inventory,
        index,
        connection,
    }
}

fn stanza() -> Stanza {
    Stanza::Message(XmppMessage::new(None::<jid::Jid>).with_body(Lang::new(), "bounded".to_owned()))
}

async fn read_text(peer: &mut WebSocketStream<DuplexStream>) -> String {
    let frame = timeout(DEADLINE, peer.next())
        .await
        .expect("peer read deadline")
        .expect("peer remains open")
        .expect("valid WebSocket frame");
    let Message::Text(text) = frame else {
        panic!("expected an XMPP text frame");
    };
    text.to_string()
}

async fn wait_snapshot(
    driver: &ConnectionActorDriverTestHandle,
    condition: impl Fn(waddle_server::test_support::actor_local_endpoint::PhysicalSnapshot) -> bool,
) {
    timeout(DEADLINE, async {
        loop {
            if driver.physical_snapshot().is_some_and(&condition) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("physical state deadline");
}

async fn wait_cleanup_joined(fixture: &Fixture) {
    let observer = fixture
        .inventory
        .observe(&fixture.connection)
        .expect("pump observer");
    assert_eq!(
        timeout(DEADLINE, observer.wait_until_joined())
            .await
            .expect("pumps joined"),
        CustodianState::Joined
    );
    timeout(DEADLINE, async {
        while !fixture
            .index
            .join_cleanup_finished_for_test(&fixture.connection)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("driver and endpoint joined");
}

#[tokio::test(flavor = "current_thread")]
async fn never_started_producer_survives_exact_joined_recovery() {
    let fixture = fixture(None, 1).await;
    let gate = ConnectionActorDriverTestGate::default();
    // This task performs admission and stop without yielding. The parked
    // worker has not run on the single-thread executor between those calls.
    let ticket = fixture
        .driver
        .submit_physical_producer(stanza(), 2, 2048, &gate)
        .expect("queued producer");
    let id = ticket.id();
    let credits = ticket.reserved_credits();
    assert_eq!(
        ticket.snapshot(),
        ConnectionActorDriverTestSnapshot::Accepted
    );
    fixture.owner.request_stop(StopReason::Requested);
    wait_cleanup_joined(&fixture).await;
    let recovery = fixture
        .index
        .take_joined_physical_for_test(&fixture.connection)
        .expect("exact recovery");
    assert_eq!(recovery.enrollment, PhysicalEnrollment::NotStarted);
    assert!(recovery.active.is_none());
    assert_eq!(recovery.operations.len(), 1);
    assert_eq!(
        recovery.retire_stream().await.expect("unstarted stream"),
        IngressRetirementOutcome::StreamMissing
    );
    drop(ticket);
    fixture.server.abort();
    drop(fixture);
    let operation = &recovery.operations[0];
    assert_eq!(operation.id(), id);
    assert_eq!(
        operation.snapshot(),
        ConnectionActorDriverTestSnapshot::NotStarted
    );
    assert_eq!(operation.run_count(), 0);
    assert_eq!(operation.reserved_credits(), credits);
    assert_eq!(credits.0, 1);
    assert!(credits.1 >= 2048);
    let (input, count) = operation.physical_producer_input().expect("typed input");
    assert_eq!(input.to_element(), stanza().to_element());
    assert_eq!(count, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborted_active_effect_survives_exact_joined_recovery() {
    let fixture = fixture_with_retirement(None, 1, Duration::from_millis(150)).await;
    let gate = ConnectionActorDriverTestGate::default();
    let ticket = fixture
        .driver
        .submit_physical_producer(stanza(), 2, 2048, &gate)
        .expect("producer admitted");
    let id = ticket.id();
    let credits = ticket.reserved_credits();
    timeout(DEADLINE, gate.wait_until_entered())
        .await
        .expect("effect entered");
    fixture.owner.request_stop(StopReason::Requested);
    wait_cleanup_joined(&fixture).await;
    let recovery = fixture
        .index
        .take_joined_physical_for_test(&fixture.connection)
        .expect("exact recovery");
    assert_eq!(recovery.enrollment, PhysicalEnrollment::Enrolled);
    let active = recovery.active.as_ref().expect("active continuation");
    assert_eq!(active.operation_id, id);
    assert_eq!(active.ordinal, 0);
    assert_eq!(active.phase, ContinuationPhase::Producing);
    assert!(active.stanza().is_none());
    assert_eq!(active.retained_credits, (1, 2048));
    assert_eq!(active.input_stanza().to_element(), stanza().to_element());
    assert_eq!(active.requested_frames(), 2);
    assert_eq!(recovery.operations.len(), 1);
    assert_eq!(
        recovery.retire_stream().await.expect("enrolled stream"),
        IngressRetirementOutcome::Deleted
    );
    drop(ticket);
    fixture.server.abort();
    drop(fixture);
    let operation = &recovery.operations[0];
    assert_eq!(operation.id(), id);
    assert_eq!(
        operation.snapshot(),
        ConnectionActorDriverTestSnapshot::CompletionUnknown(
            ConnectionActorDriverTestUnknownReason::WorkerAborted
        )
    );
    assert_eq!(operation.run_count(), 1);
    assert_eq!(operation.reserved_credits(), credits);
    let (input, count) = operation.physical_producer_input().expect("typed input");
    assert_eq!(input.to_element(), stanza().to_element());
    assert_eq!(count, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_waiter_control_checkpoint_and_full_replay_use_one_writer() {
    let mut fixture = fixture(None, 1).await;
    let gate = ConnectionActorDriverTestGate::default();
    let ticket = fixture
        .driver
        .submit_physical_producer(stanza(), 2, 2048, &gate)
        .expect("bounded producer admitted");
    let id = ticket.id();
    timeout(DEADLINE, gate.wait_until_entered())
        .await
        .expect("effect started");
    let waiting_ticket = ticket.clone();
    let waiter = tokio::spawn(async move { waiting_ticket.wait().await });
    waiter.abort();
    let _ = waiter.await;
    fixture
        .peer
        .send(Message::Text(SmRequest::to_xml().into()))
        .await
        .expect("request h0");
    let ack = SmAck::parse(&read_text(&mut fixture.peer).await).expect("SM acknowledgment");
    assert_eq!(ack.h, 0);
    gate.release();
    let first = read_text(&mut fixture.peer).await;
    assert_eq!(
        first
            .parse::<xmpp_parsers::minidom::Element>()
            .expect("XML"),
        stanza().to_element()
    );
    wait_snapshot(&fixture.driver, |state| {
        state.flushed == 1 && state.replay_frames == 1
    })
    .await;
    fixture
        .peer
        .send(Message::Text(SmRequest::to_xml().into()))
        .await
        .expect("request h2");
    let ack = SmAck::parse(&read_text(&mut fixture.peer).await).expect("SM acknowledgment");
    assert_eq!(ack.h, 2);
    assert_eq!(
        fixture
            .driver
            .physical_snapshot()
            .expect("snapshot")
            .checkpoint_h,
        Some(2)
    );
    fixture
        .peer
        .send(Message::Text(SmAck::new(1).to_xml().into()))
        .await
        .expect("peer ACK");
    let second = read_text(&mut fixture.peer).await;
    assert_eq!(
        second
            .parse::<xmpp_parsers::minidom::Element>()
            .expect("XML"),
        stanza().to_element()
    );
    let recovered = fixture
        .driver
        .lookup(&fixture.connection, id)
        .expect("exact ticket retained");
    assert_eq!(
        timeout(DEADLINE, recovered.wait())
            .await
            .expect("producer result"),
        ConnectionActorDriverTestSnapshot::Completed
    );
    assert_eq!(recovered.run_count(), 1);
    assert!(!recovered.acknowledge());
    assert!(fixture
        .index
        .take_joined_physical_for_test(&fixture.connection)
        .is_none());
    fixture.owner.request_stop(StopReason::Requested);
    let observer = fixture
        .inventory
        .observe(&fixture.connection)
        .expect("pump observer");
    assert_eq!(
        timeout(DEADLINE, observer.wait_until_joined())
            .await
            .expect("pump join"),
        CustodianState::Joined
    );
    timeout(DEADLINE, async {
        while !fixture
            .index
            .join_cleanup_finished_for_test(&fixture.connection)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("joined cleanup");
    let successor = ConnectionRef::fresh(fixture.connection.node_lease().clone());
    assert!(fixture
        .index
        .take_joined_physical_for_test(&successor)
        .is_none());
    let recovery = fixture
        .index
        .take_joined_physical_for_test(&fixture.connection)
        .expect("exact joined recovery");
    assert_eq!(recovery.replay.len(), 1);
    assert!(matches!(
        &recovery.replay[0].payload,
        PhysicalReplayPayload::Producer { .. }
    ));
    recovery
        .retire_stream()
        .await
        .expect("retire fixture stream");
    fixture.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_actor_channel_rejects_physical_attach_and_keeps_terminal_custody() {
    use waddle_xmpp::registry::ActorOutboundStage;

    let UnattachedFixture {
        mut owner,
        input,
        output,
        peer: _peer,
        server,
        inventory,
        index,
        connection,
    } = unattached_fixture(None, DEADLINE).await;
    let (endpoint, mut receiver) = owner.new_binding_channel().expect("direct channel");
    let jid: jid::FullJid = "alice@example.test/phone".parse().expect("full JID");
    let candidate = ConnectionEntry::new_actor_provisional_for_binding(
        endpoint,
        BindingRef::fresh(connection.clone()),
        jid.clone(),
    )
    .expect("exact binding");
    let custody = candidate.actor_custody().expect("candidate custody");
    let registry = Arc::new(ConnectionRegistry::new());
    let actor = UserActor::spawn(UserActor::new(jid.to_bare()));
    owner
        .publish_binding(
            Arc::clone(&registry),
            actor.clone(),
            jid.clone(),
            candidate,
            None,
            DEADLINE,
        )
        .await
        .expect("publish direct endpoint");
    let obligation = SmRelayedAppendObligation {
        key: SmIngressAppendKey {
            message_key: waddle_xmpp::ingress::MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(1),
            semantic_identity_hash: [7; 32],
            resource: jid.clone(),
        },
        sender_bare: "sender@example.test".parse().expect("sender"),
        received_at: None,
        archive_positions: Vec::new(),
        dispatch_stream: None,
    };
    let mut outbound = OutboundStanza::new(stanza()).with_ingress_append(obligation.clone());
    outbound.pending_row_id = Some(waddle_xmpp::pending_delivery::PendingRowId::new(
        "physical-pending-row",
    ));
    assert!(matches!(
        actor
            .ask(TrySendDirectWithCustody { jid, outbound })
            .await
            .expect("dispatch"),
        LocalRetryDispatchOutcome::AcceptedOrSettled
    ));
    let id = timeout(DEADLINE, receiver.recv())
        .await
        .expect("direct queue deadline")
        .expect("queued direct ID");
    let original = custody.snapshot(&id).expect("retained direct envelope");
    assert!(matches!(
        owner.attach_physical_driver_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
            input,
            output,
            physical_limits(1),
        ),
        Err(ActorLocalEndpointError::PriorBindingActive)
    ));
    assert!(Arc::ptr_eq(
        &original,
        &custody
            .snapshot(&id)
            .expect("custody survived rejected attach")
    ));
    owner.request_stop(StopReason::Requested);
    let observer = inventory.observe(&connection).expect("pump observer");
    assert_eq!(
        timeout(DEADLINE, observer.wait_until_joined())
            .await
            .expect("pump join"),
        CustodianState::Joined
    );
    timeout(DEADLINE, async {
        while !index.join_cleanup_finished_for_test(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("joined cleanup");
    let (recovered, stage) = index
        .take_joined_terminal(&id)
        .expect("one terminal transfer");
    assert_eq!(stage, ActorOutboundStage::NotAttempted);
    assert!(Arc::ptr_eq(&original, &recovered));
    assert_eq!(
        recovered.outbound().ingress_append.as_ref(),
        Some(&obligation)
    );
    assert!(index.take_joined_terminal(&id).is_none());
    actor.kill();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn binding_replacement_waits_for_terminal_snapshot_lease_and_outlives_owner() {
    let UnattachedFixture {
        mut owner,
        input: _input,
        output: _output,
        peer,
        server,
        inventory,
        index,
        connection,
    } = unattached_fixture(None, DEADLINE).await;

    let (endpoint, receiver) = owner.new_binding_channel().expect("first channel");
    let custody = receiver.custody().clone();
    let binding = BindingRef::fresh(connection.clone());
    let id = custody
        .insert_for_test(
            LocalConnectionBindingId::fresh(),
            &binding,
            OutboundStanza::new(stanza()),
        )
        .expect("test frame retained by custody");
    let snapshot = custody.snapshot(&id).expect("snapshot");
    assert_eq!(custody.attempt_with(&id, || ()), Some(()));
    custody.mark_flushed(&id);
    custody.seal();
    let (terminal, stage) = custody.take_terminal(&id).expect("terminal transfer");
    assert_eq!(stage, ActorOutboundStage::Flushed);
    assert_eq!(custody.retained_count(), 0);
    assert_eq!(custody.outstanding_lease_count(), 1);

    assert!(matches!(
        owner.new_binding_channel(),
        Err(ActorLocalEndpointError::PriorBindingActive)
    ));
    drop(terminal);
    assert_eq!(custody.outstanding_lease_count(), 1);
    assert!(matches!(
        owner.new_binding_channel(),
        Err(ActorLocalEndpointError::PriorBindingActive)
    ));
    drop(snapshot);
    assert_eq!(custody.outstanding_lease_count(), 0);
    drop(endpoint);
    drop(receiver);
    drop(custody);

    let (endpoint, receiver) = owner
        .new_binding_channel()
        .expect("replacement after final lease drop");
    let custody = receiver.custody().clone();
    let id = custody
        .insert_for_test(
            LocalConnectionBindingId::fresh(),
            &BindingRef::fresh(connection.clone()),
            OutboundStanza::new(stanza()),
        )
        .expect("second generation frame retained");
    let snapshot = custody.snapshot(&id).expect("second snapshot");
    assert_eq!(custody.attempt_with(&id, || ()), Some(()));
    custody.mark_flushed(&id);
    custody.seal();
    let (terminal, _) = custody.take_terminal(&id).expect("second terminal");
    drop(endpoint);
    drop(receiver);

    owner.request_stop(StopReason::Requested);
    let observer = inventory.observe(&connection).expect("pump observer");
    assert_eq!(
        timeout(DEADLINE, observer.wait_until_joined())
            .await
            .expect("pump join"),
        CustodianState::Joined
    );
    timeout(DEADLINE, async {
        while index.contains(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cleanup index release");
    server.abort();

    drop(owner);
    drop(index);
    drop(inventory);
    drop(peer);
    assert_eq!(custody.retained_count(), 0);
    assert_eq!(custody.outstanding_lease_count(), 1);
    drop(terminal);
    assert_eq!(custody.outstanding_lease_count(), 1);
    drop(snapshot);
    assert_eq!(custody.outstanding_lease_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn physical_attach_excludes_new_actor_channel_and_repeated_attach() {
    let mut fixture = fixture(None, 1).await;
    assert!(matches!(
        fixture.owner.new_binding_channel(),
        Err(ActorLocalEndpointError::PriorBindingActive)
    ));
    assert!(matches!(
        fixture.owner.attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        ),
        Err(ActorLocalEndpointError::DriverAlreadyAttached)
    ));
    wait_snapshot(&fixture.driver, |state| {
        state.enrollment == PhysicalEnrollment::Enrolled
    })
    .await;
    fixture.owner.request_stop(StopReason::Requested);
    assert!(matches!(
        fixture.owner.attach_driver_task_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
        ),
        Err(ActorLocalEndpointError::DriverCleanupStarted)
    ));
    wait_cleanup_joined(&fixture).await;
    let recovery = fixture
        .index
        .take_joined_physical_for_test(&fixture.connection)
        .expect("exact joined recovery");
    recovery
        .retire_stream()
        .await
        .expect("retire fixture stream");
    fixture.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn physical_attach_rejects_another_connections_input_and_output() {
    let UnattachedFixture {
        owner,
        input,
        output,
        peer: _peer,
        server,
        inventory,
        index,
        connection,
    } = unattached_fixture(None, DEADLINE).await;
    let UnattachedFixture {
        owner: other_owner,
        input: other_input,
        output: other_output,
        peer: _other_peer,
        server: other_server,
        inventory: other_inventory,
        index: _other_index,
        connection: other_connection,
    } = unattached_fixture(None, DEADLINE).await;
    assert_ne!(connection, other_connection);
    assert!(matches!(
        owner.attach_physical_driver_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
            other_input,
            other_output,
            physical_limits(1),
        ),
        Err(ActorLocalEndpointError::DriverConnectionMismatch)
    ));
    let driver = owner
        .attach_physical_driver_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
            input,
            output,
            physical_limits(1),
        )
        .expect("correct owner still attaches");
    assert_eq!(driver.connection_ref(), &connection);
    wait_snapshot(&driver, |state| {
        state.enrollment == PhysicalEnrollment::Enrolled
    })
    .await;
    owner.request_stop(StopReason::Requested);
    let observer = inventory.observe(&connection).expect("pump observer");
    assert_eq!(
        timeout(DEADLINE, observer.wait_until_joined())
            .await
            .expect("join"),
        CustodianState::Joined
    );
    timeout(DEADLINE, async {
        while !index.join_cleanup_finished_for_test(&connection) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("joined cleanup");
    let recovery = index
        .take_joined_physical_for_test(&connection)
        .expect("exact recovery");
    recovery
        .retire_stream()
        .await
        .expect("retire fixture stream");
    other_owner.request_stop(StopReason::Requested);
    let other_observer = other_inventory
        .observe(&other_connection)
        .expect("other observer");
    assert_eq!(
        timeout(DEADLINE, other_observer.wait_until_joined())
            .await
            .expect("other join"),
        CustodianState::Joined
    );
    server.abort();
    other_server.abort();
}

async fn rejects_mixed_physical_halves(wrong_input: bool) {
    let UnattachedFixture {
        owner,
        input,
        output,
        peer: _peer,
        server,
        inventory,
        index: _index,
        connection,
    } = unattached_fixture(None, DEADLINE).await;
    let UnattachedFixture {
        owner: other_owner,
        input: other_input,
        output: other_output,
        peer: _other_peer,
        server: other_server,
        inventory: other_inventory,
        index: _other_index,
        connection: other_connection,
    } = unattached_fixture(None, DEADLINE).await;
    let (mut input, mut output) = (Some(input), Some(output));
    let (mut other_input, mut other_output) = (Some(other_input), Some(other_output));
    let attempted_input = if wrong_input {
        other_input.take().expect("other input")
    } else {
        input.take().expect("matching input")
    };
    let attempted_output = if wrong_input {
        output.take().expect("matching output")
    } else {
        other_output.take().expect("other output")
    };
    assert!(matches!(
        owner.attach_physical_driver_for_test(
            websocket_state().await,
            tokio_util::sync::CancellationToken::new(),
            attempted_input,
            attempted_output,
            physical_limits(1),
        ),
        Err(ActorLocalEndpointError::DriverConnectionMismatch)
    ));
    drop((input, output, other_input, other_output));
    owner.request_stop(StopReason::Requested);
    other_owner.request_stop(StopReason::Requested);
    let observer = inventory.observe(&connection).expect("first observer");
    let other_observer = other_inventory
        .observe(&other_connection)
        .expect("second observer");
    assert_eq!(
        timeout(DEADLINE, observer.wait_until_joined())
            .await
            .expect("first join"),
        CustodianState::Joined
    );
    assert_eq!(
        timeout(DEADLINE, other_observer.wait_until_joined())
            .await
            .expect("second join"),
        CustodianState::Joined
    );
    server.abort();
    other_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn physical_attach_rejects_wrong_input_with_correct_output() {
    rejects_mixed_physical_halves(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn physical_attach_rejects_wrong_output_with_correct_input() {
    rejects_mixed_physical_halves(false).await;
}

struct ReleaseStartProbe(Arc<ActorWriteProbe>);

impl Drop for ReleaseStartProbe {
    fn drop(&mut self) {
        self.0.release_after_start_send();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_during_start_send_keeps_driver_deadline_and_exact_join_custody() {
    use waddle_server::test_support::supervised_io::WriteProgress;

    let probe = Arc::new(ActorWriteProbe::default());
    probe.pause_after_start_send();
    let _release_on_failure = ReleaseStartProbe(Arc::clone(&probe));
    let mut fixture =
        fixture_with_retirement(Some(Arc::clone(&probe)), 1, Duration::from_millis(250)).await;
    let gate = ConnectionActorDriverTestGate::default();
    let ticket = fixture
        .driver
        .submit_physical_producer(stanza(), 1, 2048, &gate)
        .expect("producer admitted");
    timeout(DEADLINE, gate.wait_until_entered())
        .await
        .expect("effect entered");
    gate.release();
    timeout(DEADLINE, probe.wait_for_after_start_send())
        .await
        .expect("physical enqueue paused");
    fixture
        .peer
        .send(Message::Text(SmRequest::to_xml().into()))
        .await
        .expect("SM request");
    wait_snapshot(&fixture.driver, |state| state.checkpoint_h == Some(2)).await;
    assert!(
        fixture
            .driver
            .physical_snapshot()
            .expect("snapshot")
            .control_pending
    );

    fixture.driver.request_stop(StopReason::LeaseLost);
    wait_snapshot(&fixture.driver, |state| state.stop_forwarded).await;
    timeout(DEADLINE, fixture.driver.wait_joined())
        .await
        .expect("driver abort deadline");
    assert!(matches!(
        ticket.snapshot(),
        ConnectionActorDriverTestSnapshot::CompletionUnknown(_)
    ));
    let observer = fixture
        .inventory
        .observe(&fixture.connection)
        .expect("pump observer");
    timeout(DEADLINE, async {
        while observer.state() != CustodianState::CleanupPending {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reaper deadline");
    assert!(fixture
        .index
        .take_joined_physical_for_test(&fixture.connection)
        .is_none());

    probe.release_after_start_send();
    assert_eq!(
        timeout(DEADLINE, observer.wait_until_joined())
            .await
            .expect("pump and stop join"),
        CustodianState::Joined
    );
    timeout(DEADLINE, async {
        while !fixture
            .index
            .join_cleanup_finished_for_test(&fixture.connection)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("joined cleanup");
    let successor = ConnectionRef::fresh(fixture.connection.node_lease().clone());
    assert!(fixture
        .index
        .take_joined_physical_for_test(&successor)
        .is_none());
    let recovery = fixture
        .index
        .take_joined_physical_for_test(&fixture.connection)
        .expect("exact recovery");
    let receipt = recovery
        .current
        .as_ref()
        .and_then(|write| write.receipt.as_ref())
        .expect("retained receipt");
    assert!(matches!(
        receipt.progress(),
        WriteProgress::Completed | WriteProgress::Uncertain
    ));
    recovery
        .retire_stream()
        .await
        .expect("retire fixture stream");
    fixture.server.abort();
}

struct ReleaseAttemptProbe(Arc<ActorWriteProbe>);

impl Drop for ReleaseAttemptProbe {
    fn drop(&mut self) {
        self.0.release_after_attempt();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborted_post_attempt_write_retains_uncertain_receipt() {
    use waddle_server::test_support::supervised_io::WriteProgress;

    let probe = Arc::new(ActorWriteProbe::default());
    probe.pause_after_attempt();
    let _release_on_failure = ReleaseAttemptProbe(Arc::clone(&probe));
    let fixture =
        fixture_with_retirement(Some(Arc::clone(&probe)), 1, Duration::from_millis(150)).await;
    let gate = ConnectionActorDriverTestGate::default();
    let ticket = fixture
        .driver
        .submit_physical_producer(stanza(), 1, 2048, &gate)
        .expect("producer admitted");
    timeout(DEADLINE, gate.wait_until_entered())
        .await
        .expect("effect entered");
    gate.release();
    timeout(DEADLINE, probe.wait_for_after_attempt())
        .await
        .expect("writer attempted before flush");
    wait_snapshot(&fixture.driver, |state| {
        state.progress == Some(WriteProgress::Attempted)
    })
    .await;
    fixture.driver.request_stop(StopReason::Requested);
    wait_snapshot(&fixture.driver, |state| state.stop_forwarded).await;
    wait_cleanup_joined(&fixture).await;
    assert_eq!(ticket.run_count(), 1);
    let recovery = fixture
        .index
        .take_joined_physical_for_test(&fixture.connection)
        .expect("exact joined recovery");
    let receipt = recovery
        .current
        .as_ref()
        .and_then(|write| write.receipt.as_ref())
        .expect("retained receipt");
    assert!(receipt.is_for(&fixture.connection));
    assert_eq!(receipt.progress(), WriteProgress::Uncertain);
    assert_eq!(recovery.replay.len(), 1);
    assert_eq!(
        recovery.replay[0]
            .receipt
            .as_ref()
            .expect("replay receipt")
            .progress(),
        WriteProgress::Uncertain
    );
    recovery
        .retire_stream()
        .await
        .expect("retire fixture stream");
    fixture.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_ack_during_start_send_waits_for_the_actual_receipt() {
    let probe = Arc::new(ActorWriteProbe::default());
    probe.pause_after_start_send();
    let _release_on_failure = ReleaseStartProbe(Arc::clone(&probe));
    let mut fixture = fixture(Some(Arc::clone(&probe)), 1).await;
    let gate = ConnectionActorDriverTestGate::default();
    let ticket = fixture
        .driver
        .submit_physical_producer(stanza(), 1, 2048, &gate)
        .expect("producer admitted");
    timeout(DEADLINE, gate.wait_until_entered())
        .await
        .expect("effect entered");
    gate.release();
    timeout(DEADLINE, probe.wait_for_after_start_send())
        .await
        .expect("sink returned before receipt attempt milestone");
    wait_snapshot(&fixture.driver, |state| {
        state.progress == Some(waddle_server::test_support::supervised_io::WriteProgress::Admitted)
    })
    .await;
    fixture
        .peer
        .send(Message::Text(SmAck::new(1).to_xml().into()))
        .await
        .expect("fast ACK");
    wait_snapshot(&fixture.driver, |state| state.control_pending).await;
    let paused = fixture.driver.physical_snapshot().expect("snapshot");
    assert_eq!(paused.peer_acked, 0);
    assert_eq!(paused.attempted, 0);
    probe.release_after_start_send();
    let _ = read_text(&mut fixture.peer).await;
    wait_snapshot(&fixture.driver, |state| state.peer_acked == 1).await;
    assert_eq!(
        timeout(DEADLINE, ticket.wait()).await.expect("operation"),
        ConnectionActorDriverTestSnapshot::Completed
    );
    fixture.owner.request_stop(StopReason::Requested);
    let observer = fixture
        .inventory
        .observe(&fixture.connection)
        .expect("observer");
    assert_eq!(
        timeout(DEADLINE, observer.wait_until_joined())
            .await
            .expect("pump join"),
        CustodianState::Joined
    );
    timeout(DEADLINE, async {
        while !fixture
            .index
            .join_cleanup_finished_for_test(&fixture.connection)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("joined cleanup");
    let recovery = fixture
        .index
        .take_joined_physical_for_test(&fixture.connection)
        .expect("recovery");
    assert!(recovery.replay.is_empty());
    recovery
        .retire_stream()
        .await
        .expect("retire fixture stream");
    fixture.server.abort();
}
