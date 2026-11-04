use futures::{Sink, Stream};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use waddle_server::test_support::supervised_io::{
    CustodianState, InboundTerminal, IoCustodianHandle, IoCustodianInventory, IoCustodianObserver,
    Limits, OutboundTerminal, QueueItem, ReservedIoStartConfig, ReservedStartError,
    ReservedStartStage, ShutdownProtocol, StopReason, TransportLimits, WriteSender,
};
use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::registry::LocalIoRelinquishmentOutcome;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const READER: u8 = 1;
const WRITER: u8 = 2;

struct DropProbe {
    half: u8,
    entered: mpsc::UnboundedSender<u8>,
    finished: Arc<AtomicU8>,
    release: Option<std_mpsc::Receiver<()>>,
    panics: bool,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        let _ = self.entered.send(self.half);
        if let Some(release) = self.release.take() {
            let _ = release.recv();
        }
        assert!(!self.panics, "injected half destructor panic");
        self.finished.fetch_or(self.half, Ordering::Release);
    }
}

struct Reader {
    _probe: DropProbe,
}

impl Stream for Reader {
    type Item = Result<u8, Infallible>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

struct Writer {
    _probe: DropProbe,
}

impl Sink<u8> for Writer {
    type Error = Infallible;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, _: u8) -> Result<(), Self::Error> {
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

type Started = (
    IoCustodianHandle,
    WriteSender<u8>,
    mpsc::Receiver<QueueItem<u8>>,
);
type StartTask = tokio::task::JoinHandle<Result<Started, ReservedStartError>>;
type StageFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

struct StartedConnection {
    handle: IoCustodianHandle,
    observer: IoCustodianObserver,
    _output: WriteSender<u8>,
    _input: mpsc::Receiver<QueueItem<u8>>,
}

fn config() -> ReservedIoStartConfig<
    u8,
    impl Fn(&Infallible) -> bool,
    impl Fn(&u8) -> usize,
    impl Fn(&u8) -> bool,
> {
    ReservedIoStartConfig::new(
        TransportLimits::new(
            Limits::new(2, 8).expect("inbound limits"),
            Limits::new(2, 8).expect("outbound limits"),
        ),
        ShutdownProtocol::new(0, |_: &Infallible| false),
        |_: &u8| 1,
        |_: &u8| false,
        Duration::from_millis(30),
    )
}

fn halves(
    reader_release: Option<std_mpsc::Receiver<()>>,
    writer_release: Option<std_mpsc::Receiver<()>>,
    panic_half: Option<u8>,
) -> (Reader, Writer, mpsc::UnboundedReceiver<u8>, Arc<AtomicU8>) {
    let (entered, events) = mpsc::unbounded_channel();
    let finished = Arc::new(AtomicU8::new(0));
    let probe = |half, release| DropProbe {
        half,
        entered: entered.clone(),
        finished: finished.clone(),
        release,
        panics: panic_half == Some(half),
    };
    (
        Reader {
            _probe: probe(READER, reader_release),
        },
        Writer {
            _probe: probe(WRITER, writer_release),
        },
        events,
        finished,
    )
}

async fn start(
    inventory: &IoCustodianInventory,
    connection_ref: &ConnectionRef,
    reader: Reader,
    writer: Writer,
) -> StartedConnection {
    let reservation = inventory.reserve(connection_ref.clone()).expect("reserve");
    let (handle, output, input) = reservation
        .start_with(config(), move |_| async move { (reader, writer) })
        .await
        .expect("start pumps");
    let observer = inventory.observe(connection_ref).expect("indexed observer");
    StartedConnection {
        handle,
        observer,
        _output: output,
        _input: input,
    }
}

async fn event(events: &mut mpsc::UnboundedReceiver<u8>) -> u8 {
    timeout(TEST_TIMEOUT, events.recv())
        .await
        .expect("half destructor entered")
        .expect("event channel open")
}

async fn wait_state(observer: &IoCustodianObserver, state: CustodianState) {
    timeout(TEST_TIMEOUT, async {
        while observer.state() != state {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expected custodian state");
}

async fn wait_finished(finished: &AtomicU8, half: u8) {
    timeout(TEST_TIMEOUT, async {
        while finished.load(Ordering::Acquire) & half == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("half destructor returned");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_half_destructors_must_both_return_in_either_order() {
    for first in [READER, WRITER] {
        let inventory = IoCustodianInventory::new(1).expect("inventory");
        let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
        let (reader_release, reader_gate) = std_mpsc::sync_channel(1);
        let (writer_release, writer_gate) = std_mpsc::sync_channel(1);
        let (reader, writer, mut events, finished) =
            halves(Some(reader_gate), Some(writer_gate), None);
        let started = start(&inventory, &connection_ref, reader, writer).await;
        let witness = started.observer.local_io_relinquishment();
        assert_eq!(witness.outcome(), None);
        started.handle.request_stop(StopReason::Requested);
        let first_entered = event(&mut events).await;
        let second_entered = event(&mut events).await;
        assert_eq!(first_entered | second_entered, READER | WRITER);
        assert_eq!(
            witness.outcome(),
            None,
            "both destructors are still blocked"
        );

        if first == READER {
            reader_release.send(()).expect("release reader");
        } else {
            writer_release.send(()).expect("release writer");
        }
        wait_finished(&finished, first).await;
        assert_eq!(witness.outcome(), None, "the other destructor is blocked");
        if first == READER {
            writer_release.send(()).expect("release writer");
        } else {
            reader_release.send(()).expect("release reader");
        }
        assert_eq!(
            timeout(TEST_TIMEOUT, witness.wait())
                .await
                .expect("relinquishment"),
            LocalIoRelinquishmentOutcome::Relinquished
        );
        assert_eq!(finished.load(Ordering::Acquire), READER | WRITER);
        assert_eq!(
            started.observer.wait_until_joined().await,
            CustodianState::Joined
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cleanup_pending_retains_the_witness_until_the_held_half_releases() {
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let (release, gate) = std_mpsc::sync_channel(1);
    let (reader, writer, mut events, _) = halves(None, Some(gate), None);
    let started = start(&inventory, &connection_ref, reader, writer).await;
    let witness = started.observer.local_io_relinquishment();
    started.handle.request_stop(StopReason::Requested);
    assert_eq!(
        event(&mut events).await | event(&mut events).await,
        READER | WRITER
    );
    wait_state(&started.observer, CustodianState::CleanupPending).await;
    assert_eq!(witness.outcome(), None);
    assert!(futures::poll!(Box::pin(witness.wait()).as_mut()).is_pending());
    assert_eq!(inventory.prune_joined(), 0);
    assert!(inventory.observe(&connection_ref).is_some());
    release.send(()).expect("release writer");
    assert_eq!(
        started.observer.wait_until_joined().await,
        CustodianState::Joined
    );
    assert_eq!(
        witness.wait().await,
        LocalIoRelinquishmentOutcome::Relinquished
    );
    assert_eq!(inventory.prune_joined(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn either_panicking_destructor_is_unobserved_only_after_the_other_join() {
    for panic_half in [READER, WRITER] {
        let inventory = IoCustodianInventory::new(1).expect("inventory");
        let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
        let (release, gate) = std_mpsc::sync_channel(1);
        let (reader_gate, writer_gate) = if panic_half == READER {
            (None, Some(gate))
        } else {
            (Some(gate), None)
        };
        let (reader, writer, mut events, _) = halves(reader_gate, writer_gate, Some(panic_half));
        let started = start(&inventory, &connection_ref, reader, writer).await;
        let witness = started.observer.local_io_relinquishment();
        started.handle.request_stop(StopReason::Requested);
        assert_eq!(
            event(&mut events).await | event(&mut events).await,
            READER | WRITER
        );
        wait_state(&started.observer, CustodianState::CleanupPending).await;
        assert_eq!(witness.outcome(), None);
        assert_eq!(inventory.prune_joined(), 0);
        release.send(()).expect("release other half");
        assert_eq!(
            started.observer.wait_until_joined().await,
            CustodianState::Joined
        );
        assert_eq!(
            witness.wait().await,
            LocalIoRelinquishmentOutcome::Unobserved
        );
        let summary = started
            .observer
            .terminal_summary()
            .expect("terminal outcomes");
        if panic_half == READER {
            assert_eq!(summary.inbound, InboundTerminal::Panicked);
        } else {
            assert_eq!(summary.outbound, OutboundTerminal::Panicked);
        }
    }
}

fn paused_start(
    inventory: &IoCustodianInventory,
    connection_ref: &ConnectionRef,
    target: ReservedStartStage,
) -> (StartTask, oneshot::Receiver<()>, Arc<AtomicU8>) {
    let reservation = inventory.reserve(connection_ref.clone()).expect("reserve");
    let (reader, writer, _events, finished) = halves(None, None, None);
    let (entered, reached) = oneshot::channel();
    let barrier = Arc::new(Mutex::new(Some(entered)));
    let task = tokio::spawn(reservation.start_with_hook(
        config(),
        move |_| async move { (reader, writer) },
        move |stage| {
            let entered = if stage == target {
                barrier.lock().expect("barrier").take()
            } else {
                None
            };
            Box::pin(async move {
                if let Some(entered) = entered {
                    let _ = entered.send(());
                    std::future::pending::<()>().await;
                }
            }) as StageFuture
        },
    ));
    (task, reached, finished)
}

#[tokio::test]
async fn aborted_startup_observes_actual_halves_before_and_between_pump_spawns() {
    for (stage, expected) in [
        (
            ReservedStartStage::BeforeInboundSpawn,
            CustodianState::StartupAbandoned,
        ),
        (
            ReservedStartStage::BetweenPumpSpawns,
            CustodianState::PartialStartJoined,
        ),
        (
            ReservedStartStage::AfterOutboundSpawn,
            CustodianState::Joined,
        ),
    ] {
        let inventory = IoCustodianInventory::new(1).expect("inventory");
        let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
        let (task, reached, finished) = paused_start(&inventory, &connection_ref, stage);
        timeout(TEST_TIMEOUT, reached)
            .await
            .expect("startup barrier")
            .expect("barrier open");
        let observer = inventory
            .observe(&connection_ref)
            .expect("starting observer");
        let witness = observer.local_io_relinquishment();
        assert_eq!(witness.outcome(), None);
        task.abort();
        assert!(task.await.err().expect("start canceled").is_cancelled());
        assert_eq!(
            timeout(TEST_TIMEOUT, observer.wait_until_joined())
                .await
                .expect("startup joins"),
            expected
        );
        assert_eq!(finished.load(Ordering::Acquire), READER | WRITER);
        assert_eq!(
            witness.wait().await,
            LocalIoRelinquishmentOutcome::Relinquished
        );
    }
}

#[tokio::test]
async fn factory_without_returned_halves_has_no_witness() {
    let inventory = IoCustodianInventory::new(1).expect("inventory");
    let connection_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let reservation = inventory.reserve(connection_ref.clone()).expect("reserve");
    let cancellation = CancellationToken::new();
    let (entered, reached) = oneshot::channel();
    let task = tokio::spawn(reservation.start_with_hook_and_cancellation(
        config(),
        move |_| async move {
            let _ = entered.send(());
            std::future::pending::<(Reader, Writer)>().await
        },
        cancellation.clone(),
        |_| std::future::ready(()),
    ));
    reached.await.expect("factory started");
    assert!(inventory.observe(&connection_ref).is_none());
    cancellation.cancel();
    assert!(matches!(
        task.await.expect("starter joined"),
        Err(ReservedStartError::CancelledBeforePumps)
    ));
    assert!(inventory.is_empty());
    assert!(inventory.observe(&connection_ref).is_none());

    let called = Arc::new(AtomicBool::new(false));
    let called_by_factory = called.clone();
    let reservation = inventory
        .reserve(connection_ref.clone())
        .expect("reserve again");
    let result = reservation
        .start_with_hook_and_cancellation(
            config(),
            move |_| {
                called_by_factory.store(true, Ordering::Release);
                std::future::pending::<(Reader, Writer)>()
            },
            cancellation,
            |_| std::future::ready(()),
        )
        .await;
    assert!(matches!(
        result,
        Err(ReservedStartError::CancelledBeforePumps)
    ));
    assert!(!called.load(Ordering::Acquire));
    assert!(inventory.observe(&connection_ref).is_none());
}

#[tokio::test]
async fn inventory_observers_are_bound_to_exact_connection_refs() {
    let inventory = IoCustodianInventory::new(2).expect("inventory");
    let first_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let second_ref = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let (reader, writer, _events, _) = halves(None, None, None);
    let first = start(&inventory, &first_ref, reader, writer).await;
    let (reader, writer, _events, _) = halves(None, None, None);
    let second = start(&inventory, &second_ref, reader, writer).await;
    let first_witness = first.observer.local_io_relinquishment();
    let second_witness = second.observer.local_io_relinquishment();
    assert_eq!(first_witness.connection_ref(), &first_ref);
    assert!(first_witness.is_for(&first_ref));
    assert!(!first_witness.is_for(&second_ref));
    assert!(second_witness.is_for(&second_ref));
    first.handle.request_stop(StopReason::Requested);
    assert_eq!(
        first_witness.wait().await,
        LocalIoRelinquishmentOutcome::Relinquished
    );
    assert_eq!(second_witness.outcome(), None);
    assert!(inventory
        .observe(&second_ref)
        .expect("second indexed")
        .local_io_relinquishment()
        .is_for(&second_ref));
    second.handle.request_stop(StopReason::Requested);
    assert_eq!(
        second_witness.wait().await,
        LocalIoRelinquishmentOutcome::Relinquished
    );
}
