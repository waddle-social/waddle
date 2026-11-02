use kameo::actor::Spawn;
use kameo::error::SendError;
use kameo::mailbox;
use kameo::message::{Context, Message};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

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
