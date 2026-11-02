use kameo::actor::Spawn;
use kameo::error::SendError;
use kameo::mailbox;
use kameo::message::{Context, Message};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use waddle_server::test_support::supervised_io::{
    spawn_io_custodian, spawn_supervised_io, CustodianState, IoCustodianHandle,
    IoCustodianObserver, Limits, QueueItem, ShutdownProtocol, StopReason, TransportLimits,
    WriteSender,
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

async fn assert_custodian_joined(observer: &IoCustodianObserver) {
    assert_eq!(
        timeout(Duration::from_secs(5), observer.wait_until_joined())
            .await
            .expect("independent custodian joins both pumps"),
        CustodianState::Joined
    );
}

#[tokio::test]
async fn actor_stop_drops_control_and_independent_custodian_joins_pumps() {
    let (custodian, _outbound, _inbound) = spawn_custodian();
    let observer = custodian.observer();
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
    assert_custodian_joined(&observer).await;
}

#[tokio::test]
async fn actor_panic_drops_control_and_independent_custodian_joins_pumps() {
    let (custodian, _outbound, _inbound) = spawn_custodian();
    let observer = custodian.observer();
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
    assert_custodian_joined(&observer).await;
}
