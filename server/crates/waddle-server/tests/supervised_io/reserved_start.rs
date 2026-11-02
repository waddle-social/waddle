use futures::{Sink, SinkExt, Stream, StreamExt};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{mpsc as tokio_mpsc, oneshot, Barrier as AsyncBarrier};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error, Message};
use waddle_server::test_support::supervised_io::{
    CustodianInventoryError, CustodianState, InboundTerminal, IoCustodianHandle,
    IoCustodianInventory, IoCustodianReservation, Limits, OutboundTerminal, QueueItem,
    ReservedIoStartConfig, ReservedReaperExitPoint, ReservedStartError, ReservedStartStage,
    ShutdownProtocol, StopReason, TransportLimits, WriteSender,
};
use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

type Started = (
    IoCustodianHandle,
    WriteSender<u8>,
    tokio::sync::mpsc::Receiver<QueueItem<u8>>,
);
type StartTask = tokio::task::JoinHandle<Result<Started, ReservedStartError>>;
type StageFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

struct DropGatedStart {
    task: StartTask,
    entered: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
    dropped_half: tokio_mpsc::UnboundedReceiver<u8>,
    drop_releases: [std_mpsc::SyncSender<()>; 2],
}

struct BlockingDropHalf {
    id: u8,
    entered: tokio_mpsc::UnboundedSender<u8>,
    release: std_mpsc::Receiver<()>,
}

impl Drop for BlockingDropHalf {
    fn drop(&mut self) {
        let _ = self.entered.send(self.id);
        let _ = self.release.recv();
    }
}

struct BlockingDropReader {
    _drop_gate: BlockingDropHalf,
}

impl Stream for BlockingDropReader {
    type Item = Result<u8, Infallible>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

struct BlockingDropWriter {
    _drop_gate: BlockingDropHalf,
}

impl Sink<u8> for BlockingDropWriter {
    type Error = Infallible;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, _item: u8) -> Result<(), Self::Error> {
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

fn test_limits() -> TransportLimits {
    TransportLimits::new(
        Limits::new(2, 8).expect("inbound limits"),
        Limits::new(2, 8).expect("outbound limits"),
    )
}

fn paused_start(
    reservation: IoCustodianReservation,
    target: ReservedStartStage,
    panic_after_release: bool,
) -> (StartTask, oneshot::Receiver<()>, oneshot::Sender<()>) {
    paused_start_with_reaper_exit(reservation, target, panic_after_release, None)
}

fn paused_start_with_reaper_exit(
    reservation: IoCustodianReservation,
    target: ReservedStartStage,
    panic_after_release: bool,
    exit_point: Option<ReservedReaperExitPoint>,
) -> (StartTask, oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let barrier = Arc::new(Mutex::new(Some((entered_tx, release_rx))));
    let config = ReservedIoStartConfig::new(
        test_limits(),
        ShutdownProtocol::new(0_u8, |_: &Infallible| false),
        |_| 1,
        |_| false,
        Duration::from_millis(40),
    );
    let config = match exit_point {
        Some(exit_point) => config.with_test_reaper_exit_point(exit_point),
        None => config,
    };
    let task = tokio::spawn(reservation.start_with_hook(
        config,
        |_| async {
            (
                futures::stream::pending::<Result<u8, Infallible>>(),
                futures::sink::drain::<u8>(),
            )
        },
        move |stage| {
            let pause = if stage == target {
                barrier.lock().expect("stage barrier lock").take()
            } else {
                None
            };
            Box::pin(async move {
                if let Some((entered, release)) = pause {
                    let _ = entered.send(());
                    let _ = release.await;
                    if panic_after_release {
                        panic!("injected reserved-start panic at {target:?}");
                    }
                }
            }) as StageFuture
        },
    ));
    (task, entered_rx, release_tx)
}

fn paused_start_with_blocking_drops(
    reservation: IoCustodianReservation,
    exit_point: Option<ReservedReaperExitPoint>,
) -> DropGatedStart {
    let (drop_entered_tx, drop_entered_rx) = tokio_mpsc::unbounded_channel();
    let (reader_release_tx, reader_release_rx) = std_mpsc::sync_channel(1);
    let (writer_release_tx, writer_release_rx) = std_mpsc::sync_channel(1);
    let reader = BlockingDropReader {
        _drop_gate: BlockingDropHalf {
            id: 0,
            entered: drop_entered_tx.clone(),
            release: reader_release_rx,
        },
    };
    let writer = BlockingDropWriter {
        _drop_gate: BlockingDropHalf {
            id: 1,
            entered: drop_entered_tx,
            release: writer_release_rx,
        },
    };
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let barrier = Arc::new(Mutex::new(Some((entered_tx, release_rx))));
    let config = ReservedIoStartConfig::new(
        test_limits(),
        ShutdownProtocol::new(0_u8, |_: &Infallible| false),
        |_| 1,
        |_| false,
        Duration::from_millis(40),
    );
    let config = match exit_point {
        Some(exit_point) => config.with_test_reaper_exit_point(exit_point),
        None => config,
    };
    let task = tokio::spawn(reservation.start_with_hook(
        config,
        move |_| async move { (reader, writer) },
        move |stage| {
            let pause = if stage == ReservedStartStage::BeforeInboundSpawn {
                barrier.lock().expect("stage barrier lock").take()
            } else {
                None
            };
            Box::pin(async move {
                if let Some((entered, release)) = pause {
                    let _ = entered.send(());
                    let _ = release.await;
                }
            }) as StageFuture
        },
    ));
    DropGatedStart {
        task,
        entered: entered_rx,
        release: release_tx,
        dropped_half: drop_entered_rx,
        drop_releases: [reader_release_tx, writer_release_tx],
    }
}

async fn inventory_observer(
    inventory: &IoCustodianInventory,
    connection_ref: &ConnectionRef,
) -> waddle_server::test_support::supervised_io::IoCustodianObserver {
    timeout(TEST_TIMEOUT, async {
        loop {
            if let Some(observer) = inventory.observe(connection_ref) {
                return observer;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("inventory publishes the starting observer")
}

async fn wait_terminal(
    observer: &waddle_server::test_support::supervised_io::IoCustodianObserver,
) -> CustodianState {
    timeout(TEST_TIMEOUT, observer.wait_until_joined())
        .await
        .expect("custodian completes every started pump join")
}

async fn wait_for_state(
    observer: &waddle_server::test_support::supervised_io::IoCustodianObserver,
    expected: CustodianState,
) -> CustodianState {
    timeout(TEST_TIMEOUT, async {
        loop {
            let state = observer.state();
            if state == expected {
                return state;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("custodian publishes the expected lifecycle state")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auxiliary_registration_remains_single_assignment_while_join_handle_is_awaited() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let mut reservation = inventory
        .reserve(connection_ref.clone())
        .expect("reserve exact socket");
    let cleanup = reservation
        .retain_until_auxiliary_cleanup()
        .expect("reserve one cleanup hold");
    let config = ReservedIoStartConfig::new(
        test_limits(),
        ShutdownProtocol::new(0_u8, |_: &Infallible| false),
        |_| 1,
        |_| false,
        Duration::from_millis(100),
    );
    let (custodian, outbound, inbound) = reservation
        .start_with(config, |_| async {
            (
                futures::stream::pending::<Result<u8, Infallible>>(),
                futures::sink::drain::<u8>(),
            )
        })
        .await
        .expect("start both real pump tasks");
    drop(outbound);
    drop(inbound);
    let observer = inventory
        .observe(&connection_ref)
        .expect("exact reservation has an observer");

    let (task_entered_tx, task_entered_rx) = oneshot::channel();
    let (task_release_tx, task_release_rx) = oneshot::channel();
    let first_task = tokio::spawn(async move {
        let _ = task_entered_tx.send(());
        let _ = task_release_rx.await;
    });
    inventory
        .register_auxiliary_task(&connection_ref, first_task)
        .expect("first auxiliary task is retained");
    cleanup.complete();
    custodian.request_stop(StopReason::Requested);
    timeout(TEST_TIMEOUT, task_entered_rx)
        .await
        .expect("auxiliary task started")
        .expect("task entry signal");
    timeout(TEST_TIMEOUT, async {
        while !observer.auxiliary_join_started() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reaper took the first handle and is joining it");
    assert_ne!(observer.state(), CustodianState::Joined);
    assert_eq!(inventory.prune_joined(), 0);

    let second_task = tokio::spawn(std::future::pending::<()>());
    assert_eq!(
        inventory.register_auxiliary_task(&connection_ref, second_task),
        Err(CustodianInventoryError::AuxiliaryTaskAlreadyRegistered)
    );
    let _ = task_release_tx.send(());
    assert_eq!(wait_terminal(&observer).await, CustodianState::Joined);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn reservation_refuses_a_second_auxiliary_cleanup_hold() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let mut reservation = inventory
        .reserve(connection_ref)
        .expect("reserve exact socket");
    let first = reservation
        .retain_until_auxiliary_cleanup()
        .expect("first cleanup hold");
    assert!(matches!(
        reservation.retain_until_auxiliary_cleanup(),
        Err(CustodianInventoryError::AuxiliaryHoldAlreadyRetained)
    ));
    drop(first);
    drop(reservation);
    assert!(inventory.is_empty(), "unused reservation releases its slot");
}

#[tokio::test]
async fn completed_auxiliary_guard_without_registration_stays_pending_until_task_arrives() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let mut reservation = inventory
        .reserve(connection_ref.clone())
        .expect("reserve exact socket");
    let cleanup = reservation
        .retain_until_auxiliary_cleanup()
        .expect("retain auxiliary cleanup hold");
    let config = ReservedIoStartConfig::new(
        test_limits(),
        ShutdownProtocol::new(0_u8, |_: &Infallible| false),
        |_| 1,
        |_| false,
        Duration::from_millis(40),
    );
    let (custodian, outbound, inbound) = reservation
        .start_with(config, |_| async {
            (
                futures::stream::pending::<Result<u8, Infallible>>(),
                futures::sink::drain::<u8>(),
            )
        })
        .await
        .expect("start both pump tasks");
    drop(outbound);
    drop(inbound);
    let observer = inventory
        .observe(&connection_ref)
        .expect("exact reservation has an observer");
    cleanup.complete();
    custodian.request_stop(waddle_server::test_support::supervised_io::StopReason::Requested);
    assert_eq!(
        wait_for_state(&observer, CustodianState::CleanupPending).await,
        CustodianState::CleanupPending
    );
    assert_eq!(inventory.prune_joined(), 0);

    inventory
        .register_auxiliary_task(&connection_ref, tokio::spawn(async {}))
        .expect("late cleanup owner is retained and wakes the reaper");
    assert_eq!(wait_terminal(&observer).await, CustodianState::Joined);
    assert_eq!(inventory.prune_joined(), 1);
}

async fn panic_before_building_pump_halves() -> (
    futures::stream::Pending<Result<u8, Infallible>>,
    futures::sink::Drain<u8>,
) {
    panic!("injected pump factory panic")
}

#[tokio::test]
async fn reservation_is_exact_raii_bounded_and_race_safe() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let first = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let other = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let reservation = inventory
        .reserve(first.clone())
        .expect("reserve first socket");

    assert!(reservation.is_for(&first));
    assert!(inventory.observe(&first).is_none());
    assert_eq!(
        inventory.reserve(first.clone()).err(),
        Some(CustodianInventoryError::DuplicateConnection)
    );
    assert_eq!(
        inventory.reserve(other.clone()).err(),
        Some(CustodianInventoryError::Full)
    );
    drop(reservation);
    assert!(inventory.is_empty(), "unused reservation returns its slot");

    let barrier = Arc::new(AsyncBarrier::new(3));
    let first_inventory = inventory.clone();
    let first_barrier = Arc::clone(&barrier);
    let first_ref = first.clone();
    let first_task = tokio::spawn(async move {
        first_barrier.wait().await;
        first_inventory.reserve(first_ref)
    });
    let second_inventory = inventory.clone();
    let second_barrier = Arc::clone(&barrier);
    let second_ref = other.clone();
    let second_task = tokio::spawn(async move {
        second_barrier.wait().await;
        second_inventory.reserve(second_ref)
    });
    barrier.wait().await;

    let first_result = first_task.await.expect("first reservation caller");
    let second_result = second_task.await.expect("second reservation caller");
    match (first_result, second_result) {
        (Ok(winner), Err(error)) | (Err(error), Ok(winner)) => {
            assert_eq!(error, CustodianInventoryError::Full);
            drop(winner);
        }
        _ => panic!("exactly one final-slot reservation must win"),
    }
    assert!(inventory.is_empty());
}

#[tokio::test]
async fn cancellation_at_each_reserved_start_boundary_joins_only_started_pumps() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");

    let before_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let before = inventory
        .reserve(before_ref.clone())
        .expect("reserve before start");
    let (before_task, before_entered, before_release) =
        paused_start(before, ReservedStartStage::BeforeInboundSpawn, false);
    timeout(TEST_TIMEOUT, before_entered)
        .await
        .expect("pause before the first pump")
        .expect("before-start barrier");
    let before_observer = inventory_observer(&inventory, &before_ref).await;
    assert_eq!(before_observer.state(), CustodianState::Starting);
    before_task.abort();
    assert!(before_task
        .await
        .err()
        .expect("start future is canceled")
        .is_cancelled());
    let _ = before_release.send(());
    assert_eq!(
        wait_terminal(&before_observer).await,
        CustodianState::StartupAbandoned
    );
    assert!(!before_observer.cleanup_pending_was_published());
    assert!(
        inventory.is_empty(),
        "no started pump leaves no retained slot"
    );

    let partial_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let partial = inventory
        .reserve(partial_ref.clone())
        .expect("reserve partial start");
    let (partial_task, partial_entered, partial_release) =
        paused_start(partial, ReservedStartStage::BetweenPumpSpawns, false);
    timeout(TEST_TIMEOUT, partial_entered)
        .await
        .expect("pause after the inbound handle handoff")
        .expect("between-pumps barrier");
    let partial_observer = inventory_observer(&inventory, &partial_ref).await;
    partial_task.abort();
    assert!(partial_task
        .await
        .err()
        .expect("partial start is canceled")
        .is_cancelled());
    let _ = partial_release.send(());
    assert_eq!(
        wait_terminal(&partial_observer).await,
        CustodianState::PartialStartJoined
    );
    assert!(!partial_observer.cleanup_pending_was_published());
    let partial_summary = partial_observer
        .terminal_summary()
        .expect("partial-start outcomes retained");
    assert_eq!(partial_summary.inbound, InboundTerminal::Aborted);
    assert_eq!(partial_summary.outbound, OutboundTerminal::NotStarted);
    assert_eq!(
        inventory.len(),
        1,
        "partial cleanup remains indexed until pruned"
    );
    assert_eq!(inventory.prune_joined(), 1);

    let precommit_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let precommit = inventory
        .reserve(precommit_ref.clone())
        .expect("reserve complete pair");
    let (precommit_task, precommit_entered, precommit_release) =
        paused_start(precommit, ReservedStartStage::AfterOutboundSpawn, false);
    timeout(TEST_TIMEOUT, precommit_entered)
        .await
        .expect("pause after the outbound handle handoff")
        .expect("after-second-pump barrier");
    let precommit_observer = inventory_observer(&inventory, &precommit_ref).await;
    precommit_task.abort();
    assert!(precommit_task
        .await
        .err()
        .expect("precommit start is canceled")
        .is_cancelled());
    let _ = precommit_release.send(());
    assert_eq!(
        wait_terminal(&precommit_observer).await,
        CustodianState::Joined
    );
    let precommit_summary = precommit_observer
        .terminal_summary()
        .expect("both aborted outcomes retained");
    assert_eq!(precommit_summary.inbound, InboundTerminal::Aborted);
    assert_eq!(precommit_summary.outbound, OutboundTerminal::Aborted);
    assert_eq!(inventory.prune_joined(), 1);

    let committed_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let committed = inventory
        .reserve(committed_ref.clone())
        .expect("reserve committed pair");
    let (committed_task, committed_entered, committed_release) =
        paused_start(committed, ReservedStartStage::AfterCommit, false);
    timeout(TEST_TIMEOUT, committed_entered)
        .await
        .expect("pause after ownership commit")
        .expect("postcommit barrier");
    let committed_observer = inventory_observer(&inventory, &committed_ref).await;
    committed_task.abort();
    assert!(committed_task
        .await
        .err()
        .expect("postcommit start future is canceled")
        .is_cancelled());
    let _ = committed_release.send(());
    assert_eq!(
        wait_terminal(&committed_observer).await,
        CustodianState::Joined
    );
    let committed_summary = committed_observer
        .terminal_summary()
        .expect("committed stop outcomes retained");
    assert_eq!(committed_summary.inbound, InboundTerminal::Stopped);
    assert_eq!(committed_summary.outbound, OutboundTerminal::Stopped);
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test]
async fn reaper_loss_returns_typed_error_and_keeps_cleanup_pending_indexed() {
    let inventory = IoCustodianInventory::new(2).expect("inventory capacity");

    for exit_point in [
        ReservedReaperExitPoint::BeforeCommit,
        ReservedReaperExitPoint::BeforeReady,
    ] {
        let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
        let reservation = inventory
            .reserve(connection_ref.clone())
            .expect("reserve exact connection");
        let config = ReservedIoStartConfig::new(
            test_limits(),
            ShutdownProtocol::new(0_u8, |_: &Infallible| false),
            |_| 1,
            |_| false,
            Duration::from_millis(40),
        )
        .with_test_reaper_exit_point(exit_point);

        let result = timeout(
            TEST_TIMEOUT,
            reservation.start_with(config, |_| async {
                (
                    futures::stream::pending::<Result<u8, Infallible>>(),
                    futures::sink::drain::<u8>(),
                )
            }),
        )
        .await
        .expect("starter observes the lost reaper");
        assert_eq!(result.err(), Some(ReservedStartError::ReaperUnavailable));

        let observer = inventory_observer(&inventory, &connection_ref).await;
        assert_eq!(observer.state(), CustodianState::CleanupPending);
        assert!(observer.cleanup_pending_was_published());
        assert!(observer.terminal_summary().is_none());
        assert_eq!(inventory.prune_joined(), 0);
    }
}

#[tokio::test]
async fn reaper_loss_then_starter_cancellation_finishes_no_start_and_retains_partial_cleanup() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");

    let no_start_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let no_start = inventory
        .reserve(no_start_ref.clone())
        .expect("reserve no-start connection");
    let (no_start_task, no_start_entered, no_start_release) = paused_start_with_reaper_exit(
        no_start,
        ReservedStartStage::BeforeInboundSpawn,
        false,
        Some(ReservedReaperExitPoint::BeforeCommit),
    );
    timeout(TEST_TIMEOUT, no_start_entered)
        .await
        .expect("starter pauses before either pump")
        .expect("no-start barrier notification");
    let no_start_observer = inventory_observer(&inventory, &no_start_ref).await;
    no_start_task.abort();
    assert!(no_start_task
        .await
        .err()
        .expect("starter cancellation is reported")
        .is_cancelled());
    let _ = no_start_release.send(());
    assert_eq!(
        wait_terminal(&no_start_observer).await,
        CustodianState::StartupAbandoned
    );
    assert!(!no_start_observer.cleanup_pending_was_published());
    let no_start_summary = no_start_observer
        .terminal_summary()
        .expect("no-start outcome retained");
    assert_eq!(no_start_summary.inbound, InboundTerminal::NotStarted);
    assert_eq!(no_start_summary.outbound, OutboundTerminal::NotStarted);
    assert!(inventory.is_empty(), "no started pump releases capacity");

    let partial_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let partial = inventory
        .reserve(partial_ref.clone())
        .expect("released capacity accepts the next connection");
    let (partial_task, partial_entered, partial_release) = paused_start_with_reaper_exit(
        partial,
        ReservedStartStage::BetweenPumpSpawns,
        false,
        Some(ReservedReaperExitPoint::BeforeCommit),
    );
    timeout(TEST_TIMEOUT, partial_entered)
        .await
        .expect("starter pauses after the inbound handle handoff")
        .expect("partial-start barrier notification");
    let partial_observer = inventory_observer(&inventory, &partial_ref).await;
    partial_task.abort();
    assert!(partial_task
        .await
        .err()
        .expect("partial starter cancellation is reported")
        .is_cancelled());
    let _ = partial_release.send(());
    assert_eq!(
        wait_for_state(&partial_observer, CustodianState::CleanupPending).await,
        CustodianState::CleanupPending
    );
    assert!(partial_observer.cleanup_pending_was_published());
    assert!(partial_observer.terminal_summary().is_none());
    assert_eq!(inventory.len(), 1, "partial cleanup keeps its slot");
    assert_eq!(
        inventory
            .reserve(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
            .err(),
        Some(CustodianInventoryError::Full)
    );
    assert_eq!(inventory.prune_joined(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn no_start_capacity_waits_for_both_socket_halves_to_drop() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");

    for exit_point in [None, Some(ReservedReaperExitPoint::BeforeCommit)] {
        let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
        let reservation = inventory
            .reserve(connection_ref.clone())
            .expect("reserve no-start connection");
        let DropGatedStart {
            task: start,
            entered: start_entered,
            release: start_release,
            mut dropped_half,
            drop_releases,
        } = paused_start_with_blocking_drops(reservation, exit_point);
        timeout(TEST_TIMEOUT, start_entered)
            .await
            .expect("starter reaches the first-pump barrier")
            .expect("first-pump barrier notification");
        let observer = inventory_observer(&inventory, &connection_ref).await;
        start.abort();
        let _ = start_release.send(());

        let first_drop = timeout(TEST_TIMEOUT, dropped_half.recv())
            .await
            .expect("one physical half starts dropping")
            .expect("drop event");
        assert!(
            !observer.startup_completion_is_finished(),
            "startup completion cannot publish while either Drop is blocked"
        );
        assert_eq!(inventory.len(), 1);
        assert_eq!(
            inventory
                .reserve(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
                .err(),
            Some(CustodianInventoryError::Full),
            "inventory remains occupied while the first half's Drop blocks"
        );
        let _ = drop_releases[usize::from(first_drop)].send(());

        let second_drop = timeout(TEST_TIMEOUT, dropped_half.recv())
            .await
            .expect("the other physical half starts dropping")
            .expect("second drop event");
        assert_ne!(first_drop, second_drop);
        assert!(
            !observer.startup_completion_is_finished(),
            "startup completion waits until both half destructors return"
        );
        assert_eq!(inventory.len(), 1);
        assert_eq!(
            inventory
                .reserve(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
                .err(),
            Some(CustodianInventoryError::Full),
            "inventory remains occupied while the second half's Drop blocks"
        );
        let _ = drop_releases[usize::from(second_drop)].send(());

        assert!(timeout(TEST_TIMEOUT, start)
            .await
            .expect("canceled startup finishes after both Drop gates open")
            .err()
            .expect("starter was canceled")
            .is_cancelled());
        assert_eq!(
            wait_terminal(&observer).await,
            CustodianState::StartupAbandoned
        );
        assert!(observer.startup_completion_is_finished());
        assert_eq!(inventory.len(), 0, "no-start capacity releases after Drop");
        assert!(inventory.observe(&connection_ref).is_none());
        let successor_reservation = inventory
            .reserve(connection_ref.clone())
            .expect("terminal no-start state implies its exact slot is available");
        assert_eq!(observer.state(), CustodianState::StartupAbandoned);
        drop(successor_reservation);
    }
}

#[tokio::test]
async fn panic_before_between_and_after_reserved_spawns_never_loses_started_handles() {
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let factory_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let factory_reservation = inventory
        .reserve(factory_ref.clone())
        .expect("reserve before factory");
    let factory_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let factory_called = Arc::clone(&factory_calls);
    let factory_task = tokio::spawn(factory_reservation.start_with(
        ReservedIoStartConfig::new(
            test_limits(),
            ShutdownProtocol::new(0_u8, |_: &Infallible| false),
            |_| 1,
            |_| false,
            Duration::from_millis(40),
        ),
        move |_| {
            factory_called.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            panic_before_building_pump_halves()
        },
    ));
    assert!(timeout(TEST_TIMEOUT, factory_task)
        .await
        .expect("factory panic task completes")
        .err()
        .expect("factory panic surfaces")
        .is_panic());
    assert_eq!(factory_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(
        inventory.is_empty(),
        "factory panic releases its unused slot"
    );

    let before_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let before = inventory
        .reserve(before_ref.clone())
        .expect("reserve before panic");
    let (before_task, before_entered, before_release) =
        paused_start(before, ReservedStartStage::BeforeInboundSpawn, true);
    timeout(TEST_TIMEOUT, before_entered)
        .await
        .expect("pause before first spawn")
        .expect("before-spawn panic barrier");
    let before_observer = inventory_observer(&inventory, &before_ref).await;
    let _ = before_release.send(());
    assert!(timeout(TEST_TIMEOUT, before_task)
        .await
        .expect("startup panics")
        .err()
        .expect("hook panic surfaces")
        .is_panic());
    assert_eq!(
        wait_terminal(&before_observer).await,
        CustodianState::StartupAbandoned
    );
    assert!(!before_observer.cleanup_pending_was_published());
    assert!(inventory.is_empty());

    let between_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let between = inventory
        .reserve(between_ref.clone())
        .expect("reserve between panic");
    let (between_task, between_entered, between_release) =
        paused_start(between, ReservedStartStage::BetweenPumpSpawns, true);
    timeout(TEST_TIMEOUT, between_entered)
        .await
        .expect("pause between pump spawns")
        .expect("between-spawn panic barrier");
    let between_observer = inventory_observer(&inventory, &between_ref).await;
    let _ = between_release.send(());
    assert!(timeout(TEST_TIMEOUT, between_task)
        .await
        .expect("startup panics")
        .err()
        .expect("hook panic surfaces")
        .is_panic());
    assert_eq!(
        wait_terminal(&between_observer).await,
        CustodianState::PartialStartJoined
    );
    assert!(!between_observer.cleanup_pending_was_published());
    let between_summary = between_observer
        .terminal_summary()
        .expect("partial panic outcomes retained");
    assert_eq!(between_summary.inbound, InboundTerminal::Aborted);
    assert_eq!(between_summary.outbound, OutboundTerminal::NotStarted);
    assert_eq!(inventory.prune_joined(), 1);

    let after_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let after = inventory
        .reserve(after_ref.clone())
        .expect("reserve after spawn");
    let (after_task, after_entered, after_release) =
        paused_start(after, ReservedStartStage::AfterOutboundSpawn, true);
    timeout(TEST_TIMEOUT, after_entered)
        .await
        .expect("pause after both pump spawns")
        .expect("post-spawn panic barrier");
    let after_observer = inventory_observer(&inventory, &after_ref).await;
    let _ = after_release.send(());
    assert!(timeout(TEST_TIMEOUT, after_task)
        .await
        .expect("startup panics")
        .err()
        .expect("hook panic surfaces")
        .is_panic());
    assert_eq!(wait_terminal(&after_observer).await, CustodianState::Joined);
    let after_summary = after_observer
        .terminal_summary()
        .expect("both panic-cleanup outcomes retained");
    assert_eq!(after_summary.inbound, InboundTerminal::Aborted);
    assert_eq!(after_summary.outbound, OutboundTerminal::Aborted);
    assert_eq!(inventory.prune_joined(), 1);
}

struct CountStream<S> {
    inner: S,
    polls: Arc<std::sync::atomic::AtomicUsize>,
}

impl<S: Stream + Unpin> Stream for CountStream<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.polls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Pin::new(&mut self.inner).poll_next(context)
    }
}

struct CountSink<S> {
    inner: S,
    polls: Arc<std::sync::atomic::AtomicUsize>,
}

impl<S: Sink<Message, Error = Error> + Unpin> Sink<Message> for CountSink<S> {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.polls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Pin::new(&mut self.inner).poll_ready(context)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        Pin::new(&mut self.inner).start_send(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.polls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_close(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.polls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Pin::new(&mut self.inner).poll_close(context)
    }
}

#[tokio::test]
async fn reserved_loopback_pumps_do_not_poll_io_before_reaper_commit() {
    let (socket, mut peer) = super::pumps::socket_pair().await;
    let (writer, reader) = socket.split();
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let reservation = inventory
        .reserve(connection_ref.clone())
        .expect("reserve before pump construction");
    let read_polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let write_polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let hook_gate = Arc::new(Mutex::new(Some((entered_tx, release_rx))));
    let factory_read_polls = Arc::clone(&read_polls);
    let factory_write_polls = Arc::clone(&write_polls);
    let task = tokio::spawn(reservation.start_with_hook(
        ReservedIoStartConfig::new(
            test_limits(),
            ShutdownProtocol::new(Message::Close(None), |error: &Error| {
                matches!(error, Error::Protocol(ProtocolError::SendAfterClosing))
            }),
            Message::len,
            Message::is_close,
            Duration::from_millis(100),
        ),
        move |_| async move {
            (
                CountStream {
                    inner: reader,
                    polls: factory_read_polls,
                },
                CountSink {
                    inner: writer,
                    polls: factory_write_polls,
                },
            )
        },
        {
            let hook_gate = Arc::clone(&hook_gate);
            move |stage| {
                let pause = if stage == ReservedStartStage::AfterOutboundSpawn {
                    hook_gate.lock().expect("loopback gate lock").take()
                } else {
                    None
                };
                Box::pin(async move {
                    if let Some((entered, release)) = pause {
                        let _ = entered.send(());
                        let _ = release.await;
                    }
                }) as StageFuture
            }
        },
    ));

    timeout(TEST_TIMEOUT, entered_rx)
        .await
        .expect("both handles handed to the reaper before the barrier")
        .expect("loopback start barrier");
    let observer = inventory_observer(&inventory, &connection_ref).await;
    assert_eq!(observer.state(), CustodianState::Starting);
    assert_eq!(read_polls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(write_polls.load(std::sync::atomic::Ordering::Relaxed), 0);

    release_tx.send(()).expect("release ownership barrier");
    let (custodian, outbound, mut inbound) = timeout(TEST_TIMEOUT, task)
        .await
        .expect("reserved startup returns")
        .expect("start task")
        .expect("reservation remains owned");
    assert_eq!(observer.state(), CustodianState::Running);

    peer.send(Message::Text("inbound".into()))
        .await
        .expect("send loopback inbound frame");
    let received = timeout(TEST_TIMEOUT, inbound.recv())
        .await
        .expect("reader pump receives the frame")
        .expect("inbound queue item")
        .into_inner();
    assert!(matches!(received, Message::Text(text) if text == "inbound"));

    outbound
        .try_send(Message::Text("outbound".into()), 8)
        .expect("admit loopback outbound frame");
    let sent = timeout(TEST_TIMEOUT, peer.next())
        .await
        .expect("writer pump flushes the frame")
        .expect("peer frame")
        .expect("valid peer frame");
    assert!(matches!(sent, Message::Text(text) if text == "outbound"));
    assert!(read_polls.load(std::sync::atomic::Ordering::Relaxed) > 0);
    assert!(write_polls.load(std::sync::atomic::Ordering::Relaxed) > 0);

    custodian.request_stop(waddle_server::test_support::supervised_io::StopReason::Requested);
    assert_eq!(wait_terminal(&observer).await, CustodianState::Joined);
}
