use futures::Sink;
use kameo::actor::Spawn;
use kameo::error::SendError;
use kameo::mailbox;
use kameo::message::{Context, Message};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use waddle_server::test_support::supervised_io::{
    spawn_io_custodian, spawn_supervised_io, CustodianInventoryError, CustodianState,
    InboundTerminal, IoCustodianHandle, IoCustodianInventory, Limits, OutboundTerminal, QueueItem,
    ReservedIoStartConfig, ShutdownProtocol, StopReason, TransportLimits, WriteSender,
};
use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};

#[derive(kameo::Actor)]
struct ControlProbe {
    entered: Option<oneshot::Sender<()>>,
    observed: Option<oneshot::Sender<()>>,
    interrupt: CancellationToken,
}

struct BlockTurn;
struct Data;
struct Control;

impl Message<BlockTurn> for ControlProbe {
    type Reply = ();

    async fn handle(&mut self, _: BlockTurn, ctx: &mut Context<Self, Self::Reply>) {
        self.entered
            .take()
            .expect("entered signal")
            .send(())
            .expect("observer");
        self.interrupt.cancelled().await;
        self.observed
            .take()
            .expect("observed signal")
            .send(())
            .expect("observer");
        ctx.stop();
    }
}

impl Message<Data> for ControlProbe {
    type Reply = ();

    async fn handle(&mut self, _: Data, _: &mut Context<Self, Self::Reply>) {}
}

impl Message<Control> for ControlProbe {
    type Reply = ();

    async fn handle(&mut self, _: Control, _: &mut Context<Self, Self::Reply>) {}
}

#[tokio::test]
async fn full_256_entry_kameo_mailbox_requires_an_independent_interrupt_path() {
    let (custodian, _outbound, _inbound) = spawn_custodian();
    let observer = custodian.observer();
    let interrupt = CancellationToken::new();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (observed_tx, observed_rx) = oneshot::channel();
    let actor = ControlProbe::spawn_with_mailbox(
        ControlProbe {
            entered: Some(entered_tx),
            observed: Some(observed_tx),
            interrupt: interrupt.clone(),
        },
        mailbox::bounded(256),
    );
    actor
        .tell(BlockTurn)
        .try_send()
        .expect("start blocked turn");
    timeout(Duration::from_secs(5), entered_rx)
        .await
        .expect("actor entered handler")
        .expect("entered signal");
    for _ in 0..256 {
        actor.tell(Data).try_send().expect("fill data mailbox");
    }
    assert!(matches!(
        actor.tell(Control).try_send(),
        Err(SendError::MailboxFull(_))
    ));
    custodian.request_stop(StopReason::LeaseLost);
    assert_eq!(
        timeout(Duration::from_secs(5), observer.wait_until_joined())
            .await
            .expect("custodian stops without the full actor mailbox"),
        CustodianState::Joined
    );
    // The separate signal can be observed only because this handler polls it;
    // the experiment does not establish priority or preemption in Kameo.
    interrupt.cancel();
    timeout(Duration::from_secs(5), observed_rx)
        .await
        .expect("independent interrupt is observed")
        .expect("observed signal");
    timeout(Duration::from_secs(5), actor.wait_for_shutdown())
        .await
        .expect("actor stops");
}

#[derive(kameo::Actor)]
struct CustodianProbe {
    custodian: Option<IoCustodianHandle>,
    dropped: Option<oneshot::Sender<()>>,
}

impl Drop for CustodianProbe {
    fn drop(&mut self) {
        self.custodian.take();
        if let Some(dropped) = self.dropped.take() {
            let _ = dropped.send(());
        }
    }
}

struct StopCustodianActor;
struct PanicCustodianActor;

impl Message<StopCustodianActor> for CustodianProbe {
    type Reply = ();

    async fn handle(&mut self, _: StopCustodianActor, ctx: &mut Context<Self, Self::Reply>) {
        ctx.stop();
    }
}

impl Message<PanicCustodianActor> for CustodianProbe {
    type Reply = ();

    async fn handle(&mut self, _: PanicCustodianActor, _: &mut Context<Self, Self::Reply>) {
        panic!("injected Kameo actor panic");
    }
}

fn spawn_custodian() -> (
    IoCustodianHandle,
    WriteSender<u8>,
    tokio::sync::mpsc::Receiver<QueueItem<u8>>,
) {
    let (supervisor, outbound, inbound) = spawn_supervised_io(
        ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        futures::stream::pending::<Result<u8, std::convert::Infallible>>(),
        futures::sink::drain::<u8>(),
        TransportLimits::new(
            Limits::new(2, 8).expect("inbound limits"),
            Limits::new(2, 8).expect("outbound limits"),
        ),
        ShutdownProtocol::new(0_u8, |_: &std::convert::Infallible| false),
        |_| 1,
        |_| false,
    );
    (
        spawn_io_custodian(supervisor, Duration::from_millis(100)),
        outbound,
        inbound,
    )
}

#[tokio::test]
async fn actor_stop_drops_control_and_independent_custodian_joins_pumps() {
    let (custodian, _outbound, _inbound) = spawn_custodian();
    let connection_ref = custodian.connection_ref().clone();
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    inventory
        .try_retain(&custodian)
        .expect("retain observation independently of actor");
    let (dropped_tx, dropped_rx) = oneshot::channel();
    let actor = CustodianProbe::spawn(CustodianProbe {
        custodian: Some(custodian),
        dropped: Some(dropped_tx),
    });

    actor
        .tell(StopCustodianActor)
        .try_send()
        .expect("stop actor");
    timeout(Duration::from_secs(5), dropped_rx)
        .await
        .expect("actor is dropped")
        .expect("drop signal");
    let observer = inventory
        .observe(&connection_ref)
        .expect("reacquire after the actor dropped its only control handle");
    assert_eq!(
        timeout(Duration::from_secs(5), observer.wait_until_joined())
            .await
            .expect("independent custodian joins both pumps"),
        CustodianState::Joined
    );
    let summary = observer
        .terminal_summary()
        .expect("joined outcomes retained");
    assert!(summary.is_for(&connection_ref));
    assert_eq!(summary.inbound, InboundTerminal::Stopped);
    assert_eq!(summary.outbound, OutboundTerminal::Stopped);
}

#[tokio::test]
async fn actor_panic_drops_control_and_independent_custodian_joins_pumps() {
    let (custodian, _outbound, _inbound) = spawn_custodian();
    let connection_ref = custodian.connection_ref().clone();
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");
    inventory
        .try_retain(&custodian)
        .expect("retain observation independently of actor");
    let (dropped_tx, dropped_rx) = oneshot::channel();
    let actor = CustodianProbe::spawn(CustodianProbe {
        custodian: Some(custodian),
        dropped: Some(dropped_tx),
    });

    actor
        .tell(PanicCustodianActor)
        .try_send()
        .expect("deliver panic message");
    timeout(Duration::from_secs(5), dropped_rx)
        .await
        .expect("panicked actor is dropped")
        .expect("drop signal");
    let observer = inventory
        .observe(&connection_ref)
        .expect("reacquire after the actor dropped its only control handle");
    assert_eq!(
        timeout(Duration::from_secs(5), observer.wait_until_joined())
            .await
            .expect("independent custodian joins both pumps"),
        CustodianState::Joined
    );
}

#[tokio::test]
async fn custodian_inventory_refuses_duplicates_and_full_entries_and_prunes_only_joined() {
    let (first, _first_outbound, _first_inbound) = spawn_custodian();
    let (second, _second_outbound, _second_inbound) = spawn_custodian();
    let first_ref = first.connection_ref().clone();
    let inventory = IoCustodianInventory::new(1).expect("inventory capacity");

    assert_eq!(
        IoCustodianInventory::new(0).err(),
        Some(CustodianInventoryError::ZeroCapacity)
    );
    inventory.try_retain(&first).expect("first entry fits");
    assert_eq!(
        inventory.try_retain(&first),
        Err(CustodianInventoryError::DuplicateConnection)
    );
    assert_eq!(
        inventory.try_retain(&second),
        Err(CustodianInventoryError::Full)
    );
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory.prune_joined(), 0, "running entry remains indexed");

    first.request_stop(StopReason::Requested);
    let first_observer = inventory
        .observe(&first_ref)
        .expect("unfinished remains indexed");
    assert_eq!(
        timeout(Duration::from_secs(5), first_observer.wait_until_joined())
            .await
            .expect("join first pumps"),
        CustodianState::Joined
    );
    assert_eq!(inventory.prune_joined(), 1);
    assert!(inventory.observe(&first_ref).is_none());

    inventory
        .try_retain(&second)
        .expect("joined entry freed its slot");
    second.request_stop(StopReason::Requested);
    timeout(Duration::from_secs(5), second.wait_until_joined())
        .await
        .expect("join second pumps");
}

#[derive(Default)]
struct BlockingPollGate {
    released: Mutex<bool>,
    changed: Condvar,
}

impl BlockingPollGate {
    fn wait(&self) {
        let mut released = self.released.lock().expect("poll gate lock");
        while !*released {
            released = self.changed.wait(released).expect("poll gate wait");
        }
    }

    fn release(&self) {
        *self.released.lock().expect("poll gate lock") = true;
        self.changed.notify_all();
    }
}

struct ReleasePollGateOnDrop(Arc<BlockingPollGate>);

impl Drop for ReleasePollGateOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct BlockingFlushSink {
    entered: Option<oneshot::Sender<()>>,
    gate: Arc<BlockingPollGate>,
}

impl Sink<u8> for BlockingFlushSink {
    type Error = std::convert::Infallible;

    fn poll_ready(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, _: u8) -> Result<(), Self::Error> {
        Ok(())
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
            self.gate.wait();
            context.waker().wake_by_ref();
            return Poll::Pending;
        }
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn actor_drop_keeps_stalled_reserved_cleanup_discoverable_and_fences_successor() {
    let inventory = IoCustodianInventory::new(2).expect("inventory capacity");
    let old_connection = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let reservation = inventory
        .reserve(old_connection.clone())
        .expect("reserve old physical connection");
    let gate = Arc::new(BlockingPollGate::default());
    let _release_gate_on_drop = ReleasePollGateOnDrop(Arc::clone(&gate));
    let (flush_entered_tx, flush_entered_rx) = oneshot::channel();
    let (custodian, outbound, _inbound) = reservation
        .start_with(
            ReservedIoStartConfig::new(
                TransportLimits::new(
                    Limits::new(2, 8).expect("inbound limits"),
                    Limits::new(2, 8).expect("outbound limits"),
                ),
                ShutdownProtocol::new(0_u8, |_: &std::convert::Infallible| false),
                |_| 1,
                |_| false,
                Duration::from_millis(40),
            ),
            {
                let gate = Arc::clone(&gate);
                move |_| async move {
                    (
                        futures::stream::pending::<Result<u8, std::convert::Infallible>>(),
                        BlockingFlushSink {
                            entered: Some(flush_entered_tx),
                            gate,
                        },
                    )
                }
            },
        )
        .await
        .expect("reserved pumps start");
    outbound.try_send(1, 1).expect("admit stalled write");
    timeout(Duration::from_secs(5), flush_entered_rx)
        .await
        .expect("writer is stalled in poll_flush")
        .expect("flush entered signal");

    let (dropped_tx, dropped_rx) = oneshot::channel();
    let actor = CustodianProbe::spawn(CustodianProbe {
        custodian: Some(custodian),
        dropped: Some(dropped_tx),
    });
    actor
        .tell(StopCustodianActor)
        .try_send()
        .expect("stop actor owning the control handle");
    timeout(Duration::from_secs(5), dropped_rx)
        .await
        .expect("actor drops its controller")
        .expect("drop signal");

    let old_observer = timeout(Duration::from_secs(5), async {
        loop {
            if let Some(observer) = inventory.observe(&old_connection) {
                return observer;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("inventory observation survives actor death");
    timeout(Duration::from_secs(5), async {
        while old_observer.state() != CustodianState::CleanupPending {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stalled task remains discoverable at its deadline");
    assert_eq!(inventory.prune_joined(), 0);

    let successor_connection = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let successor_reservation = inventory
        .reserve(successor_connection.clone())
        .expect("successor has a distinct inventory identity");
    let (successor_custodian, _successor_outbound, _successor_inbound) = successor_reservation
        .start_with(
            ReservedIoStartConfig::new(
                TransportLimits::new(
                    Limits::new(2, 8).expect("inbound limits"),
                    Limits::new(2, 8).expect("outbound limits"),
                ),
                ShutdownProtocol::new(0_u8, |_: &std::convert::Infallible| false),
                |_| 1,
                |_| false,
                Duration::from_millis(100),
            ),
            |_| async {
                (
                    futures::stream::pending::<Result<u8, std::convert::Infallible>>(),
                    futures::sink::drain::<u8>(),
                )
            },
        )
        .await
        .expect("successor pumps start independently");
    let successor_observer = inventory
        .observe(&successor_connection)
        .expect("successor is indexed by its exact connection reference");
    assert!(old_observer.is_for(&old_connection));
    assert!(!old_observer.is_for(&successor_connection));

    gate.release();
    assert_eq!(
        timeout(Duration::from_secs(5), old_observer.wait_until_joined())
            .await
            .expect("release and join the old stalled pump"),
        CustodianState::Joined
    );
    assert_eq!(inventory.prune_joined(), 1);
    assert!(inventory.observe(&old_connection).is_none());
    assert!(inventory.observe(&successor_connection).is_some());

    successor_custodian.request_stop(StopReason::Requested);
    assert_eq!(
        timeout(
            Duration::from_secs(5),
            successor_observer.wait_until_joined()
        )
        .await
        .expect("join successor pumps"),
        CustodianState::Joined
    );
    assert_eq!(inventory.prune_joined(), 1);
}
