//! The Kameo actor that hosts a started connection's live loop.
//!
//! The actor is run on the connection's own task, so the task regains the
//! actor, and with it every resource of the connection, when the actor stops.
//! Its mailbox carries commands and lifecycle signals only: inbound frames are
//! still pulled from the socket by the loop, and outbound stanzas still wait
//! in the endpoint queue, where the send-window pause gates them.
//!
//! Kameo handles each mailbox message between two calls of the `next` hook,
//! and nothing else of the connection is polled meanwhile: not revocation,
//! shutdown or detach, not the socket, not a timer. A message handler on this
//! actor must therefore not await unbounded work. Work that can wait on the
//! peer, a store or another actor belongs in the turn loop, where every wait
//! races the control sources.

use super::{CommandTurn, LiveOwnerCtx, LiveSocketState, OwnerExit, TurnEvent};
use crate::server::routes::websocket::WebSocketState;
use crate::websocket_transport::LegacySocketAdmission;
use kameo::actor::{Actor, ActorRef, Spawn, WeakActorRef};
use kameo::error::{ActorStopReason, Infallible};
use kameo::mailbox::{self, MailboxReceiver, Signal};
use std::ops::ControlFlow;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Bound of the hosting actor's mailbox. A sender that finds it full is
/// refused or waits; the loop's control sources are polled ahead of it either
/// way.
const LIVE_CONNECTION_MAILBOX_CAPACITY: usize = 256;

/// What the hosting actor owns on behalf of the connection task besides the
/// live state.
pub(super) struct LiveOwner {
    pub(super) state: Arc<WebSocketState>,
    pub(super) admission: LegacySocketAdmission,
    pub(super) domain: String,
    pub(super) shutdown_token: CancellationToken,
    pub(super) caller_gone: CancellationToken,
}

impl LiveOwner {
    fn ctx(&self) -> LiveOwnerCtx<'_> {
        LiveOwnerCtx {
            state: &self.state,
            admission: &self.admission,
            admission_permit: self.admission.permit(),
            domain: &self.domain,
            shutdown_token: &self.shutdown_token,
            caller_gone: &self.caller_gone,
        }
    }
}

struct LiveConnectionActor {
    // Declared first so the connection's resources drop before the admission
    // that reserves its capacity. `on_stop` empties it on every stop.
    live: Option<Box<LiveSocketState>>,
    started: bool,
    // Cleared once every `ActorRef` is gone. The connection's lifetime follows
    // its socket, so a closed mailbox only stops being waited on.
    mailbox_open: bool,
    // Set while the last thing the loop produced was a mailbox signal. The
    // next wait then withholds the mailbox, so signals alternate with the
    // connection's own events instead of running ahead of them.
    signal_taken: bool,
    owner: LiveOwner,
    #[cfg(feature = "test-support")]
    test_tap: Option<Arc<crate::websocket_transport::LiveActorTestTap>>,
}

impl Actor for LiveConnectionActor {
    type Args = Self;
    type Error = Infallible;

    async fn on_start(args: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
        Ok(args)
    }

    /// Run turns of the live loop until the mailbox has a signal for Kameo to
    /// handle, or the loop ends. The loop ends only by retiring the
    /// connection; `None` then stops the actor normally, and `on_stop`
    /// settles the retirement.
    ///
    /// The turns run in a boxed future. The handlers' futures are large, and
    /// Kameo polls this hook through several nested futures of its own, each
    /// of which would otherwise carry that size in its stack frame.
    async fn next(
        &mut self,
        _: WeakActorRef<Self>,
        mailbox_rx: &mut MailboxReceiver<Self>,
    ) -> Result<Option<Signal<Self>>, Self::Error> {
        Ok(Box::pin(self.run_turns(mailbox_rx)).await)
    }

    /// Kameo calls this once, outside the part of the actor a kill can abort.
    /// A normal stop settles the connection. Any other stop is a panic or a
    /// kill that may have interrupted a handler, so nothing is settled: the
    /// live state is dropped, which releases the socket halves and then the
    /// transport cleanup guard before the connection task learns the outcome.
    async fn on_stop(
        &mut self,
        _: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        let Some(live) = self.live.take() else {
            return Ok(());
        };
        if matches!(reason, ActorStopReason::Normal) {
            // Boxed for the same reason as the turns in `next`.
            Box::pin(live.settle(&self.owner.ctx())).await;
        } else {
            drop(live);
        }
        Ok(())
    }
}

impl LiveConnectionActor {
    /// Connection events are handled here, with direct access to the live
    /// state, and never pass through the mailbox. Each turn polls the control
    /// sources before the mailbox, so returning one signal per turn cannot
    /// starve them, and the wait after a signal withholds the mailbox, so a
    /// mailbox that never runs dry cannot starve the socket, the outbound
    /// queue or the timers.
    async fn run_turns(&mut self, mailbox_rx: &mut MailboxReceiver<Self>) -> Option<Signal<Self>> {
        let live = self.live.as_mut()?;
        let ctx = self.owner.ctx();
        if !self.started {
            self.started = true;
            live.start(&ctx).await;
        }
        loop {
            #[cfg(feature = "test-support")]
            if let Some(tap) = &self.test_tap {
                // The message that woke a parked loop is not a signal the
                // test wants counted against the next wait.
                if tap.before_turn().await {
                    self.signal_taken = false;
                }
            }
            if let ControlFlow::Break(retire) = live.prepare_turn(&ctx).await {
                live.retire(&ctx, retire).await;
                return None;
            }
            let command_turn = if self.signal_taken {
                CommandTurn::Withheld
            } else {
                CommandTurn::Offered
            };
            self.signal_taken = false;
            let mailbox_open = self.mailbox_open;
            let commands = async {
                if mailbox_open {
                    mailbox_rx.recv().await
                } else {
                    std::future::pending().await
                }
            };
            match live.next_event(&ctx, commands, command_turn).await {
                TurnEvent::Command(Some(signal)) => {
                    self.signal_taken = true;
                    return Some(signal);
                }
                TurnEvent::Command(None) => self.mailbox_open = false,
                TurnEvent::Idle => {}
                TurnEvent::Loop(event) => {
                    #[cfg(feature = "test-support")]
                    if let Some(tap) = &self.test_tap {
                        tap.note_loop_event_handled();
                    }
                    let turn_kind = event.turn_kind();
                    let turn_started = std::time::Instant::now();
                    let flow = live.handle_event(&ctx, event).await;
                    waddle_xmpp::telemetry::authority::record_actor_turn(
                        turn_kind,
                        turn_started.elapsed(),
                    );
                    if let ControlFlow::Break(retire) = flow {
                        live.retire(&ctx, retire).await;
                        return None;
                    }
                }
            }
        }
    }
}

/// Run the live loop inside an actor on the calling task and report how it
/// ended. The actor reference is not handed to anything outside this task.
pub(super) async fn host(live: Box<LiveSocketState>, owner: LiveOwner) -> OwnerExit {
    let prepared = LiveConnectionActor::prepare_with_mailbox(mailbox::bounded(
        LIVE_CONNECTION_MAILBOX_CAPACITY,
    ));
    #[cfg(feature = "test-support")]
    let test_tap = owner
        .state
        .deps
        .app_state
        .websocket_admission_test_barriers
        .live_actor
        .clone();
    // The tap observes the first connection hosted for its state only.
    #[cfg(feature = "test-support")]
    let test_tap = test_tap.filter(|tap| tap.install(test_ops(prepared.actor_ref().clone())));
    let actor = LiveConnectionActor {
        live: Some(live),
        started: false,
        mailbox_open: true,
        signal_taken: false,
        owner,
        #[cfg(feature = "test-support")]
        test_tap,
    };
    // A panic while settling is not caught by Kameo: it unwinds out of `run`
    // here, past the rest of Kameo's stop sequence, to the connection task,
    // which fences it like any owner panic. Skipping that sequence loses
    // nothing while no actor reference leaves this task: nobody waits on the
    // shutdown result and the actor is registered nowhere.
    match prepared.run(actor).await {
        Ok((actor, ActorStopReason::Normal)) => {
            drop(actor);
            OwnerExit::Settled
        }
        Ok((actor, reason)) => {
            warn!(
                connection_ref = ?actor.owner.admission.connection_ref(),
                ?reason,
                "Live connection actor stopped without settling"
            );
            drop(actor);
            OwnerExit::Abandoned
        }
        Err(error) => {
            warn!(%error, "Live connection actor did not start");
            OwnerExit::Abandoned
        }
    }
}

/// A message with no effect, used by tests to observe mailbox progress. It is
/// the only message of this actor; the module documentation states what a
/// handler added here must not do.
#[cfg(feature = "test-support")]
struct LiveActorTestPing;

#[cfg(feature = "test-support")]
impl kameo::message::Message<LiveActorTestPing> for LiveConnectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _: LiveActorTestPing,
        _: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(tap) = &self.test_tap {
            tap.note_ping_handled();
        }
    }
}

#[cfg(feature = "test-support")]
fn test_ops(actor: ActorRef<LiveConnectionActor>) -> crate::websocket_transport::LiveActorTestOps {
    use futures::FutureExt;
    let flood = actor.clone();
    let ping = actor.clone();
    crate::websocket_transport::LiveActorTestOps {
        flood: Box::new(move |count| {
            (0..count)
                .filter(|_| flood.tell(LiveActorTestPing).try_send().is_ok())
                .count()
        }),
        ping: Box::new(move || {
            let actor = ping.clone();
            async move { actor.ask(LiveActorTestPing).await.is_ok() }.boxed()
        }),
        kill: Box::new(move || actor.kill()),
    }
}
