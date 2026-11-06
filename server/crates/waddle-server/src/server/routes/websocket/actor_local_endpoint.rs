//! Dormant physical owner for one actor-side local endpoint.
//!
//! This owner is release compiled but is not selected by `/ws`. The cleanup
//! index retains accepted envelopes after the actor or startup caller exits
//! and until both physical pumps join and a terminal owner settles them.

use super::connection_actor_driver_task::{
    ConnectionActorDriverTask, ConnectionActorDriverTaskAckError, ConnectionActorDriverTaskCleanup,
    ConnectionActorDriverTicket,
};
#[cfg(feature = "test-support")]
use super::connection_actor_driver_task::{
    ConnectionActorDriverTestHandle, ConnectionActorDriverTestTicket, PhysicalLimits,
    PhysicalRecovery,
};
use super::supervised_io::{CustodianState, IoCustodianHandle, StopReason, TransportLimits};
use crate::websocket_transport::{
    ActorSocketPublicationAuthority, OutputError, SocketAdmission, SocketAdmissionError,
    WebSocketInput, WebSocketOutput,
};
use axum::extract::ws::{Message, WebSocket};
#[cfg(feature = "test-support")]
use futures::FutureExt;
use futures::{future::poll_fn, Sink, SinkExt};
use jid::FullJid;
use kameo::actor::ActorRef;
use kameo::error::SendError;
use std::collections::HashMap;
#[cfg(feature = "test-support")]
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(feature = "test-support")]
use tokio::sync::oneshot;
use waddle_xmpp::ownership::ConnectionRef;
#[cfg(feature = "test-support")]
use waddle_xmpp::registry::user_actor::{
    ConfirmProvisionalConnection, RegisterProvisionalConnectionIfExpected,
};
use waddle_xmpp::registry::user_actor::{UnregisterConnectionAndReportEmpty, UserActor};
use waddle_xmpp::registry::{
    ActorOutboundCustody, ActorOutboundId, ActorOutboundReceiver, ActorOutboundStage,
    ActorProvisionalConnectionEntry, ActorSocketOutboundEndpoint, ConnectionRegistry, DeliveryKind,
    LocalConnectionBindingId, OutboundStanza,
};

#[derive(Debug, thiserror::Error)]
pub enum ActorLocalEndpointError {
    #[error(transparent)]
    Socket(#[from] SocketAdmissionError),
    #[error("actor terminal inventory is full or already contains this socket")]
    InventoryFull,
    #[error("actor endpoint bounds must be positive")]
    InvalidBounds,
    #[error("the prior binding still owns an active endpoint or retained work")]
    PriorBindingActive,
    #[error("a driver task is already attached to this connection")]
    DriverAlreadyAttached,
    #[error("endpoint cleanup has started and cannot accept a driver task")]
    DriverCleanupStarted,
    #[error("driver task belongs to a different physical connection")]
    DriverConnectionMismatch,
    #[error("the connection driver task could not be started")]
    DriverTaskUnavailable,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorLocalPublishError {
    #[error(transparent)]
    Authority(#[from] SocketAdmissionError),
    #[error("local registry predecessor or binding did not match")]
    RegistryConflict,
    #[error("user actor mirror refused the expected predecessor")]
    MirrorConflict,
    #[error("user actor mirror outcome was not confirmed before the deadline")]
    MirrorUncertain,
    #[error("provisional activation lost exact ownership")]
    ActivationLost,
    #[error("startup caller cancelled before activation")]
    CallerCancelled,
    #[error("independent publication owner exited")]
    PublicationOwnerLost,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorLocalWriteError {
    #[error("outbound ID or physical owner does not match")]
    WrongOwner,
    #[error("accepted outbound ID is absent from custody")]
    MissingCustody,
    #[error("peer stanza requires the actor's recipient pass")]
    RecipientPassRequired,
    #[error("outbound XML serialization failed: {0}")]
    Serialize(#[from] xmpp_parsers::minidom::Error),
    #[error("outbound XML was not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)]
    Output(#[from] OutputError<axum::Error>),
}

struct CleanupRecord {
    custodian: IoCustodianHandle,
    driver_shutdown_grace: Duration,
    custodies: Mutex<Vec<Arc<ActorOutboundCustody>>>,
    publications: Mutex<PublicationState>,
    lifecycle: Mutex<CleanupLifecycle>,
    #[cfg(feature = "test-support")]
    join_cleanup_finished: AtomicBool,
    #[cfg(feature = "test-support")]
    publication_probe: Mutex<Option<Arc<ActorPublicationProbe>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupPhase {
    Accepting,
    Stopping,
    Joined,
}

struct CleanupLifecycle {
    phase: CleanupPhase,
    driver: Option<ConnectionActorDriverTaskCleanup>,
}

/// Test-only pause after the UserActor mirror accepts a provisional candidate
/// and before the registry can activate it.
#[cfg(feature = "test-support")]
pub struct ActorPublicationProbe {
    entered: tokio::sync::watch::Sender<bool>,
    release: tokio::sync::Semaphore,
}

#[cfg(feature = "test-support")]
impl Default for ActorPublicationProbe {
    fn default() -> Self {
        let (entered, _) = tokio::sync::watch::channel(false);
        Self {
            entered,
            release: tokio::sync::Semaphore::new(0),
        }
    }
}

#[cfg(feature = "test-support")]
impl ActorPublicationProbe {
    pub async fn wait_for_mirror(&self) {
        let mut entered = self.entered.subscribe();
        let _ = entered.wait_for(|entered| *entered).await;
    }

    pub fn release(&self) {
        self.release.add_permits(1);
    }

    async fn pause(&self) {
        self.entered.send_replace(true);
        if let Ok(permit) = self.release.acquire().await {
            permit.forget();
        }
    }
}

struct PublicationState {
    stopped: bool,
    tickets: Vec<PublicationTicket>,
}

#[derive(Clone)]
struct PublicationTicket {
    jid: FullJid,
    candidate: ActorProvisionalConnectionEntry,
    registry: Arc<ConnectionRegistry>,
    actor: ActorRef<UserActor>,
}

impl PublicationTicket {
    async fn retire(&self, deadline: Duration) {
        loop {
            let guard = self.registry.lock_bind(&self.jid).await;
            let _ = self
                .registry
                .retire_provisional_if_exact(&guard, &self.jid, &self.candidate);
            drop(guard);
            let mirrored = tokio::time::timeout(
                deadline,
                self.actor.ask(UnregisterConnectionAndReportEmpty {
                    jid: self.jid.clone(),
                    owner: Some(self.candidate.binding_id()),
                }),
            )
            .await;
            if matches!(
                mirrored,
                Ok(Ok(_)) | Ok(Err(SendError::ActorStopped | SendError::ActorNotRunning(_)))
            ) {
                let guard = self.registry.lock_bind(&self.jid).await;
                let _ =
                    self.registry
                        .retire_provisional_if_exact(&guard, &self.jid, &self.candidate);
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

impl CleanupRecord {
    fn seal_all(&self) {
        {
            let custodies = self
                .custodies
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            for custody in custodies.iter() {
                custody.seal();
            }
        }
        let tickets = self
            .publications
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .tickets
            .clone();
        for ticket in tickets {
            ticket.candidate.seal_binding();
        }
    }

    fn retain_publication(&self, ticket: PublicationTicket) -> Result<(), ActorLocalPublishError> {
        let mut state = self
            .publications
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if state.stopped {
            return Err(ActorLocalPublishError::CallerCancelled);
        }
        if state
            .tickets
            .iter()
            .any(|current| current.candidate.binding_id() == ticket.candidate.binding_id())
        {
            return Err(ActorLocalPublishError::RegistryConflict);
        }
        state.tickets.push(ticket);
        Ok(())
    }

    fn stop_publication(&self) {
        self.publications
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .stopped = true;
    }

    fn publication_stopped(&self) -> bool {
        self.publications
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .stopped
    }

    fn forget_publication(&self, owner: LocalConnectionBindingId) {
        self.publications
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .tickets
            .retain(|ticket| ticket.candidate.binding_id() != owner);
    }

    async fn retire_publications(&self, deadline: Duration) {
        loop {
            let next = self
                .publications
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                .tickets
                .first()
                .cloned();
            let Some(ticket) = next else { return };
            ticket.retire(deadline).await;
            self.forget_publication(ticket.candidate.binding_id());
        }
    }

    fn has_publications(&self) -> bool {
        !self
            .publications
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .tickets
            .is_empty()
    }

    fn contains_custody(&self, candidate: &Arc<ActorOutboundCustody>) -> bool {
        self.custodies
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .iter()
            .any(|current| Arc::ptr_eq(current, candidate))
    }

    fn attach_driver(
        &self,
        connection: &ConnectionRef,
        driver: ConnectionActorDriverTaskCleanup,
        settlement_callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), ActorLocalEndpointError> {
        if driver.connection_ref() != connection {
            return Err(ActorLocalEndpointError::DriverConnectionMismatch);
        }
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if lifecycle.phase != CleanupPhase::Accepting {
            return Err(ActorLocalEndpointError::DriverCleanupStarted);
        }
        if lifecycle.driver.is_some() {
            return Err(ActorLocalEndpointError::DriverAlreadyAttached);
        }
        #[cfg(feature = "test-support")]
        if driver.owns_physical() {
            // This fixture has no exclusive claim over actor custody. Keep
            // actor channels and the physical writer mutually exclusive under
            // the same lifecycle → custodies order used by channel creation.
            let custodies = self
                .custodies
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            if !custodies.is_empty() {
                return Err(ActorLocalEndpointError::PriorBindingActive);
            }
            if !self.custodian.enable_physical_stop_handoff() {
                return Err(ActorLocalEndpointError::DriverCleanupStarted);
            }
        }
        driver.set_settlement_callback(settlement_callback);
        driver.arm_shutdown_deadline(self.driver_shutdown_grace);
        lifecycle.driver = Some(driver);
        Ok(())
    }

    /// Atomically prevent late attachment and hand stop to the attached task.
    fn begin_shutdown(&self, reason: StopReason) -> Option<ConnectionActorDriverTaskCleanup> {
        let driver = {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            if lifecycle.phase == CleanupPhase::Accepting {
                lifecycle.phase = CleanupPhase::Stopping;
            }
            lifecycle.driver.clone()
        };
        if let Some(driver) = &driver {
            driver.request_stop(reason);
            driver.arm_shutdown_deadline(self.driver_shutdown_grace);
        }
        driver
    }

    fn driver(&self) -> Option<ConnectionActorDriverTaskCleanup> {
        self.lifecycle
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .driver
            .clone()
    }

    fn mark_joined(&self, pumps_joined: bool, driver_joined: bool) {
        if !pumps_joined || !driver_joined {
            return;
        }
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if lifecycle.phase == CleanupPhase::Stopping
            && lifecycle
                .driver
                .as_ref()
                .is_none_or(ConnectionActorDriverTaskCleanup::is_joined)
        {
            lifecycle.phase = CleanupPhase::Joined;
        }
    }

    fn is_joined(&self) -> bool {
        self.lifecycle
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .phase
            == CleanupPhase::Joined
    }

    fn driver_is_settled(&self) -> bool {
        self.driver().is_none_or(|driver| driver.is_settled())
    }
}

struct CleanupIndexState {
    records: HashMap<ConnectionRef, Arc<CleanupRecord>>,
    max_connections: usize,
}

/// App-owned bounded process-local terminal inventory. The serving app must
/// retain one clone for its lifetime; the actor and its join task only hold
/// temporary clones. After both pumps join, this is the retrieval authority
/// for accepted envelopes until explicit terminal settlement. It provides no
/// process-crash durability. Removing the final envelope releases its slot.
#[derive(Clone)]
pub struct ActorEndpointCleanupIndex {
    state: Arc<Mutex<CleanupIndexState>>,
}

impl ActorEndpointCleanupIndex {
    pub fn new(max_connections: usize) -> Option<Self> {
        (max_connections > 0).then(|| Self {
            state: Arc::new(Mutex::new(CleanupIndexState {
                records: HashMap::new(),
                max_connections,
            })),
        })
    }

    fn insert(
        &self,
        connection: ConnectionRef,
        custodian: IoCustodianHandle,
        driver_shutdown_grace: Duration,
    ) -> Result<Arc<CleanupRecord>, ActorLocalEndpointError> {
        let mut index = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if index.records.len() >= index.max_connections || index.records.contains_key(&connection) {
            return Err(ActorLocalEndpointError::InventoryFull);
        }
        let record = Arc::new(CleanupRecord {
            custodian,
            driver_shutdown_grace,
            custodies: Mutex::new(Vec::new()),
            publications: Mutex::new(PublicationState {
                stopped: false,
                tickets: Vec::new(),
            }),
            lifecycle: Mutex::new(CleanupLifecycle {
                phase: CleanupPhase::Accepting,
                driver: None,
            }),
            #[cfg(feature = "test-support")]
            join_cleanup_finished: AtomicBool::new(false),
            #[cfg(feature = "test-support")]
            publication_probe: Mutex::new(None),
        });
        index.records.insert(connection, Arc::clone(&record));
        Ok(record)
    }

    pub fn contains(&self, connection: &ConnectionRef) -> bool {
        self.state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .records
            .contains_key(connection)
    }

    #[cfg(feature = "test-support")]
    pub fn join_cleanup_finished_for_test(&self, connection: &ConnectionRef) -> bool {
        self.state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .records
            .get(connection)
            .is_some_and(|record| record.join_cleanup_finished.load(Ordering::Acquire))
    }

    /// Resolve a retained driver operation only through the exact physical
    /// connection record. The index does not create a second operation map.
    pub(super) fn lookup_driver(
        &self,
        connection: &ConnectionRef,
        id: u64,
    ) -> Option<ConnectionActorDriverTicket> {
        let record = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .records
            .get(connection)?
            .clone();
        record.driver()?.lookup(connection, id)
    }

    /// Acknowledge a terminal driver result through its app-owned connection
    /// record, then release the record if all actor and pump custody settled.
    pub(super) fn acknowledge_driver(
        &self,
        connection: &ConnectionRef,
        id: u64,
    ) -> Result<(), ConnectionActorDriverTaskAckError> {
        let record = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .records
            .get(connection)
            .cloned()
            .ok_or(ConnectionActorDriverTaskAckError::WrongOwner)?;
        let driver = record
            .driver()
            .ok_or(ConnectionActorDriverTaskAckError::WrongOwner)?;
        let ticket = driver
            .lookup(connection, id)
            .ok_or(ConnectionActorDriverTaskAckError::AlreadyAcknowledged)?;
        ticket.acknowledge()?;
        self.prune_if_settled(connection);
        Ok(())
    }

    #[cfg(feature = "test-support")]
    pub fn lookup_driver_for_test(
        &self,
        connection: &ConnectionRef,
        id: u64,
    ) -> Option<ConnectionActorDriverTestTicket> {
        self.lookup_driver(connection, id)
            .map(ConnectionActorDriverTestTicket::from_ticket)
    }

    #[cfg(feature = "test-support")]
    pub fn acknowledge_driver_for_test(&self, connection: &ConnectionRef, id: u64) -> bool {
        self.acknowledge_driver(connection, id).is_ok()
    }

    /// Transfer physical fixture custody only after the exact record has
    /// observed both pumps and its driver join.
    #[cfg(feature = "test-support")]
    pub fn take_joined_physical_for_test(
        &self,
        connection: &ConnectionRef,
    ) -> Option<PhysicalRecovery> {
        let record = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .records
            .get(connection)?
            .clone();
        if !record.is_joined() {
            return None;
        }
        let recovery = record.driver()?.take_joined_physical_recovery(connection)?;
        self.prune_if_settled(connection);
        Some(recovery)
    }

    /// An attempt still running or unjoined pumps cannot be settled by this
    /// index. The caller decides retry, retention or durable compensation.
    pub fn take_joined_terminal(
        &self,
        id: &ActorOutboundId,
    ) -> Option<(Arc<OutboundStanza>, ActorOutboundStage)> {
        let mut index = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        let record = index.records.get(id.connection())?.clone();
        if !record.is_joined() {
            return None;
        }
        let custodies = record
            .custodies
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        let result = custodies
            .iter()
            .find_map(|custody| custody.take_terminal(id));
        let empty = custodies
            .iter()
            .all(|custody| custody.retained_count() == 0);
        drop(custodies);
        if empty && !record.has_publications() && record.driver_is_settled() {
            index.records.remove(id.connection());
        }
        result
    }

    /// Enumerate the entire bounded terminal inventory after both pumps
    /// join. No actor-held ID is needed, including after mailbox loss.
    pub fn drain_joined_terminal(
        &self,
        connection: &ConnectionRef,
    ) -> Option<Vec<(ActorOutboundId, Arc<OutboundStanza>, ActorOutboundStage)>> {
        let mut index = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        let record = index.records.get(connection)?.clone();
        if !record.is_joined() {
            return None;
        }
        let custodies = record
            .custodies
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        let mut terminal = Vec::new();
        for custody in custodies.iter() {
            for id in custody.terminal_ids()? {
                if let Some((outbound, stage)) = custody.take_terminal(&id) {
                    terminal.push((id, outbound, stage));
                }
            }
        }
        let empty = custodies
            .iter()
            .all(|custody| custody.retained_count() == 0);
        drop(custodies);
        if empty && !record.has_publications() && record.driver_is_settled() {
            index.records.remove(connection);
        }
        Some(terminal)
    }

    fn prune_if_settled(&self, connection: &ConnectionRef) {
        prune_cleanup_index_state(&self.state, connection);
    }
}

fn prune_cleanup_index_state(state: &Mutex<CleanupIndexState>, connection: &ConnectionRef) {
    let mut index = state
        .lock()
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
    if let Some(record) = index.records.get(connection) {
        let settled = record.is_joined()
            && !record.has_publications()
            && record.driver_is_settled()
            && record
                .custodies
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                .iter()
                .all(|custody| custody.retained_count() == 0);
        if settled {
            index.records.remove(connection);
        }
    }
}

/// The actor holds this object, while the cleanup index independently keeps
/// accepted work and the custodian until both pumps join.
pub struct ActorLocalEndpointOwner {
    authority: ActorSocketPublicationAuthority,
    cleanup: Arc<CleanupRecord>,
    index: ActorEndpointCleanupIndex,
    max_count: usize,
    max_payload_bytes: usize,
}

impl ActorLocalEndpointOwner {
    #[cfg(feature = "test-support")]
    pub fn install_publication_probe(&self, probe: Arc<ActorPublicationProbe>) {
        *self
            .cleanup
            .publication_probe
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = Some(probe);
    }

    /// `index` must be held independently by process-lifetime app state. A
    /// caller-owned temporary index would make post-join terminal work
    /// undiscoverable after the owner and join task exit.
    pub(crate) async fn start(
        admission: SocketAdmission,
        socket: WebSocket,
        limits: TransportLimits,
        retirement_deadline: Duration,
        index: &ActorEndpointCleanupIndex,
        max_count: usize,
        max_payload_bytes: usize,
    ) -> Result<
        (
            Self,
            WebSocketInput<axum::Error>,
            WebSocketOutput<axum::Error>,
        ),
        ActorLocalEndpointError,
    > {
        if max_count == 0 || max_payload_bytes == 0 {
            return Err(ActorLocalEndpointError::InvalidBounds);
        }
        let authority = admission.actor_publication_authority();
        let connection = authority.connection_ref().clone();
        let (custodian, input, output) = admission
            .start_socket(socket, limits, retirement_deadline)
            .await?;
        let cleanup = match index.insert(connection.clone(), custodian.clone(), retirement_deadline)
        {
            Ok(cleanup) => cleanup,
            Err(error) => {
                custodian.request_stop(StopReason::Requested);
                return Err(error);
            }
        };
        let stop_cleanup = Arc::downgrade(&cleanup);
        cleanup
            .custodian
            .set_retirement_listener(Arc::new(move |reason| {
                if let Some(cleanup) = stop_cleanup.upgrade() {
                    cleanup.begin_shutdown(reason);
                }
            }));
        let driver_shutdown_grace = retirement_deadline;
        let joined_cleanup = Arc::clone(&cleanup);
        let joined_index = index.clone();
        let joined_connection = connection.clone();
        tokio::spawn(async move {
            let state: CustodianState = joined_cleanup.custodian.wait_until_joined().await;
            let driver = joined_cleanup.begin_shutdown(StopReason::Requested);
            joined_cleanup.stop_publication();
            joined_cleanup.seal_all();
            let driver_joined = if let Some(driver) = driver {
                driver.shutdown_and_join(driver_shutdown_grace).await;
                driver.is_joined()
            } else {
                true
            };
            joined_cleanup.mark_joined(state == CustodianState::Joined, driver_joined);
            joined_cleanup
                .retire_publications(retirement_deadline)
                .await;
            joined_index.prune_if_settled(&joined_connection);
            #[cfg(feature = "test-support")]
            joined_cleanup
                .join_cleanup_finished
                .store(true, Ordering::Release);
        });
        Ok((
            Self {
                authority,
                cleanup,
                index: index.clone(),
                max_count,
                max_payload_bytes,
            },
            input,
            output,
        ))
    }

    pub fn connection_ref(&self) -> &ConnectionRef {
        self.authority.connection_ref()
    }

    /// Attach the one dormant actor driver to this physical endpoint's
    /// existing cleanup record. The supplied state must carry this exact
    /// transport identity; task and I/O custody remain under one index entry.
    pub(super) fn attach_driver_task(
        &self,
        state: super::state::WsConnState,
        websocket: Arc<super::WebSocketState>,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Result<ConnectionActorDriverTask, ActorLocalEndpointError> {
        if state.connection_ref != *self.connection_ref() {
            return Err(ActorLocalEndpointError::DriverConnectionMismatch);
        }
        let (task, release) = ConnectionActorDriverTask::spawn_parked(
            state,
            websocket,
            self.authority.admission_permit(),
            shutdown,
        )
        .map_err(|_| ActorLocalEndpointError::DriverTaskUnavailable)?;
        self.attach_parked_driver(task, release)
    }

    fn attach_parked_driver(
        &self,
        mut task: ConnectionActorDriverTask,
        release: tokio::sync::oneshot::Sender<()>,
    ) -> Result<ConnectionActorDriverTask, ActorLocalEndpointError> {
        let index_state = Arc::downgrade(&self.index.state);
        let connection = self.connection_ref().clone();
        let settlement_callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(index_state) = index_state.upgrade() {
                prune_cleanup_index_state(&index_state, &connection);
            }
        });
        self.cleanup.attach_driver(
            self.connection_ref(),
            task.cleanup_handle(),
            settlement_callback,
        )?;
        task.mark_app_owned();
        if release.send(()).is_err() {
            self.request_stop(StopReason::Requested);
            return Err(ActorLocalEndpointError::DriverTaskUnavailable);
        }
        Ok(task)
    }

    /// Attach one test-only physical arbiter to this exact cleanup owner.
    #[cfg(feature = "test-support")]
    pub fn attach_physical_driver_for_test(
        &self,
        websocket: Arc<super::WebSocketState>,
        shutdown: tokio_util::sync::CancellationToken,
        input: WebSocketInput<axum::Error>,
        output: WebSocketOutput<axum::Error>,
        limits: PhysicalLimits,
    ) -> Result<ConnectionActorDriverTestHandle, ActorLocalEndpointError> {
        if input.connection_ref() != self.connection_ref()
            || output.connection_ref() != self.connection_ref()
        {
            return Err(ActorLocalEndpointError::DriverConnectionMismatch);
        }
        let mut state =
            super::state::WsConnState::new_for_connection_ref(self.connection_ref().clone());
        let stream = waddle_xmpp::pending_delivery::SmSessionId::new(format!(
            "dark-continuation-{}",
            uuid::Uuid::new_v4()
        ));
        state.sm_state.enable(stream.to_string(), true, Some(300));
        let first = state.sm_inbound_completion.reserve(&state.sm_state);
        let second = state.sm_inbound_completion.reserve(&state.sm_state);
        state
            .sm_inbound_completion
            .complete(second, &mut state.sm_state);
        let parts = super::connection_actor_driver_task::physical::PhysicalParts::new(
            input,
            output,
            self.cleanup.custodian.clone(),
            limits,
            first,
            stream,
        )
        .ok_or(ActorLocalEndpointError::InvalidBounds)?;
        let (task, release) = ConnectionActorDriverTask::spawn_physical_parked(
            state,
            websocket,
            self.authority.admission_permit(),
            shutdown,
            parts,
        )
        .map_err(|_| ActorLocalEndpointError::DriverTaskUnavailable)?;
        self.attach_parked_driver(task, release)
            .map(ConnectionActorDriverTestHandle::from_attached)
    }

    #[cfg(feature = "test-support")]
    pub fn attach_driver_task_for_test(
        &self,
        websocket: Arc<super::WebSocketState>,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Result<ConnectionActorDriverTestHandle, ActorLocalEndpointError> {
        let state =
            super::state::WsConnState::new_for_connection_ref(self.connection_ref().clone());
        self.attach_driver_task(state, websocket, shutdown)
            .map(ConnectionActorDriverTestHandle::from_attached)
    }

    /// A detached startup task owns the whole registry→mirror→activation
    /// sequence. If its caller disappears, it seals and retires the exact
    /// candidate; the physical cleanup record is a second retained fallback.
    #[cfg(feature = "test-support")]
    pub async fn publish_binding(
        &self,
        registry: Arc<ConnectionRegistry>,
        actor: ActorRef<UserActor>,
        jid: FullJid,
        candidate: ActorProvisionalConnectionEntry,
        expected: Option<LocalConnectionBindingId>,
        deadline: Duration,
    ) -> Result<(), ActorLocalPublishError> {
        let Some(custody) = candidate.actor_custody() else {
            return Err(ActorLocalPublishError::RegistryConflict);
        };
        if candidate.connection_ref() != self.connection_ref()
            || !self.cleanup.contains_custody(&custody)
        {
            return Err(ActorLocalPublishError::RegistryConflict);
        }
        let ticket = PublicationTicket {
            jid: jid.clone(),
            candidate: candidate.clone(),
            registry,
            actor,
        };
        self.cleanup.retain_publication(ticket.clone())?;
        let cleanup = Arc::clone(&self.cleanup);
        let authority = self.authority.clone();
        let index = self.index.clone();
        let connection = self.connection_ref().clone();
        let (result_tx, result_rx) = oneshot::channel();
        tokio::spawn(async move {
            let result = AssertUnwindSafe(run_publication(
                &authority, &cleanup, &ticket, expected, deadline, &result_tx,
            ))
            .catch_unwind()
            .await
            .unwrap_or(Err(ActorLocalPublishError::PublicationOwnerLost));
            if result.is_err() {
                ticket.candidate.seal_binding();
                let _ = result_tx.send(result);
                ticket.retire(deadline).await;
                cleanup.forget_publication(ticket.candidate.binding_id());
                index.prune_if_settled(&connection);
            } else if result_tx.send(result).is_err() {
                ticket.candidate.seal_binding();
                ticket.retire(deadline).await;
                cleanup.forget_publication(ticket.candidate.binding_id());
                index.prune_if_settled(&connection);
            }
        });
        result_rx
            .await
            .map_err(|_| ActorLocalPublishError::PublicationOwnerLost)?
    }

    /// A new binding gets a fresh queue. Earlier queues must be sealed and
    /// empty before their space can be reused on the same physical socket.
    pub fn new_binding_channel(
        &mut self,
    ) -> Result<(ActorSocketOutboundEndpoint, ActorOutboundReceiver), ActorLocalEndpointError> {
        let lifecycle = self
            .cleanup
            .lifecycle
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        #[cfg(feature = "test-support")]
        if lifecycle
            .driver
            .as_ref()
            .is_some_and(ConnectionActorDriverTaskCleanup::owns_physical)
        {
            return Err(ActorLocalEndpointError::PriorBindingActive);
        }
        if lifecycle.phase != CleanupPhase::Accepting {
            return Err(ActorLocalEndpointError::PriorBindingActive);
        }
        if self.cleanup.publication_stopped() || self.cleanup.has_publications() {
            return Err(ActorLocalEndpointError::PriorBindingActive);
        }
        let mut custodies = self
            .cleanup
            .custodies
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        // Stop first marks the publication state, then seals the custody
        // list. Recheck while holding that list so a join cannot finish its
        // seal pass before a fresh channel is inserted.
        if self.cleanup.publication_stopped() || self.cleanup.has_publications() {
            return Err(ActorLocalEndpointError::PriorBindingActive);
        }
        if custodies
            .iter()
            .any(|custody| !custody.is_sealed() || custody.retained_count() != 0)
        {
            return Err(ActorLocalEndpointError::PriorBindingActive);
        }
        custodies.clear();
        let (endpoint, receiver, custody) = ActorSocketOutboundEndpoint::channel(
            self.connection_ref().clone(),
            self.max_count,
            self.max_payload_bytes,
        )
        .ok_or(ActorLocalEndpointError::InvalidBounds)?;
        custodies.push(custody);
        drop(custodies);
        drop(lifecycle);
        Ok((endpoint, receiver))
    }

    pub fn request_stop(&self, reason: StopReason) {
        self.cleanup.stop_publication();
        self.cleanup.seal_all();
        self.cleanup.begin_shutdown(reason);
        self.cleanup.custodian.request_stop(reason);
    }

    /// The actor moves only an ID through its mailbox. This writer derives
    /// one frame while custody keeps the complete original envelope. Its
    /// separately bounded WebSocket output may hold a serialized copy.
    pub async fn write_next_direct(
        &self,
        receiver: &mut ActorOutboundReceiver,
        output: &mut WebSocketOutput<axum::Error>,
    ) -> Result<Option<ActorOutboundId>, ActorLocalWriteError> {
        if receiver.custody().connection() != self.connection_ref()
            || output.connection_ref() != self.connection_ref()
            || !self.cleanup.contains_custody(receiver.custody())
        {
            return Err(ActorLocalWriteError::WrongOwner);
        }
        let Some(id) = receiver.recv().await else {
            return Ok(None);
        };
        if id.connection() != self.connection_ref() {
            return Err(ActorLocalWriteError::WrongOwner);
        }
        let custody = receiver.custody();
        let outbound = custody
            .snapshot(&id)
            .ok_or(ActorLocalWriteError::MissingCustody)?;
        if outbound.kind != DeliveryKind::DirectFrame {
            return Err(ActorLocalWriteError::RecipientPassRequired);
        }
        let mut encoded = Vec::new();
        outbound.stanza.to_element().write_to(&mut encoded)?;
        let text = String::from_utf8(encoded)?;
        drop(outbound);
        let frame =
            output.actor_data_frame(Message::Text(text.into()), Arc::clone(custody), id.clone())?;
        poll_fn(|cx| Pin::new(&mut *output).poll_ready(cx)).await?;
        Pin::new(&mut *output).start_send(frame)?;
        output.flush().await?;
        Ok(Some(id))
    }
}

#[cfg(feature = "test-support")]
async fn run_publication(
    authority: &ActorSocketPublicationAuthority,
    cleanup: &CleanupRecord,
    ticket: &PublicationTicket,
    expected: Option<LocalConnectionBindingId>,
    deadline: Duration,
    caller: &oneshot::Sender<Result<(), ActorLocalPublishError>>,
) -> Result<(), ActorLocalPublishError> {
    let guard = ticket.registry.lock_bind(&ticket.jid).await;
    if cleanup.publication_stopped() || caller.is_closed() {
        return Err(ActorLocalPublishError::CallerCancelled);
    }
    let published = authority.with_current(|| {
        ticket.registry.publish_provisional_if_expected(
            &guard,
            ticket.jid.clone(),
            &ticket.candidate,
            expected,
        )
    })?;
    if !published {
        return Err(ActorLocalPublishError::RegistryConflict);
    }
    drop(guard);
    authority.with_current(|| ())?;
    let mirror = tokio::time::timeout(
        deadline,
        ticket.actor.ask(RegisterProvisionalConnectionIfExpected {
            jid: ticket.jid.clone(),
            candidate: ticket.candidate.clone(),
            expected,
        }),
    )
    .await;
    match mirror {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return Err(ActorLocalPublishError::MirrorConflict),
        Err(_) | Ok(Err(_)) => return Err(ActorLocalPublishError::MirrorUncertain),
    }
    let probe = cleanup
        .publication_probe
        .lock()
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
        .clone();
    if let Some(probe) = probe {
        probe.pause().await;
    }
    if cleanup.publication_stopped() || caller.is_closed() {
        return Err(ActorLocalPublishError::CallerCancelled);
    }
    authority.with_current(|| ())?;
    let confirmation = tokio::time::timeout(
        deadline,
        ticket.actor.ask(ConfirmProvisionalConnection {
            jid: ticket.jid.clone(),
            candidate: ticket.candidate.clone(),
        }),
    )
    .await;
    let confirmed = match confirmation {
        Ok(Ok(Some(confirmed))) => confirmed,
        Ok(Ok(None)) => return Err(ActorLocalPublishError::MirrorConflict),
        Err(_) | Ok(Err(_)) => return Err(ActorLocalPublishError::MirrorUncertain),
    };
    if cleanup.publication_stopped() || caller.is_closed() {
        return Err(ActorLocalPublishError::CallerCancelled);
    }
    let guard = ticket.registry.lock_bind(&ticket.jid).await;
    if cleanup.publication_stopped() || caller.is_closed() {
        return Err(ActorLocalPublishError::CallerCancelled);
    }
    let active = authority.with_current(|| {
        ticket.registry.activate_provisional_if_confirmed(
            &guard,
            &ticket.jid,
            &ticket.candidate,
            &confirmed,
        )
    })?;
    if !active {
        return Err(ActorLocalPublishError::ActivationLost);
    }
    Ok(())
}

impl Drop for ActorLocalEndpointOwner {
    fn drop(&mut self) {
        self.request_stop(StopReason::Requested);
    }
}
