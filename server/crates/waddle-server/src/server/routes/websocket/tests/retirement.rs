//! The retirement protocol of a hosted connection: one typed cause, retired
//! once, fenced on the connection generation, and a same-node transfer that
//! is acknowledged only after the old pumps were seen to stop.

use super::super::connection::{
    fence_unjoined_transfer, finish_connection, observe_pump_termination,
    ConnectionShutdownChannels, FrameAuthority, PumpTermination, RetirementCause, RetirementEntry,
    RetirementFence, RetirementRefusal, TRANSFER_JOIN_DEADLINE,
};
use super::super::registration::{register_bound_connection_after_frame, RegistrationAfterFrame};
use super::super::send::{InterruptedWrite, InterruptibleSink};
use super::{
    create_test_session, create_test_websocket_state, mpsc, Arc, ConnectionPhase, FullJid,
    OutboundStanza, WsConnState,
};
use crate::clustering::{CriticalNodeFailure, NodeLifecycle};
use crate::websocket_transport::StopReason;
use axum::extract::ws::Message;
use futures::SinkExt;
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::registry::{
    ForceDetachOrigin, ForceDetachRequest, LocalSocketIo, SocketCleanupState,
    SocketWriteInterruption,
};
use waddle_xmpp::telemetry::attributes::ActorRetirementCause;

const EVERY_CAUSE: [RetirementCause; 14] = [
    RetirementCause::PeerClose,
    RetirementCause::StreamClosed,
    RetirementCause::Shutdown,
    RetirementCause::IdentityRevoked,
    RetirementCause::PermitRevoked,
    RetirementCause::ForceDetach,
    RetirementCause::Replaced,
    RetirementCause::WriteFailure,
    RetirementCause::PumpExit,
    RetirementCause::LeaseLost,
    RetirementCause::OwnerCancelled,
    RetirementCause::PeerUnresponsive,
    RetirementCause::DispatchTimeout,
    RetirementCause::OwnerPanicked,
];

fn generation() -> ConnectionRef {
    ConnectionRef::fresh(NodeLeaseIdentity::local_process())
}

#[test]
fn each_cause_retires_exactly_once_with_its_own_outcome() {
    for cause in EVERY_CAUSE {
        let connection = generation();
        let fence = RetirementFence::new(connection.clone());
        assert_eq!(fence.cause(), None);
        assert_eq!(fence.admit(&connection), Ok(()));

        assert_eq!(fence.retire(cause), RetirementEntry::First(cause));
        assert_eq!(fence.cause(), Some(cause));
        assert_eq!(
            fence.admit(&connection),
            Err(RetirementRefusal::Retired(cause))
        );

        // Only a displaced binding hands its full JID to a successor here.
        assert_eq!(
            cause.is_transfer(),
            matches!(
                cause,
                RetirementCause::ForceDetach | RetirementCause::Replaced
            )
        );
        let expected_metric = match cause {
            RetirementCause::PeerClose | RetirementCause::StreamClosed => {
                ActorRetirementCause::ClientClose
            }
            RetirementCause::Shutdown
            | RetirementCause::IdentityRevoked
            | RetirementCause::PermitRevoked
            | RetirementCause::LeaseLost => ActorRetirementCause::ServerShutdown,
            RetirementCause::ForceDetach | RetirementCause::Replaced => {
                ActorRetirementCause::Replaced
            }
            RetirementCause::WriteFailure | RetirementCause::PumpExit => {
                ActorRetirementCause::TransportError
            }
            RetirementCause::OwnerCancelled => ActorRetirementCause::Abandoned,
            RetirementCause::PeerUnresponsive | RetirementCause::DispatchTimeout => {
                ActorRetirementCause::Deadline
            }
            RetirementCause::OwnerPanicked => ActorRetirementCause::Panic,
        };
        assert_eq!(cause.metric(), expected_metric);
    }
}

#[test]
fn a_second_retirement_is_a_no_op_that_reports_the_first_cause() {
    for first in EVERY_CAUSE {
        for second in EVERY_CAUSE {
            let fence = RetirementFence::new(generation());
            assert_eq!(fence.retire(first), RetirementEntry::First(first));
            assert_eq!(fence.retire(second), RetirementEntry::Already(first));
            assert_eq!(fence.cause(), Some(first));
        }
    }
}

#[test]
fn every_pump_stop_reason_names_a_cause() {
    for (reason, cause) in [
        (StopReason::PeerClosed, RetirementCause::PeerClose),
        (StopReason::OutboundIoFailure, RetirementCause::WriteFailure),
        (StopReason::LeaseLost, RetirementCause::LeaseLost),
        (StopReason::Requested, RetirementCause::PumpExit),
        (StopReason::InboundOverload, RetirementCause::PumpExit),
        (StopReason::OutboundOverload, RetirementCause::PumpExit),
        (StopReason::InboundQueueClosed, RetirementCause::PumpExit),
        (StopReason::OutboundQueueClosed, RetirementCause::PumpExit),
        (StopReason::InboundIoFailure, RetirementCause::PumpExit),
        (StopReason::PumpExited, RetirementCause::PumpExit),
        (StopReason::ShutdownDeadline, RetirementCause::PumpExit),
    ] {
        assert_eq!(RetirementCause::from_pump_stop(reason), cause);
    }
}

#[test]
fn a_late_effect_for_a_retired_or_foreign_generation_is_refused() {
    let connection = generation();
    let fence = RetirementFence::new(connection.clone());
    // A recovered client is a new physical connection: its reference never
    // matches the fence of the connection it replaces.
    let recovered = generation();
    assert_eq!(
        fence.admit(&recovered),
        Err(RetirementRefusal::WrongGeneration)
    );
    fence.retire(RetirementCause::LeaseLost);
    assert_eq!(
        fence.admit(&connection),
        Err(RetirementRefusal::Retired(RetirementCause::LeaseLost))
    );
    assert_eq!(
        fence.admit(&recovered),
        Err(RetirementRefusal::WrongGeneration)
    );
}

#[tokio::test]
async fn a_retired_connection_refuses_ordinary_writes_but_not_its_own_close() {
    let connection = generation();
    let fence = Arc::new(RetirementFence::new(connection.clone()));
    let (frames, mut written) = futures::channel::mpsc::unbounded::<Message>();
    let mut sink = InterruptibleSink::new(frames, SocketWriteInterruption::default())
        .with_retirement(fence.clone(), connection);

    sink.send(Message::Text("before".into()))
        .await
        .expect("a live connection writes");
    fence.retire(RetirementCause::Shutdown);

    let refused = sink.send(Message::Text("late".into())).await;
    assert!(matches!(
        refused,
        Err(InterruptedWrite::Retired(RetirementRefusal::Retired(
            RetirementCause::Shutdown
        )))
    ));

    // The retirement's own close goes through, and the fence is back in
    // force as soon as that borrow ends.
    sink.retirement_close()
        .send(Message::Close(None))
        .await
        .expect("the retirement writes its close");
    assert!(matches!(
        sink.send(Message::Text("later".into())).await,
        Err(InterruptedWrite::Retired(_))
    ));

    drop(sink);
    let mut seen = Vec::new();
    while let Ok(frame) = written.try_recv() {
        seen.push(frame);
    }
    assert_eq!(
        seen,
        vec![Message::Text("before".into()), Message::Close(None)],
        "nothing written after retirement except its close"
    );
}

#[tokio::test(start_paused = true)]
async fn pump_termination_is_observed_only_when_both_halves_are_dropped_in_time() {
    let deadline = || tokio::time::Instant::now() + TRANSFER_JOIN_DEADLINE;

    assert_eq!(
        observe_pump_termination(None, deadline()).await,
        PumpTermination::NotApplicable
    );

    let (halves, witness) = LocalSocketIo::new((), ());
    drop(halves);
    assert_eq!(
        observe_pump_termination(Some(&witness), deadline()).await,
        PumpTermination::Observed
    );

    let (held, witness) = LocalSocketIo::new((), ());
    let started = tokio::time::Instant::now();
    assert_eq!(
        observe_pump_termination(Some(&witness), deadline()).await,
        PumpTermination::Missed
    );
    assert_eq!(started.elapsed(), TRANSFER_JOIN_DEADLINE);
    drop(held);
}

#[test]
fn an_unjoined_transfer_takes_the_terminal_path_and_cannot_be_recovered() {
    let lifecycle = NodeLifecycle::new();
    let stop = CancellationToken::new();
    let admitted = lifecycle.admit().expect("a serving node admits");

    fence_unjoined_transfer(&lifecycle, &stop);

    assert_eq!(
        lifecycle.critical_failure(),
        Some(CriticalNodeFailure::WebSocketPumpUnjoined)
    );
    assert!(stop.is_cancelled());
    assert!(lifecycle.fatal_fence_token().is_cancelled());
    assert!(
        admitted.revalidate().is_err(),
        "the generation that could still be written is revoked"
    );
    // The recoverable fence and a later promotion both leave the node failed.
    lifecycle.begin_fenced_recovery();
    lifecycle.serve();
    assert!(lifecycle.admit().is_err());
    assert_eq!(
        lifecycle.critical_failure(),
        Some(CriticalNodeFailure::WebSocketPumpUnjoined)
    );
}

struct Transfer {
    state: Arc<super::WebSocketState>,
    conn: WsConnState,
    outbound_rx: mpsc::Receiver<OutboundStanza>,
    lifecycle: Arc<waddle_xmpp::registry::SocketLifecycle>,
    force_detach_rx: Option<mpsc::Receiver<ForceDetachRequest>>,
    ack: Option<oneshot::Receiver<waddle_xmpp::registry::ForceDetachOutcome>>,
}

/// A bound, registered connection, optionally with a fresh-bind replacement
/// already asking for its full JID.
async fn bound_connection(resource: &str, replacement_waiting: bool) -> Transfer {
    let state = create_test_websocket_state().await;
    let jid: FullJid = format!("alice@example.com/{resource}")
        .parse()
        .expect("JID");
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(create_test_session(&state, "alice").await);
    let (tx, outbound_rx) = mpsc::channel::<OutboundStanza>(4);
    let mut pending_tx = Some(tx);
    let registered = register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;
    assert!(matches!(registered, RegistrationAfterFrame::Registered(_)));
    let lifecycle = conn.socket_lifecycle.clone().expect("socket lifecycle");
    let entry = lifecycle.entry.clone();
    let force_tx = entry.force_detach_sender();
    let force_detach_rx = Some(entry.take_force_detach_rx().expect("control receiver"));
    let ack = if replacement_waiting {
        let (ack, ack_rx) = oneshot::channel();
        force_tx
            .send(ForceDetachRequest {
                origin: ForceDetachOrigin::FreshBindReplacement,
                target_binding: None,
                requester_bare_jid: jid.to_bare(),
                ack,
            })
            .await
            .expect("queue the replacement's request");
        Some(ack_rx)
    } else {
        None
    };
    Transfer {
        state,
        conn,
        outbound_rx,
        lifecycle,
        force_detach_rx,
        ack,
    }
}

async fn settle(transfer: &mut Transfer, cause: RetirementCause) {
    let sink = InterruptibleSink::new(
        futures::sink::drain(),
        transfer.conn.write_interruption.clone(),
    );
    let (socket_io, _owner_halves) = LocalSocketIo::new(sink, ());
    let permit = transfer
        .state
        .deps
        .app_state
        .node_lifecycle
        .admit()
        .expect("permit");
    let shutdown = CancellationToken::new();
    let (_handoff_tx, mut handoff_rx) =
        crate::server::routes::interpret::ordered_relay_handoff_channel();
    let mut pending_force_detach = Vec::new();
    timeout(
        Duration::from_secs(30),
        finish_connection(
            &transfer.state,
            &mut transfer.conn,
            socket_io,
            FrameAuthority {
                permit: &permit,
                shutdown: &shutdown,
            },
            cause,
            false,
            ConnectionShutdownChannels {
                outbound_rx: &mut transfer.outbound_rx,
                handoff_rx: &mut handoff_rx,
                force_detach_rx: &mut transfer.force_detach_rx,
                pending_force_detach: &mut pending_force_detach,
            },
        ),
    )
    .await
    .expect("settlement is bounded");
}

#[tokio::test]
async fn a_same_node_transfer_is_acknowledged_once_the_pumps_have_stopped() {
    let mut transfer = bound_connection("transfer-joined", true).await;
    let (pump_halves, witness) = LocalSocketIo::new((), ());
    transfer.conn.local_io_relinquishment = Some(witness);
    // The pumps stop while settlement is still cleaning up.
    let stopping = tokio::spawn(async move {
        tokio::time::sleep(TRANSFER_JOIN_DEADLINE / 4).await;
        drop(pump_halves);
    });

    settle(&mut transfer, RetirementCause::ForceDetach).await;
    stopping.await.expect("pumps stopped");

    let node = &transfer.state.deps.app_state.node_lifecycle;
    assert_eq!(node.critical_failure(), None);
    assert_ne!(transfer.lifecycle.state(), SocketCleanupState::Running);
    assert!(
        transfer.ack.take().expect("waiter").await.is_ok(),
        "the replacement is answered after the join"
    );
}

#[tokio::test]
async fn a_transfer_whose_pumps_miss_the_deadline_fails_the_node_and_answers_no_waiter() {
    let mut transfer = bound_connection("transfer-unjoined", true).await;
    // These halves stand for pumps that never give the socket up.
    let (_pump_halves, witness) = LocalSocketIo::new((), ());
    transfer.conn.local_io_relinquishment = Some(witness);

    let started = tokio::time::Instant::now();
    settle(&mut transfer, RetirementCause::ForceDetach).await;
    assert!(started.elapsed() >= TRANSFER_JOIN_DEADLINE);

    let node = &transfer.state.deps.app_state.node_lifecycle;
    assert_eq!(
        node.critical_failure(),
        Some(CriticalNodeFailure::WebSocketPumpUnjoined),
        "the terminal path, not the recoverable fence"
    );
    assert!(node.admit().is_err());
    assert!(transfer.state.deps.shutdown.stop_token().is_cancelled());
    assert!(
        transfer.ack.take().expect("waiter").await.is_err(),
        "an unobserved pump is never acknowledged as detached"
    );
}

#[tokio::test]
async fn an_ordinary_close_with_slow_pumps_does_not_fail_the_node() {
    let mut transfer = bound_connection("close-unjoined", false).await;
    let (_pump_halves, witness) = LocalSocketIo::new((), ());
    transfer.conn.local_io_relinquishment = Some(witness);

    settle(&mut transfer, RetirementCause::PeerClose).await;

    let node = &transfer.state.deps.app_state.node_lifecycle;
    assert_eq!(node.critical_failure(), None);
    assert!(node.admit().is_ok());
    assert_ne!(transfer.lifecycle.state(), SocketCleanupState::Running);
}

/// The custodian's stop listener hands a pump stop to the owner directly:
/// nothing here polls the input, writes the output or runs an actor mailbox.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn a_pump_stop_reaches_the_owner_without_a_read_or_a_mailbox_turn() {
    use crate::websocket_transport::{
        start_websocket_transport, IoCustodianInventory, Limits, PumpStop, TestSocketAdmission,
        TransportLimits,
    };
    use futures::StreamExt;

    let lifecycle = NodeLifecycle::new();
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let admission = TestSocketAdmission::standalone(
        &lifecycle,
        &inventory,
        CancellationToken::new(),
        CancellationToken::new(),
    )
    .expect("admission");
    let limits = TransportLimits::new(
        Limits::new(4, 4096).expect("inbound limits"),
        Limits::new(4, 4096).expect("outbound limits"),
    );
    let (custodian, _input, _output) = start_websocket_transport(
        admission,
        limits,
        Duration::from_secs(5),
        |_: &std::convert::Infallible| false,
        |_| {
            // The peer closes at once and then stays silent.
            let reader = futures::stream::iter([Ok::<_, std::io::Error>(Message::Close(None))])
                .chain(futures::stream::pending());
            std::future::ready((reader, futures::sink::drain::<Message>()))
        },
    )
    .await
    .expect("transport starts");

    let pump_stop = PumpStop::install(&custodian);
    let reason = timeout(Duration::from_secs(5), pump_stop.stopped())
        .await
        .expect("the stop is reported");
    assert_eq!(reason, StopReason::PeerClosed);
    assert_eq!(pump_stop.reason(), Some(StopReason::PeerClosed));
    assert_eq!(
        RetirementCause::from_pump_stop(reason),
        RetirementCause::PeerClose
    );
}
