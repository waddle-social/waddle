//! Dormant physical owner for one actor-side local endpoint.
//!
//! This owner is release compiled but is not selected by `/ws`. The cleanup
//! index retains accepted envelopes after the actor or startup caller exits
//! and until both physical pumps join and a terminal owner settles them.

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
    custodies: Mutex<Vec<Arc<ActorOutboundCustody>>>,
    publications: Mutex<PublicationState>,
    joined: AtomicBool,
    #[cfg(feature = "test-support")]
    publication_probe: Mutex<Option<Arc<ActorPublicationProbe>>>,
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
            custodies: Mutex::new(Vec::new()),
            publications: Mutex::new(PublicationState {
                stopped: false,
                tickets: Vec::new(),
            }),
            joined: AtomicBool::new(false),
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
        if !record.joined.load(Ordering::Acquire) {
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
        if empty && !record.has_publications() {
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
        if !record.joined.load(Ordering::Acquire) {
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
        if empty && !record.has_publications() {
            index.records.remove(connection);
        }
        Some(terminal)
    }

    fn prune_if_settled(&self, connection: &ConnectionRef) {
        let mut index = self
            .state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if let Some(record) = index.records.get(connection) {
            let settled = record.joined.load(Ordering::Acquire)
                && !record.has_publications()
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
        let cleanup = match index.insert(connection.clone(), custodian.clone()) {
            Ok(cleanup) => cleanup,
            Err(error) => {
                custodian.request_stop(StopReason::Requested);
                return Err(error);
            }
        };
        let joined_cleanup = Arc::clone(&cleanup);
        let joined_index = index.clone();
        let joined_connection = connection.clone();
        tokio::spawn(async move {
            let state: CustodianState = joined_cleanup.custodian.wait_until_joined().await;
            joined_cleanup.stop_publication();
            joined_cleanup.seal_all();
            joined_cleanup
                .joined
                .store(state == CustodianState::Joined, Ordering::Release);
            joined_cleanup
                .retire_publications(retirement_deadline)
                .await;
            joined_index.prune_if_settled(&joined_connection);
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
        Ok((endpoint, receiver))
    }

    pub fn request_stop(&self, reason: StopReason) {
        self.cleanup.stop_publication();
        self.cleanup.seal_all();
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
