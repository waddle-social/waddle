//! Test-support-only connection actor proving single ownership of connection
//! protocol state and its exact reserved physical pump pair.
//!
//! This is a dark vertical slice. It does not replace the production
//! WebSocket loop, perform authentication or publish a registry binding. The
//! fixture begins with non-resumable XEP-0198 state so it can exercise the
//! existing handled-prefix tracker and the typed `<r/>` → `<a h='N'/>` path.
//! Its synthetic effect boundary does not perform production routing or
//! storage, and the fixture does not preserve parser metadata such as a
//! message's `xml:lang`; this proves sequencing and custody only.

use super::state::WsConnState;
use super::supervised_io::{
    AdmissionError, AuxiliaryCleanupGuard, CustodianInventoryError, CustodianState,
    IoCustodianHandle, IoCustodianInventory, IoCustodianObserver, QueueItem, ReservedIoStartConfig,
    ReservedStartError, StopReason, WriteSender,
};
use crate::server::routes::interpret::OrderedRelayInboundSequence;
use futures::future::BoxFuture;
use kameo::actor::{ActorRef, Spawn};
use kameo::mailbox;
use kameo::message::{Context, Message};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::sleep;
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::stream_management::{SmAck, SmRequest};
use waddle_xmpp::Stanza;

const MAX_HANDLED_INBOUND_OPERATIONS: usize = 256;

/// Typed input and output carried by the isolated connection transport.
/// Serialization stays in the caller's transport sink adapter.
#[derive(Debug, Clone)]
pub enum ConnectionWireFrame {
    /// A peer requested an XEP-0198 acknowledgment.
    RequestAcknowledgement(SmRequest),
    /// An XEP-0198 acknowledgment authored by the connection actor.
    Acknowledgement(SmAck),
    /// A parsed, countable peer stanza plus its raw serialized byte length.
    /// The dark fixture uses this for sequencing and custody; it does not
    /// carry all live parser metadata or imply routing parity.
    CountableStanza {
        stanza: Arc<Stanza>,
        wire_bytes: usize,
    },
    /// A malformed or non-stanza peer frame retained only long enough for
    /// the reader's byte bound to reject it. It never consumes an SM sequence.
    RejectedInbound { wire_bytes: usize },
    /// Transport close marker used by the supervised writer.
    Close,
}

impl ConnectionWireFrame {
    /// Count payload bytes using the typed stanza serializer at the transport
    /// boundary. Close frames have no XMPP payload.
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::RequestAcknowledgement(_) => SmRequest::to_xml().len(),
            Self::Acknowledgement(ack) => ack.to_xml().len(),
            Self::CountableStanza { wire_bytes, .. } | Self::RejectedInbound { wire_bytes } => {
                *wire_bytes
            }
            Self::Close => 0,
        }
    }

    /// Whether this is the WebSocket close marker used by the pump protocol.
    pub fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}

/// Typed disposition returned by the isolated slow external effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountableEffectDisposition {
    /// External work reached its handled boundary; the actor may complete h.
    Handled,
    /// The effect failed definitively; the connection stops with the h hole.
    Failed,
    /// The effect's outcome is uncertain; the connection stops with the h hole.
    Uncertain,
}

/// Input to the test-only external effect. The token carries the exact
/// connection, binding, sequence, and nonce generation that completion must
/// match.
#[derive(Debug, Clone)]
pub struct CountableInboundEffect {
    /// Parsed countable XMPP stanza.
    pub stanza: Arc<Stanza>,
    /// Exact actor-issued completion capability.
    pub sequence: ConnectionActorSequence,
}

/// Test-only effect runner. The actor never awaits this future.
pub type CountableStanzaEffect = Arc<
    dyn Fn(CountableInboundEffect) -> BoxFuture<'static, CountableEffectDisposition> + Send + Sync,
>;

/// Failure to start the reserved physical pumps for a connection actor.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionActorStartError {
    /// Exact physical-connection reservation failed before the factory ran.
    #[error(transparent)]
    Inventory(#[from] CustodianInventoryError),
    /// The independently owned pump pair did not complete startup.
    #[error(transparent)]
    Pumps(#[from] ReservedStartError),
}

/// Start the test-only actor after reserving the exact connection identity
/// minted in its real [`WsConnState`]. The pump factory cannot run before this
/// reservation succeeds.
pub async fn start_connection_actor<
    R,
    W,
    ReadError,
    WriteError,
    Size,
    IsClose,
    IsSendAfterClose,
    Factory,
    FactoryFuture,
>(
    inventory: &IoCustodianInventory,
    node_lease: NodeLeaseIdentity,
    lifecycle_handoff_deadline: Duration,
    config: ReservedIoStartConfig<ConnectionWireFrame, IsSendAfterClose, Size, IsClose>,
    factory: Factory,
) -> Result<ConnectionActorController, ConnectionActorStartError>
where
    R: futures::Stream<Item = Result<ConnectionWireFrame, ReadError>> + Unpin + Send + 'static,
    W: futures::Sink<ConnectionWireFrame, Error = WriteError> + Unpin + Send + 'static,
    ReadError: Send + 'static,
    WriteError: Send + 'static,
    Size: Fn(&ConnectionWireFrame) -> usize + Send + 'static,
    IsClose: Fn(&ConnectionWireFrame) -> bool + Send + 'static,
    IsSendAfterClose: Fn(&WriteError) -> bool + Send + Sync + 'static,
    Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
    FactoryFuture: Future<Output = (R, W)> + Send,
{
    let effect_runner: CountableStanzaEffect =
        Arc::new(|_| Box::pin(async { CountableEffectDisposition::Handled }));
    start_connection_actor_inner(
        inventory,
        node_lease,
        lifecycle_handoff_deadline,
        config,
        effect_runner,
        factory,
    )
    .await
}

/// Start the same test-only actor with an explicit slow external effect
/// boundary for parsed countable inbound stanzas.
pub async fn start_connection_actor_with_effect<
    R,
    W,
    ReadError,
    WriteError,
    Size,
    IsClose,
    IsSendAfterClose,
    Factory,
    FactoryFuture,
    Effect,
    EffectFuture,
>(
    inventory: &IoCustodianInventory,
    node_lease: NodeLeaseIdentity,
    lifecycle_handoff_deadline: Duration,
    config: ReservedIoStartConfig<ConnectionWireFrame, IsSendAfterClose, Size, IsClose>,
    factory: Factory,
    effect: Effect,
) -> Result<ConnectionActorController, ConnectionActorStartError>
where
    R: futures::Stream<Item = Result<ConnectionWireFrame, ReadError>> + Unpin + Send + 'static,
    W: futures::Sink<ConnectionWireFrame, Error = WriteError> + Unpin + Send + 'static,
    ReadError: Send + 'static,
    WriteError: Send + 'static,
    Size: Fn(&ConnectionWireFrame) -> usize + Send + 'static,
    IsClose: Fn(&ConnectionWireFrame) -> bool + Send + 'static,
    IsSendAfterClose: Fn(&WriteError) -> bool + Send + Sync + 'static,
    Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
    FactoryFuture: Future<Output = (R, W)> + Send,
    Effect: Fn(CountableInboundEffect) -> EffectFuture + Send + Sync + 'static,
    EffectFuture: Future<Output = CountableEffectDisposition> + Send + 'static,
{
    let effect_runner: CountableStanzaEffect = Arc::new(move |input| Box::pin(effect(input)));
    start_connection_actor_inner(
        inventory,
        node_lease,
        lifecycle_handoff_deadline,
        config,
        effect_runner,
        factory,
    )
    .await
}

async fn start_connection_actor_inner<
    R,
    W,
    ReadError,
    WriteError,
    Size,
    IsClose,
    IsSendAfterClose,
    Factory,
    FactoryFuture,
>(
    inventory: &IoCustodianInventory,
    node_lease: NodeLeaseIdentity,
    lifecycle_handoff_deadline: Duration,
    config: ReservedIoStartConfig<ConnectionWireFrame, IsSendAfterClose, Size, IsClose>,
    effect_runner: CountableStanzaEffect,
    factory: Factory,
) -> Result<ConnectionActorController, ConnectionActorStartError>
where
    R: futures::Stream<Item = Result<ConnectionWireFrame, ReadError>> + Unpin + Send + 'static,
    W: futures::Sink<ConnectionWireFrame, Error = WriteError> + Unpin + Send + 'static,
    ReadError: Send + 'static,
    WriteError: Send + 'static,
    Size: Fn(&ConnectionWireFrame) -> usize + Send + 'static,
    IsClose: Fn(&ConnectionWireFrame) -> bool + Send + 'static,
    IsSendAfterClose: Fn(&WriteError) -> bool + Send + Sync + 'static,
    Factory: FnOnce(&ConnectionRef) -> FactoryFuture + Send,
    FactoryFuture: Future<Output = (R, W)> + Send,
{
    let mut state = WsConnState::new_for_node_lease(node_lease);
    state
        .sm_state
        .enable("dark-connection-actor".to_owned(), false, None);
    let connection_ref = state.connection_ref().clone();
    let effect_capacity = config.inbound_limits().max_count();

    // This is the admission boundary: no factory, pump, or reaper exists
    // until the one physical identity owned by `state` has a slot.
    let mut reservation = inventory.reserve(connection_ref.clone())?;
    let auxiliary_cleanup = reservation.retain_until_auxiliary_cleanup();
    let (custodian, outbound, inbound) = match reservation.start_with(config, factory).await {
        Ok(started) => started,
        Err(error) => {
            let cleanup_task = tokio::spawn(async {});
            let _ = inventory.register_auxiliary_task(&connection_ref, cleanup_task);
            auxiliary_cleanup.complete();
            return Err(error.into());
        }
    };
    let lifecycle = Arc::new(ConnectionActorLifecycle::new());
    let stop_listener_lifecycle = Arc::clone(&lifecycle);
    let (effect_jobs_tx, effect_jobs_rx) = mpsc::channel(effect_capacity);
    let (effect_completed_tx, effect_completed_rx) = watch::channel(0usize);
    let (handled_h_tx, handled_h_rx) = watch::channel(0u32);
    let effect_owner = Arc::new(EffectOwnerShared {
        phase: Mutex::new(EffectOwnerPhase::Running),
        sender: effect_jobs_tx,
        completed: effect_completed_tx,
        admission_barrier: Mutex::new(None),
    });
    let stop_listener_effects = Arc::clone(&effect_owner);
    let stop_signal = custodian.stop_signal();
    custodian.set_stop_listener(Arc::new(move |reason| {
        stop_listener_lifecycle.observe_pump_stop(reason);
        stop_listener_effects.mark_unavailable();
    }));
    let io_control = Arc::new(ConnectionActorIoControl {
        custodian,
        inventory: inventory.clone(),
        connection_ref: connection_ref.clone(),
        lifecycle,
        stop_signal,
        effect_owner: Arc::clone(&effect_owner),
        handled_h_observation: handled_h_tx,
    });
    let effect_runner_for_owner = Arc::clone(&effect_runner);
    let actor = ConnectionActor::spawn_with_mailbox(
        ConnectionActor {
            state,
            io_control: Arc::clone(&io_control),
            outbound,
            effect_owner: Arc::clone(&effect_owner),
            lifecycle_handoff_deadline,
            next_handled_nonce: 1,
            handled_operations: HashMap::new(),
            highest_handled_operation_id: None,
            binding_epoch: 0,
            highest_binding_operation_id: None,
            pending_binding_attempt: None,
            active_binding_attempt: None,
            active_binding_confirmed: false,
            binding_handoff_task: None,
        },
        mailbox::bounded(256),
    );
    actor.wait_for_startup().await;

    let (forwarded_tx, forwarded_rx) = watch::channel(0usize);
    let mailbox_tracker = Arc::new(MailboxPayloadTracker::new());
    let pump_actor = actor.clone();
    let forward_tracker = Arc::clone(&mailbox_tracker);
    let input_forwarder = tokio::spawn(async move {
        forward_inbound_frames(inbound, pump_actor, forward_tracker, forwarded_tx).await;
    });
    let forwarder_abort = input_forwarder.abort_handle();
    let effect_owner_task = EffectOwnerTask {
        receiver: effect_jobs_rx,
        shared: Arc::clone(&effect_owner),
        stop_signal: io_control.stop_signal.clone(),
        actor: actor.downgrade(),
        io_control: Arc::downgrade(&io_control),
        effect_runner: effect_runner_for_owner,
    };
    let effect_owner_join = tokio::spawn(effect_owner_task.run());
    let effect_owner_abort = effect_owner_join.abort_handle();
    let cleanup_task = tokio::spawn(
        ConnectionActorCleanup {
            actor: actor.clone(),
            stop_signal: io_control.stop_signal.clone(),
            forwarder_abort,
            input_forwarder,
            effect_owner: effect_owner_join,
            effect_shared: Arc::clone(&effect_owner),
            mailbox_tracker,
            auxiliary_cleanup,
        }
        .run(),
    );
    if let Err(error) = inventory.register_auxiliary_task(&connection_ref, cleanup_task) {
        io_control.request_stop(StopReason::Requested);
        return Err(error.into());
    }

    Ok(ConnectionActorController {
        actor,
        io_control,
        inbound_forwarded: forwarded_rx,
        effect_completed: effect_completed_rx,
        handled_h_observation: handled_h_rx,
        effect_owner_abort,
    })
}

async fn forward_inbound_frames(
    mut inbound: mpsc::Receiver<QueueItem<ConnectionWireFrame>>,
    actor: ActorRef<ConnectionActor>,
    mailbox_tracker: Arc<MailboxPayloadTracker>,
    forwarded: watch::Sender<usize>,
) {
    let mut forwarded_count = 0usize;
    while let Some(item) = inbound.recv().await {
        if matches!(
            &item.value,
            ConnectionWireFrame::RequestAcknowledgement(_)
                | ConnectionWireFrame::CountableStanza { .. }
        ) {
            let item = TrackedQueueItem::new(item, Arc::clone(&mailbox_tracker));
            if actor
                .tell(ForwardedInboundFrame(item))
                .send()
                .await
                .is_err()
            {
                return;
            }
            forwarded_count = forwarded_count.saturating_add(1);
            forwarded.send_replace(forwarded_count);
        } else {
            drop(item);
        }
    }
}

struct MailboxPayloadTracker {
    count: Mutex<usize>,
    changed: watch::Sender<usize>,
}

impl MailboxPayloadTracker {
    fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            count: Mutex::new(0),
            changed,
        }
    }

    fn register(self: &Arc<Self>) -> MailboxPayloadGuard {
        let mut count = match self.count.lock() {
            Ok(count) => count,
            Err(poisoned) => poisoned.into_inner(),
        };
        *count = count.saturating_add(1);
        self.changed.send_replace(*count);
        MailboxPayloadGuard {
            tracker: Arc::clone(self),
        }
    }

    fn release(&self) {
        let mut count = match self.count.lock() {
            Ok(count) => count,
            Err(poisoned) => poisoned.into_inner(),
        };
        *count = count.saturating_sub(1);
        self.changed.send_replace(*count);
    }

    async fn wait_until_empty(&self) {
        let mut changed = self.changed.subscribe();
        loop {
            if *changed.borrow_and_update() == 0 {
                return;
            }
            if changed.changed().await.is_err() {
                return;
            }
        }
    }
}

struct MailboxPayloadGuard {
    tracker: Arc<MailboxPayloadTracker>,
}

impl Drop for MailboxPayloadGuard {
    fn drop(&mut self) {
        self.tracker.release();
    }
}

struct TrackedQueueItem {
    item: Option<QueueItem<ConnectionWireFrame>>,
    _mailbox_payload: MailboxPayloadGuard,
}

impl TrackedQueueItem {
    fn new(item: QueueItem<ConnectionWireFrame>, tracker: Arc<MailboxPayloadTracker>) -> Self {
        Self {
            item: Some(item),
            _mailbox_payload: tracker.register(),
        }
    }
}

struct EffectJob {
    item: QueueItem<ConnectionWireFrame>,
    token: ConnectionActorSequence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectOwnerPhase {
    Running,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectOwnerAdmissionError {
    Full,
    Closed,
    Unavailable,
}

struct EffectOwnerShared {
    phase: Mutex<EffectOwnerPhase>,
    sender: mpsc::Sender<EffectJob>,
    completed: watch::Sender<usize>,
    admission_barrier: Mutex<Option<EffectAdmissionBarrier>>,
}

struct EffectAdmissionBarrier {
    entered: oneshot::Sender<()>,
    barrier: Arc<ConnectionActorGateBarrier>,
}

struct EffectOwnerAdmission<'a> {
    _phase: MutexGuard<'a, EffectOwnerPhase>,
    permit: mpsc::OwnedPermit<EffectJob>,
}

impl EffectOwnerAdmission<'_> {
    fn send(self, job: EffectJob) {
        let Self { _phase, permit } = self;
        permit.send(job);
        drop(_phase);
    }
}

impl EffectOwnerShared {
    fn try_reserve(&self) -> Result<EffectOwnerAdmission<'_>, EffectOwnerAdmissionError> {
        let phase = match self.phase.lock() {
            Ok(phase) => phase,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *phase != EffectOwnerPhase::Running {
            return Err(EffectOwnerAdmissionError::Unavailable);
        }
        let permit = self
            .sender
            .clone()
            .try_reserve_owned()
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => EffectOwnerAdmissionError::Full,
                mpsc::error::TrySendError::Closed(_) => EffectOwnerAdmissionError::Closed,
            })?;
        Ok(EffectOwnerAdmission {
            _phase: phase,
            permit,
        })
    }

    fn mark_unavailable(&self) {
        let mut phase = match self.phase.lock() {
            Ok(phase) => phase,
            Err(poisoned) => poisoned.into_inner(),
        };
        *phase = EffectOwnerPhase::Unavailable;
    }

    fn record_completed(&self) {
        let next = self.completed.borrow().saturating_add(1);
        self.completed.send_replace(next);
    }

    fn take_admission_barrier(&self) -> Option<EffectAdmissionBarrier> {
        let mut barrier = match self.admission_barrier.lock() {
            Ok(barrier) => barrier,
            Err(poisoned) => poisoned.into_inner(),
        };
        barrier.take()
    }
}

struct EffectOwnerTask {
    receiver: mpsc::Receiver<EffectJob>,
    shared: Arc<EffectOwnerShared>,
    stop_signal: super::supervised_io::StopSignal,
    actor: kameo::actor::WeakActorRef<ConnectionActor>,
    io_control: std::sync::Weak<ConnectionActorIoControl>,
    effect_runner: CountableStanzaEffect,
}

impl EffectOwnerTask {
    async fn run(mut self) {
        loop {
            tokio::select! {
                biased;
                _ = self.stop_signal.cancelled() => break,
                job = self.receiver.recv() => match job {
                    Some(job) => {
                        Self::run_effect_job(
                            job,
                            Arc::clone(&self.shared),
                            self.stop_signal.clone(),
                            self.actor.clone(),
                            self.io_control.clone(),
                            Arc::clone(&self.effect_runner),
                        ).await;
                    }
                    None => break,
                }
            }
        }

        // Serialize producer closure with actor admission before closing the
        // receiver. A pre-acquired permit can no longer submit after this gate
        // changes, and Drop invokes stop only after releasing this mutex.
        self.shared.mark_unavailable();
        self.receiver.close();
        while let Some(job) = self.receiver.recv().await {
            drop(job);
        }
    }

    async fn run_effect_job(
        job: EffectJob,
        shared: Arc<EffectOwnerShared>,
        stop_signal: super::supervised_io::StopSignal,
        actor: kameo::actor::WeakActorRef<ConnectionActor>,
        io_control: std::sync::Weak<ConnectionActorIoControl>,
        effect_runner: CountableStanzaEffect,
    ) {
        let stanza = match &job.item.value {
            ConnectionWireFrame::CountableStanza { stanza, .. } => Arc::clone(stanza),
            _ => {
                if let Some(io_control) = io_control.upgrade() {
                    io_control.request_stop(StopReason::Requested);
                }
                drop(job);
                shared.record_completed();
                return;
            }
        };
        let effect = CountableInboundEffect {
            stanza,
            sequence: job.token.clone(),
        };
        let disposition = tokio::select! {
            biased;
            _ = stop_signal.cancelled() => None,
            disposition = (effect_runner)(effect) => Some(disposition),
        };

        match disposition {
            Some(CountableEffectDisposition::Handled) => {
                let Some(actor) = actor.upgrade() else {
                    if let Some(io_control) = io_control.upgrade() {
                        io_control.request_stop(StopReason::Requested);
                    }
                    drop(job);
                    shared.record_completed();
                    return;
                };
                let completion = actor.ask(CompleteHandledInbound(job.token.clone())).send();
                let accepted = tokio::select! {
                    biased;
                    _ = stop_signal.cancelled() => false,
                    result = completion => result.is_ok(),
                };
                if !accepted {
                    if let Some(io_control) = io_control.upgrade() {
                        io_control.request_stop(StopReason::Requested);
                    }
                }
            }
            Some(CountableEffectDisposition::Failed | CountableEffectDisposition::Uncertain)
            | None => {
                if let Some(io_control) = io_control.upgrade() {
                    io_control.request_stop(StopReason::Requested);
                }
            }
        }
        drop(job);
        shared.record_completed();
    }
}

impl Drop for EffectOwnerTask {
    fn drop(&mut self) {
        // Drop runs before the receiver field is destroyed. Marking the shared
        // admission phase first makes every later permit submission fail or
        // become a stopped, unhandled hole.
        self.shared.mark_unavailable();
        if let Some(io_control) = self.io_control.upgrade() {
            io_control.request_stop(StopReason::Requested);
        }
    }
}

struct ConnectionActorCleanup {
    actor: ActorRef<ConnectionActor>,
    stop_signal: super::supervised_io::StopSignal,
    forwarder_abort: AbortHandle,
    input_forwarder: JoinHandle<()>,
    effect_owner: JoinHandle<()>,
    effect_shared: Arc<EffectOwnerShared>,
    mailbox_tracker: Arc<MailboxPayloadTracker>,
    auxiliary_cleanup: AuxiliaryCleanupGuard,
}

impl ConnectionActorCleanup {
    async fn run(self) {
        self.stop_signal.cancelled().await;
        self.actor.kill();
        self.forwarder_abort.abort();
        self.effect_shared.mark_unavailable();
        let _ = self.input_forwarder.await;
        let _ = self.effect_owner.await;
        let _ = self.actor.wait_for_shutdown_result().await;
        self.mailbox_tracker.wait_until_empty().await;
        self.auxiliary_cleanup.complete();
    }
}

struct ConnectionActorIoControl {
    custodian: IoCustodianHandle,
    inventory: IoCustodianInventory,
    connection_ref: ConnectionRef,
    lifecycle: Arc<ConnectionActorLifecycle>,
    stop_signal: super::supervised_io::StopSignal,
    effect_owner: Arc<EffectOwnerShared>,
    handled_h_observation: watch::Sender<u32>,
}

impl ConnectionActorIoControl {
    fn observer(&self) -> Option<IoCustodianObserver> {
        self.inventory.observe(&self.connection_ref)
    }

    fn request_stop(&self, reason: StopReason) {
        self.custodian.request_stop(reason);
    }

    async fn expire_handled_operation(&self, operation_id: ConnectionActorOperationId) {
        if self.lifecycle.expire_handled_operation(operation_id).await {
            self.custodian.request_stop(StopReason::Requested);
        }
    }

    async fn expire_binding_handoff(&self, epoch: u64) {
        if self.lifecycle.expire_binding_handoff(epoch).await {
            self.custodian.request_stop(StopReason::Requested);
        }
    }
}

struct ConnectionActorLifecycle {
    state: Mutex<ConnectionActorLifecycleState>,
    stop_callback_count: AtomicU64,
    stop_callback_watch: watch::Sender<u64>,
}

#[derive(Default)]
struct ConnectionActorLifecycleState {
    stop_reason: Option<StopReason>,
    pending_handled_operations: HashSet<ConnectionActorOperationId>,
    pending_binding_handoff: Option<u64>,
}

impl ConnectionActorLifecycle {
    fn new() -> Self {
        let (stop_callback_watch, _) = watch::channel(0);
        Self {
            state: Mutex::new(ConnectionActorLifecycleState::default()),
            stop_callback_count: AtomicU64::new(0),
            stop_callback_watch,
        }
    }

    fn lock_running(&self) -> Result<ConnectionActorLifecycleGuard<'_>, ConnectionActorError> {
        let state = self.lock_state();
        if state.stop_reason.is_some() {
            return Err(ConnectionActorError::ConnectionStopping);
        }
        Ok(ConnectionActorLifecycleGuard { state })
    }

    fn lock_pending_handled(
        &self,
        operation_id: ConnectionActorOperationId,
    ) -> Result<ConnectionActorLifecycleGuard<'_>, ConnectionActorError> {
        let state = self.lock_state();
        if state.stop_reason.is_some() {
            return Err(ConnectionActorError::ConnectionStopping);
        }
        if !state.pending_handled_operations.contains(&operation_id) {
            return Err(ConnectionActorError::SequenceNotPending);
        }
        Ok(ConnectionActorLifecycleGuard { state })
    }

    fn request_stop(&self, reason: StopReason) -> bool {
        let mut state = self.lock_state();
        if state.stop_reason.is_some() {
            return false;
        }
        state.stop_reason = Some(reason);
        true
    }

    fn observe_pump_stop(&self, reason: StopReason) {
        let next = match self.stop_callback_count.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| current.checked_add(1),
        ) {
            Ok(previous) => previous.saturating_add(1),
            Err(current) => current,
        };
        self.stop_callback_watch.send_replace(next);
        self.request_stop(reason);
    }

    fn subscribe_stop_callback_count(&self) -> watch::Receiver<u64> {
        self.stop_callback_watch.subscribe()
    }

    async fn expire_handled_operation(&self, operation_id: ConnectionActorOperationId) -> bool {
        let mut state = self.lock_state_async().await;
        if state.stop_reason.is_some() || !state.pending_handled_operations.remove(&operation_id) {
            return false;
        }
        state.stop_reason = Some(StopReason::Requested);
        true
    }

    async fn expire_binding_handoff(&self, epoch: u64) -> bool {
        let mut state = self.lock_state_async().await;
        if state.stop_reason.is_some() || state.pending_binding_handoff != Some(epoch) {
            return false;
        }
        state.pending_binding_handoff = None;
        state.stop_reason = Some(StopReason::Requested);
        true
    }

    fn lock_state(&self) -> MutexGuard<'_, ConnectionActorLifecycleState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    async fn lock_state_async(&self) -> MutexGuard<'_, ConnectionActorLifecycleState> {
        loop {
            if let Some(state) = self.try_lock_state() {
                return state;
            }
            tokio::task::yield_now().await;
        }
    }

    fn try_lock_state(&self) -> Option<MutexGuard<'_, ConnectionActorLifecycleState>> {
        match self.state.try_lock() {
            Ok(state) => Some(state),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        }
    }
}

struct ConnectionActorLifecycleGuard<'a> {
    state: MutexGuard<'a, ConnectionActorLifecycleState>,
}

impl ConnectionActorLifecycleGuard<'_> {
    fn track_handled_operation(&mut self, operation_id: ConnectionActorOperationId) {
        self.state.pending_handled_operations.insert(operation_id);
    }

    fn finish_handled_operation(&mut self, operation_id: ConnectionActorOperationId) -> bool {
        self.state.pending_handled_operations.remove(&operation_id)
    }

    fn track_binding_handoff(&mut self, epoch: u64) {
        self.state.pending_binding_handoff = Some(epoch);
    }

    fn finish_binding_handoff(&mut self, epoch: u64) -> bool {
        if self.state.pending_binding_handoff == Some(epoch) {
            self.state.pending_binding_handoff = None;
            true
        } else {
            false
        }
    }

    fn retire_binding_handoff(&mut self, epoch: u64) {
        if self.state.pending_binding_handoff == Some(epoch) {
            self.state.pending_binding_handoff = None;
        }
    }
}

/// Addressable Kameo actor with the single mutable owner of connection state.
#[derive(kameo::Actor)]
pub struct ConnectionActor {
    state: WsConnState,
    io_control: Arc<ConnectionActorIoControl>,
    outbound: WriteSender<ConnectionWireFrame>,
    effect_owner: Arc<EffectOwnerShared>,
    lifecycle_handoff_deadline: Duration,
    next_handled_nonce: u64,
    handled_operations: HashMap<ConnectionActorOperationId, HandledOperation>,
    highest_handled_operation_id: Option<u64>,
    binding_epoch: u64,
    highest_binding_operation_id: Option<u64>,
    pending_binding_attempt: Option<ConnectionActorBindingAttempt>,
    active_binding_attempt: Option<ConnectionActorBindingAttempt>,
    active_binding_confirmed: bool,
    binding_handoff_task: Option<JoinHandle<()>>,
}

struct HandledOperation {
    token: ConnectionActorSequence,
    completed: bool,
    expiry_task: JoinHandle<()>,
}

impl Drop for ConnectionActor {
    fn drop(&mut self) {
        // Actor retirement is independent of mailbox traffic. The custodian
        // remains indexed and owns/join-reaps the actual pump handles.
        self.io_control.request_stop(StopReason::Requested);
        if let Some(task) = self.binding_handoff_task.take() {
            task.abort();
        }
        for operation in self.handled_operations.values() {
            operation.expiry_task.abort();
        }
    }
}

impl ConnectionActor {
    fn enqueue_acknowledgement(&self) -> Result<(), ConnectionActorError> {
        drop(self.io_control.lifecycle.lock_running()?);
        if !self.state.sm_state.enabled {
            return Err(ConnectionActorError::StreamManagementDisabled);
        }
        let ack = SmAck::new(self.state.sm_state.inbound_count());
        let byte_len = ack.to_xml().len();
        // WriteSender may synchronously request stop on overload. Release the
        // lifecycle gate after its preflight check so that stop callback can
        // enter without re-locking this actor's own gate. This is not an atomic
        // output-authority fence: a stop can race between preflight and queue
        // admission, which remains a live-cutover blocker.
        self.outbound
            .try_send(ConnectionWireFrame::Acknowledgement(ack), byte_len)
            .map(|_| ())
            .map_err(ConnectionActorError::from)
    }
}

/// Addressable actor handle plus an independent exact-identity stop and
/// observation path. The inventory observation survives actor retirement.
pub struct ConnectionActorController {
    actor: ActorRef<ConnectionActor>,
    io_control: Arc<ConnectionActorIoControl>,
    inbound_forwarded: watch::Receiver<usize>,
    effect_completed: watch::Receiver<usize>,
    handled_h_observation: watch::Receiver<u32>,
    effect_owner_abort: AbortHandle,
}

impl ConnectionActorController {
    /// Address of the sole actor that owns this connection's mutable state.
    pub fn actor(&self) -> &ActorRef<ConnectionActor> {
        &self.actor
    }

    /// Exact physical socket identity held by the actor and inventory.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.io_control.connection_ref
    }

    /// Reacquire independent observation by the exact connection identity.
    pub fn observer(&self) -> Option<IoCustodianObserver> {
        self.io_control.observer()
    }

    /// Interrupt physical pumps without relying on the actor data mailbox.
    pub fn request_stop(&self, reason: StopReason) {
        self.io_control.request_stop(reason);
    }

    /// First reason recorded by the independent pump owner.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.io_control.custodian.stop_reason()
    }

    /// Last actor-published handled-prefix count, retained after actor stop.
    pub fn handled_h(&self) -> u32 {
        *self.handled_h_observation.borrow()
    }

    /// Wait for the indexed custodian to join every pump it started.
    pub async fn wait_until_joined(&self) -> Option<CustodianState> {
        let observer = self.observer()?;
        Some(observer.wait_until_joined().await)
    }

    /// Wait until the inbound forwarder has placed this many retained queue
    /// items into the actor mailbox. This is a test-support synchronization
    /// point for proving that admission permits survive mailbox wait time.
    pub async fn wait_for_inbound_forwarded(&self, count: usize) -> bool {
        let mut forwarded = self.inbound_forwarded.clone();
        loop {
            if *forwarded.borrow_and_update() >= count {
                return true;
            }
            if forwarded.changed().await.is_err() {
                return false;
            }
        }
    }

    /// Wait until the serialized effect owner has completed this many jobs,
    /// including the actor completion request for handled outcomes.
    pub async fn wait_for_effects_completed(&self, count: usize) -> bool {
        let mut completed = self.effect_completed.clone();
        loop {
            if *completed.borrow_and_update() >= count {
                return true;
            }
            if completed.changed().await.is_err() {
                return false;
            }
        }
    }

    /// Inject abrupt effect-owner loss at the bounded admission handoff.
    pub fn abort_effect_owner_for_test(&self) {
        self.effect_owner_abort.abort();
    }

    /// Pause one actor admission after a work slot is reserved but before the
    /// handled sequence is assigned or the job is submitted.
    pub fn pause_effect_admission_for_test(
        &self,
        entered: oneshot::Sender<()>,
        barrier: Arc<ConnectionActorGateBarrier>,
    ) {
        let mut admission_barrier = match self.io_control.effect_owner.admission_barrier.lock() {
            Ok(admission_barrier) => admission_barrier,
            Err(poisoned) => poisoned.into_inner(),
        };
        *admission_barrier = Some(EffectAdmissionBarrier { entered, barrier });
    }

    /// Wait until a pump or control caller enters the shared StopSignal
    /// callback. This synchronization point is for lifecycle race tests.
    pub async fn wait_for_stop_signal_callbacks(&self, count: u64) -> bool {
        let mut callbacks = self.io_control.lifecycle.subscribe_stop_callback_count();
        loop {
            if *callbacks.borrow_and_update() >= count {
                return true;
            }
            if callbacks.changed().await.is_err() {
                return false;
            }
        }
    }
}

impl Drop for ConnectionActorController {
    fn drop(&mut self) {
        // The independently indexed cleanup task kills the retained actor,
        // aborts and joins the forwarder, joins the serialized effect owner,
        // and observes mailbox payload drops.
        self.io_control.request_stop(StopReason::Requested);
    }
}

/// Snapshot used to verify that the actor is the only state owner.
#[derive(Debug, Clone, PartialEq, Eq, kameo::Reply)]
pub struct ConnectionActorSnapshot {
    /// Exact physical connection identity.
    pub connection_ref: ConnectionRef,
    /// Current binding attempt, if a bind or resume attempt is active.
    pub binding_ref: Option<BindingRef>,
    /// Monotonic local binding transition epoch.
    pub binding_epoch: u64,
    /// XEP-0198 handled inbound prefix.
    pub handled_h: u32,
    /// Whether the isolated actor's SM state is active.
    pub stream_management_enabled: bool,
}

/// Opaque handle for one inbound operation reserved from the actor-owned
/// handled-prefix tracker.
#[derive(Debug, Clone, PartialEq, Eq, kameo::Reply)]
pub struct ConnectionActorSequence {
    connection_ref: ConnectionRef,
    binding_ref: Option<BindingRef>,
    sequence: OrderedRelayInboundSequence,
    operation_id: ConnectionActorOperationId,
    nonce: u64,
}

impl ConnectionActorSequence {
    /// XEP-0198 counter assigned to this operation. It may wrap independently
    /// of the unique completion-token nonce.
    pub fn sequence_number(&self) -> u32 {
        self.sequence.0
    }

    /// Monotonic actor-issued completion-token nonce independent of XEP-0198 h.
    pub fn nonce_generation(&self) -> u64 {
        self.nonce
    }
}

/// Caller-generated monotonic key for retrying a canceled actor request.
///
/// Use a fresh ID per logical operation and reuse it only to retry while that
/// operation remains unresolved. Each actor admits IDs in strictly increasing
/// order; once a higher ID is admitted, any unseen lower ID is permanently
/// rejected. This gives bounded replay rejection without a tombstone set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, kameo::Reply)]
pub struct ConnectionActorOperationId(u64);

impl ConnectionActorOperationId {
    /// Mint an operation key that may be replayed to recover an in-flight
    /// reservation after its original ask future is canceled.
    pub fn fresh() -> Option<Self> {
        static NEXT_OPERATION_ID: AtomicU64 = AtomicU64::new(1);
        NEXT_OPERATION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .ok()
            .map(Self)
    }
}

/// Opaque actor-issued permission to install one fresh binding attempt.
#[derive(Debug, Clone, PartialEq, Eq, kameo::Reply)]
pub struct ConnectionActorBindingAttempt {
    connection_ref: ConnectionRef,
    binding_ref: BindingRef,
    operation_id: ConnectionActorOperationId,
    epoch: u64,
}

impl ConnectionActorBindingAttempt {
    /// Exact binding identity the actor minted for this transition.
    pub fn binding_ref(&self) -> &BindingRef {
        &self.binding_ref
    }
}

/// Actor-side failures while sequencing a control event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionActorError {
    /// The control event arrived before the fixture's XEP-0198 state was enabled.
    #[error("XEP-0198 is not enabled on this connection")]
    StreamManagementDisabled,
    /// Binding identity belongs to a different physical connection.
    #[error("binding attempt belongs to a different physical connection")]
    BindingConnectionMismatch,
    /// Binding transition cannot strand handled work from the current stream.
    #[error("binding transition has unresolved handled work")]
    BindingHasPendingHandledInbound,
    /// A different binding is already active on this physical connection.
    #[error("a binding attempt is already active")]
    BindingAlreadyActive,
    /// An install token was superseded or belongs to a retired transition.
    #[error("binding installation token is stale")]
    StaleBindingTransition,
    /// Local binding epoch cannot safely advance further.
    #[error("binding transition epoch is exhausted")]
    BindingEpochExhausted,
    /// Completion belongs to a different physical connection.
    #[error("handled completion belongs to a different physical connection")]
    SequenceConnectionMismatch,
    /// Completion belongs to a retired or superseded binding attempt.
    #[error("handled completion belongs to a different binding attempt")]
    SequenceBindingMismatch,
    /// Completion token has already completed or was never reserved here.
    #[error("handled completion is not pending on this connection")]
    SequenceNotPending,
    /// A token's operation ID matches a current entry but its nonce does not.
    #[error("handled completion token generation is stale")]
    SequenceTokenMismatch,
    /// The bounded handled-prefix window is full.
    #[error("handled-prefix window is full")]
    HandledInboundFull,
    /// Local completion-token nonce cannot safely advance further.
    #[error("handled completion token nonce is exhausted")]
    SequenceNonceExhausted,
    /// Pump-owned operation identifier source is exhausted.
    #[error("handled operation identifier source is exhausted")]
    OperationIdExhausted,
    /// Caller replayed or delivered an unseen operation ID out of order.
    #[error("handled operation ID is not strictly increasing")]
    OperationIdNotIncreasing,
    /// The independent pump custodian has already been asked to stop.
    #[error("connection transport is stopping")]
    ConnectionStopping,
    /// The bounded writer queue rejected this output.
    #[error("outbound writer queue rejected the frame: {0:?}")]
    Admission(AdmissionError),
}

impl From<AdmissionError> for ConnectionActorError {
    fn from(error: AdmissionError) -> Self {
        Self::Admission(error)
    }
}

/// Reserve or recover one inbound handled-prefix sequence by stable caller ID.
pub struct ReserveHandledInbound {
    operation_id: ConnectionActorOperationId,
    barrier: Option<ReservationBarrier>,
}

struct ReservationBarrier {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

impl ReserveHandledInbound {
    /// Create a retryable request for one unique caller operation.
    pub fn new(operation_id: ConnectionActorOperationId) -> Self {
        Self {
            operation_id,
            barrier: None,
        }
    }

    /// Pause after the reservation is actor-owned but before the reply is
    /// delivered. This is a deterministic test-support cancellation barrier.
    pub fn with_reply_barrier(
        operation_id: ConnectionActorOperationId,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) -> Self {
        Self {
            operation_id,
            barrier: Some(ReservationBarrier { entered, release }),
        }
    }
}

impl Message<ReserveHandledInbound> for ConnectionActor {
    type Reply = Result<ConnectionActorSequence, ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: ReserveHandledInbound,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let result = self.reserve_handled_operation(msg.operation_id);
        if let Some(barrier) = msg.barrier {
            let _ = barrier.entered.send(());
            let _ = barrier.release.await;
        }
        result
    }
}

impl ConnectionActor {
    fn reserve_handled_operation(
        &mut self,
        operation_id: ConnectionActorOperationId,
    ) -> Result<ConnectionActorSequence, ConnectionActorError> {
        let lifecycle_owner = Arc::clone(&self.io_control.lifecycle);
        let mut lifecycle = lifecycle_owner.lock_running()?;
        self.reserve_handled_operation_locked(operation_id, &mut lifecycle, true)
    }

    fn reserve_handled_operation_locked(
        &mut self,
        operation_id: ConnectionActorOperationId,
        lifecycle: &mut ConnectionActorLifecycleGuard<'_>,
        caller_operation: bool,
    ) -> Result<ConnectionActorSequence, ConnectionActorError> {
        if let Some(operation) = self.handled_operations.get(&operation_id) {
            return Ok(operation.token.clone());
        }
        if caller_operation
            && self
                .highest_handled_operation_id
                .is_some_and(|highest| operation_id.0 <= highest)
        {
            return Err(ConnectionActorError::OperationIdNotIncreasing);
        }
        if self.state.sm_inbound_completion.outstanding_count() >= MAX_HANDLED_INBOUND_OPERATIONS {
            return Err(ConnectionActorError::HandledInboundFull);
        }
        let next_nonce = self
            .next_handled_nonce
            .checked_add(1)
            .ok_or(ConnectionActorError::SequenceNonceExhausted)?;
        let nonce = self.next_handled_nonce;
        let sequence = self
            .state
            .sm_inbound_completion
            .reserve(&self.state.sm_state);
        let token = ConnectionActorSequence {
            connection_ref: self.state.connection_ref().clone(),
            binding_ref: self.state.binding_ref.clone(),
            sequence,
            operation_id,
            nonce,
        };
        lifecycle.track_handled_operation(operation_id);
        let io_control = Arc::downgrade(&self.io_control);
        let deadline = self.lifecycle_handoff_deadline;
        let expiry_task = tokio::spawn(async move {
            sleep(deadline).await;
            if let Some(io_control) = io_control.upgrade() {
                io_control.expire_handled_operation(operation_id).await;
            }
        });
        self.handled_operations.insert(
            operation_id,
            HandledOperation {
                token: token.clone(),
                completed: false,
                expiry_task,
            },
        );
        self.next_handled_nonce = next_nonce;
        if caller_operation {
            self.highest_handled_operation_id = Some(operation_id.0);
        }
        Ok(token)
    }

    fn admit_countable_inbound(&mut self, mut item: TrackedQueueItem) {
        let Some(queue_item) = item.item.as_ref() else {
            return;
        };
        if !matches!(
            &queue_item.value,
            ConnectionWireFrame::CountableStanza { .. }
        ) {
            return;
        }

        let lifecycle_owner = Arc::clone(&self.io_control.lifecycle);
        let mut lifecycle = match lifecycle_owner.lock_running() {
            Ok(lifecycle) => lifecycle,
            Err(_) => return,
        };
        if !self.state.sm_state.enabled {
            drop(lifecycle);
            return;
        }
        let effect_owner = Arc::clone(&self.effect_owner);
        let admission = match effect_owner.try_reserve() {
            Ok(admission) => admission,
            Err(error) => {
                drop(lifecycle);
                if error != EffectOwnerAdmissionError::Unavailable {
                    self.io_control.request_stop(StopReason::InboundOverload);
                }
                return;
            }
        };
        if let Some(barrier) = effect_owner.take_admission_barrier() {
            barrier.barrier.wait(barrier.entered);
        }
        let operation_id = match ConnectionActorOperationId::fresh() {
            Some(operation_id) => operation_id,
            None => {
                drop(admission);
                drop(lifecycle);
                self.io_control.request_stop(StopReason::InboundOverload);
                return;
            }
        };
        let token = match self.reserve_handled_operation_locked(operation_id, &mut lifecycle, false)
        {
            Ok(token) => token,
            Err(error) => {
                drop(admission);
                drop(lifecycle);
                if matches!(
                    error,
                    ConnectionActorError::HandledInboundFull
                        | ConnectionActorError::SequenceNonceExhausted
                ) {
                    self.io_control.request_stop(StopReason::InboundOverload);
                }
                return;
            }
        };
        let Some(queue_item) = item.item.take() else {
            drop(admission);
            drop(lifecycle);
            self.io_control.request_stop(StopReason::Requested);
            return;
        };
        admission.send(EffectJob {
            item: queue_item,
            token,
        });
        drop(lifecycle);
        // The actor mailbox envelope is gone, while its QueueItem and both
        // permits now remain with the sequential effect owner.
    }
}

/// Complete one inbound operation; out-of-order completion cannot skip a hole.
pub struct CompleteHandledInbound(pub ConnectionActorSequence);

impl Message<CompleteHandledInbound> for ConnectionActor {
    type Reply = Result<u32, ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: CompleteHandledInbound,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.complete_handled_operation(msg.0, None)
    }
}

/// Synchronous barrier for deterministic lifecycle-gate race tests. The actor
/// pauses while holding the same gate used by completion and stop sources.
pub struct ConnectionActorGateBarrier {
    released: Mutex<bool>,
    release: Condvar,
}

impl ConnectionActorGateBarrier {
    /// Create a closed test barrier.
    pub fn new() -> Self {
        Self {
            released: Mutex::new(false),
            release: Condvar::new(),
        }
    }

    /// Let the actor continue past its lifecycle-gate barrier.
    pub fn release(&self) {
        let mut released = match self.released.lock() {
            Ok(released) => released,
            Err(poisoned) => poisoned.into_inner(),
        };
        *released = true;
        self.release.notify_all();
    }

    fn wait(&self, entered: oneshot::Sender<()>) {
        let _ = entered.send(());
        let mut released = match self.released.lock() {
            Ok(released) => released,
            Err(poisoned) => poisoned.into_inner(),
        };
        while !*released {
            released = match self.release.wait(released) {
                Ok(released) => released,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }
}

impl Default for ConnectionActorGateBarrier {
    fn default() -> Self {
        Self::new()
    }
}

/// Completion request paused after taking the shared lifecycle gate and
/// before advancing the handled prefix.
pub struct CompleteHandledInboundAtLifecycleGate {
    token: ConnectionActorSequence,
    entered: oneshot::Sender<()>,
    barrier: Arc<ConnectionActorGateBarrier>,
}

impl CompleteHandledInboundAtLifecycleGate {
    /// Create a request with a barrier the test releases explicitly.
    pub fn new(
        token: ConnectionActorSequence,
        entered: oneshot::Sender<()>,
        barrier: Arc<ConnectionActorGateBarrier>,
    ) -> Self {
        Self {
            token,
            entered,
            barrier,
        }
    }
}

impl Message<CompleteHandledInboundAtLifecycleGate> for ConnectionActor {
    type Reply = Result<u32, ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: CompleteHandledInboundAtLifecycleGate,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.complete_handled_operation(msg.token, Some((msg.entered, msg.barrier)))
    }
}

/// Completion request held before it tries to acquire the shared lifecycle
/// gate. A deadline can win while the operation has not begun committing.
pub struct CompleteHandledInboundBeforeLifecycleGate {
    token: ConnectionActorSequence,
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

impl CompleteHandledInboundBeforeLifecycleGate {
    /// Create a request with an asynchronous pre-gate barrier.
    pub fn new(
        token: ConnectionActorSequence,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) -> Self {
        Self {
            token,
            entered,
            release,
        }
    }
}

impl Message<CompleteHandledInboundBeforeLifecycleGate> for ConnectionActor {
    type Reply = Result<u32, ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: CompleteHandledInboundBeforeLifecycleGate,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _ = msg.entered.send(());
        let _ = msg.release.await;
        self.complete_handled_operation(msg.token, None)
    }
}

impl ConnectionActor {
    fn complete_handled_operation(
        &mut self,
        token: ConnectionActorSequence,
        barrier: Option<(oneshot::Sender<()>, Arc<ConnectionActorGateBarrier>)>,
    ) -> Result<u32, ConnectionActorError> {
        if token.connection_ref != *self.state.connection_ref() {
            return Err(ConnectionActorError::SequenceConnectionMismatch);
        }
        if token.binding_ref != self.state.binding_ref {
            return Err(ConnectionActorError::SequenceBindingMismatch);
        }
        let Some(operation) = self.handled_operations.get(&token.operation_id) else {
            if self
                .highest_handled_operation_id
                .is_some_and(|highest| token.operation_id.0 <= highest)
            {
                return Err(ConnectionActorError::SequenceTokenMismatch);
            }
            return Err(ConnectionActorError::SequenceNotPending);
        };
        if operation.token.nonce != token.nonce || operation.token.sequence != token.sequence {
            return Err(ConnectionActorError::SequenceTokenMismatch);
        }
        if operation.completed {
            return Err(ConnectionActorError::SequenceNotPending);
        }
        if !self.state.sm_inbound_completion.is_pending(token.sequence) {
            return Err(ConnectionActorError::SequenceNotPending);
        }
        let mut lifecycle = self
            .io_control
            .lifecycle
            .lock_pending_handled(token.operation_id)?;
        if let Some((entered, barrier)) = barrier {
            barrier.wait(entered);
        }
        if !lifecycle.finish_handled_operation(token.operation_id) {
            return Err(ConnectionActorError::SequenceNotPending);
        }
        self.state
            .sm_inbound_completion
            .complete(token.sequence, &mut self.state.sm_state);
        if let Some(operation) = self.handled_operations.get_mut(&token.operation_id) {
            operation.completed = true;
            operation.expiry_task.abort();
        }
        let tracker = &self.state.sm_inbound_completion;
        let settled_operation_ids = self
            .handled_operations
            .iter()
            .filter_map(|(operation_id, operation)| {
                (!tracker.is_tracked(operation.token.sequence)).then_some(*operation_id)
            })
            .collect::<Vec<_>>();
        for operation_id in settled_operation_ids {
            if let Some(operation) = self.handled_operations.remove(&operation_id) {
                operation.expiry_task.abort();
            }
        }
        let handled_h = self.state.sm_state.inbound_count();
        self.io_control
            .handled_h_observation
            .send_replace(handled_h);
        Ok(handled_h)
    }
}

/// Set the fixture's handled counter after all operation records have settled.
/// This only allows deterministic tests to reproduce u32 wrap aliases.
pub struct SeedInboundCounterForTest(pub u32);

impl Message<SeedInboundCounterForTest> for ConnectionActor {
    type Reply = Result<(), ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: SeedInboundCounterForTest,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _lifecycle = match self.io_control.lifecycle.lock_running() {
            Ok(lifecycle) => lifecycle,
            Err(error) => return Err(error),
        };
        if self.state.sm_inbound_completion.outstanding_count() > 0 {
            return Err(ConnectionActorError::BindingHasPendingHandledInbound);
        }
        for operation in self.handled_operations.values() {
            operation.expiry_task.abort();
        }
        self.handled_operations.clear();
        self.state.sm_inbound_completion.reset();
        self.state.sm_state.inbound_count = msg.0;
        self.io_control.handled_h_observation.send_replace(msg.0);
        Ok(())
    }
}

/// Answer the peer's XEP-0198 `<r/>` from this actor's current handled prefix.
pub struct RequestAcknowledgement;

/// Actor-level result for enqueueing a typed XEP-0198 acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub struct AcknowledgementResult(pub Result<(), ConnectionActorError>);

impl Message<RequestAcknowledgement> for ConnectionActor {
    type Reply = AcknowledgementResult;

    async fn handle(
        &mut self,
        _: RequestAcknowledgement,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        AcknowledgementResult(self.enqueue_acknowledgement())
    }
}

/// Forward one accepted inbound transport item while retaining its count and
/// byte permits through actor mailbox waiting and handling.
pub struct ForwardedInboundFrame(TrackedQueueItem);

impl Message<ForwardedInboundFrame> for ConnectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ForwardedInboundFrame,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let tracked = msg.0;
        let request_acknowledgement = tracked.item.as_ref().is_some_and(|item| {
            matches!(&item.value, ConnectionWireFrame::RequestAcknowledgement(_))
        });
        let countable_stanza = tracked
            .item
            .as_ref()
            .is_some_and(|item| matches!(&item.value, ConnectionWireFrame::CountableStanza { .. }));
        if request_acknowledgement {
            let _ = self.enqueue_acknowledgement();
        } else if countable_stanza {
            self.admit_countable_inbound(tracked);
        }
    }
}

/// Pause the actor mailbox on deterministic barriers in capacity tests.
pub struct PauseConnectionActor {
    /// Signaled after this message owns the actor execution slot.
    pub entered: oneshot::Sender<()>,
    /// Releases the actor to process its queued messages.
    pub release: oneshot::Receiver<()>,
}

impl Message<PauseConnectionActor> for ConnectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: PauseConnectionActor,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _ = msg.entered.send(());
        let _ = msg.release.await;
    }
}

/// Prepare one fresh binding attempt. Replaying the same operation ID while
/// preparation is pending returns the exact same actor-minted attempt.
pub struct PrepareBinding {
    operation_id: ConnectionActorOperationId,
    barrier: Option<PrepareBindingBarrier>,
}

struct PrepareBindingBarrier {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
    observed_attempt: oneshot::Sender<ConnectionActorBindingAttempt>,
}

impl PrepareBinding {
    /// Create a retryable prepare request.
    pub fn new(operation_id: ConnectionActorOperationId) -> Self {
        Self {
            operation_id,
            barrier: None,
        }
    }

    /// Pause after actor-owned preparation but before replying.
    pub fn with_reply_barrier(
        operation_id: ConnectionActorOperationId,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
        observed_attempt: oneshot::Sender<ConnectionActorBindingAttempt>,
    ) -> Self {
        Self {
            operation_id,
            barrier: Some(PrepareBindingBarrier {
                entered,
                release,
                observed_attempt,
            }),
        }
    }
}

impl Message<PrepareBinding> for ConnectionActor {
    type Reply = Result<ConnectionActorBindingAttempt, ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: PrepareBinding,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let result = self.prepare_binding(msg.operation_id);
        if let Some(barrier) = msg.barrier {
            let _ = barrier.entered.send(());
            if let Ok(attempt) = &result {
                let _ = barrier.observed_attempt.send(attempt.clone());
            }
            let _ = barrier.release.await;
        }
        result
    }
}

impl ConnectionActor {
    fn prepare_binding(
        &mut self,
        operation_id: ConnectionActorOperationId,
    ) -> Result<ConnectionActorBindingAttempt, ConnectionActorError> {
        let _lifecycle = self.io_control.lifecycle.lock_running()?;
        if let Some(active) = &self.active_binding_attempt {
            if active.operation_id == operation_id {
                return Ok(active.clone());
            }
        }
        if let Some(pending) = &self.pending_binding_attempt {
            if pending.operation_id == operation_id {
                return Ok(pending.clone());
            }
        }
        if self
            .highest_binding_operation_id
            .is_some_and(|highest| operation_id.0 <= highest)
        {
            return Err(ConnectionActorError::OperationIdNotIncreasing);
        }
        if self.active_binding_attempt.is_some() {
            if self.state.sm_inbound_completion.outstanding_count() > 0 {
                return Err(ConnectionActorError::BindingHasPendingHandledInbound);
            }
            return Err(ConnectionActorError::BindingAlreadyActive);
        }
        if self.state.binding_ref.is_some() {
            return Err(ConnectionActorError::BindingAlreadyActive);
        }
        if self.state.sm_inbound_completion.outstanding_count() > 0 {
            return Err(ConnectionActorError::BindingHasPendingHandledInbound);
        }
        let epoch = self
            .binding_epoch
            .checked_add(1)
            .ok_or(ConnectionActorError::BindingEpochExhausted)?;
        let attempt = ConnectionActorBindingAttempt {
            connection_ref: self.state.connection_ref().clone(),
            binding_ref: BindingRef::fresh(self.state.connection_ref().clone()),
            operation_id,
            epoch,
        };
        self.binding_epoch = epoch;
        self.highest_binding_operation_id = Some(operation_id.0);
        self.pending_binding_attempt = Some(attempt.clone());
        Ok(attempt)
    }
}

/// Install one actor-issued bind or resume attempt on this physical connection.
pub struct InstallBinding {
    attempt: ConnectionActorBindingAttempt,
    barrier: Option<ReservationBarrier>,
}

impl InstallBinding {
    /// Create an installation request for one prepared attempt.
    pub fn new(attempt: ConnectionActorBindingAttempt) -> Self {
        Self {
            attempt,
            barrier: None,
        }
    }

    /// Pause after installation but before replying. Retrying the same token
    /// must report the already-installed exact outcome.
    pub fn with_reply_barrier(
        attempt: ConnectionActorBindingAttempt,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) -> Self {
        Self {
            attempt,
            barrier: Some(ReservationBarrier { entered, release }),
        }
    }
}

impl Message<InstallBinding> for ConnectionActor {
    type Reply = Result<(), ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: InstallBinding,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let result = self.install_binding(msg.attempt);
        if let Some(barrier) = msg.barrier {
            let _ = barrier.entered.send(());
            let _ = barrier.release.await;
        }
        result
    }
}

impl ConnectionActor {
    fn install_binding(
        &mut self,
        attempt: ConnectionActorBindingAttempt,
    ) -> Result<(), ConnectionActorError> {
        let mut lifecycle = self.io_control.lifecycle.lock_running()?;
        if attempt.connection_ref != *self.state.connection_ref() {
            return Err(ConnectionActorError::BindingConnectionMismatch);
        }
        if self.active_binding_attempt.as_ref() == Some(&attempt)
            && self.state.binding_ref.as_ref() == Some(&attempt.binding_ref)
        {
            return Ok(());
        }
        if self.binding_epoch != attempt.epoch
            || self.pending_binding_attempt.as_ref() != Some(&attempt)
            || self.state.binding_ref.is_some()
        {
            return Err(ConnectionActorError::StaleBindingTransition);
        }
        if self.state.sm_inbound_completion.outstanding_count() > 0 {
            return Err(ConnectionActorError::BindingHasPendingHandledInbound);
        }
        lifecycle.track_binding_handoff(attempt.epoch);
        self.state.binding_ref = Some(attempt.binding_ref.clone());
        self.pending_binding_attempt = None;
        self.active_binding_attempt = Some(attempt.clone());
        self.active_binding_confirmed = false;
        let io_control = Arc::downgrade(&self.io_control);
        let deadline = self.lifecycle_handoff_deadline;
        self.binding_handoff_task = Some(tokio::spawn(async move {
            sleep(deadline).await;
            if let Some(io_control) = io_control.upgrade() {
                io_control.expire_binding_handoff(attempt.epoch).await;
            }
        }));
        Ok(())
    }
}

/// Confirm that the caller received the installed binding result. If this
/// handoff is lost, the exact physical connection stops at its configured
/// deadline instead of retaining an unacknowledged active binding forever.
pub struct ConfirmBindingInstall(pub ConnectionActorBindingAttempt);

impl Message<ConfirmBindingInstall> for ConnectionActor {
    type Reply = Result<(), ConnectionActorError>;

    async fn handle(
        &mut self,
        msg: ConfirmBindingInstall,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.confirm_binding_install(msg.0)
    }
}

impl ConnectionActor {
    fn confirm_binding_install(
        &mut self,
        attempt: ConnectionActorBindingAttempt,
    ) -> Result<(), ConnectionActorError> {
        let mut lifecycle = self.io_control.lifecycle.lock_running()?;
        if attempt.connection_ref != *self.state.connection_ref() {
            return Err(ConnectionActorError::BindingConnectionMismatch);
        }
        if self.active_binding_attempt.as_ref() != Some(&attempt)
            || self.state.binding_ref.as_ref() != Some(&attempt.binding_ref)
        {
            return Err(ConnectionActorError::StaleBindingTransition);
        }
        if self.active_binding_confirmed {
            return Ok(());
        }
        if !lifecycle.finish_binding_handoff(attempt.epoch) {
            return Err(ConnectionActorError::StaleBindingTransition);
        }
        self.active_binding_confirmed = true;
        if let Some(task) = self.binding_handoff_task.take() {
            task.abort();
        }
        Ok(())
    }
}

/// Retire one exact binding attempt. A stale completion cannot clear a later
/// binding on the same physical socket.
pub struct RetireBinding(pub BindingRef);

impl Message<RetireBinding> for ConnectionActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RetireBinding,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Ok(mut lifecycle) = self.io_control.lifecycle.lock_running() else {
            return false;
        };
        if self.state.binding_ref.as_ref() == Some(&msg.0)
            && msg.0.connection_ref() == self.state.connection_ref()
            && self.state.sm_inbound_completion.outstanding_count() == 0
        {
            let epoch = self.binding_epoch;
            let Some(next_epoch) = self.binding_epoch.checked_add(1) else {
                return false;
            };
            lifecycle.retire_binding_handoff(epoch);
            self.state.binding_ref = None;
            self.binding_epoch = next_epoch;
            self.active_binding_attempt = None;
            self.active_binding_confirmed = false;
            if let Some(task) = self.binding_handoff_task.take() {
                task.abort();
            }
            self.pending_binding_attempt = None;
            true
        } else {
            false
        }
    }
}

/// Read the current exact identity and handled-prefix state.
pub struct SnapshotConnection;

impl Message<SnapshotConnection> for ConnectionActor {
    type Reply = ConnectionActorSnapshot;

    async fn handle(
        &mut self,
        _: SnapshotConnection,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        ConnectionActorSnapshot {
            connection_ref: self.state.connection_ref().clone(),
            binding_ref: self.state.binding_ref.clone(),
            binding_epoch: self.binding_epoch,
            handled_h: self.state.sm_state.inbound_count(),
            stream_management_enabled: self.state.sm_state.enabled,
        }
    }
}

/// Deliberate panic used to prove that actor death leaves the pump owner
/// independently discoverable. This message is only exported by test support.
pub struct PanicConnectionActor;

impl Message<PanicConnectionActor> for ConnectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _: PanicConnectionActor,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        panic!("injected connection actor panic");
    }
}

/// Stop the actor normally, exercising its drop-time physical stop path.
pub struct StopConnectionActor;

impl Message<StopConnectionActor> for ConnectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _: StopConnectionActor,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        ctx.stop();
    }
}
