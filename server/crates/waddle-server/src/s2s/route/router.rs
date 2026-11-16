//! The S2S router: the stanza sink of inbound streams and the remote route
//! of local entities.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jid::{BareJid, DomainRef, FullJid, Jid};
use minidom::Element;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::identity::{enforce_stanza_addresses, CanonicalDomain};
use waddle_xmpp::s2s::into_client_content;
use waddle_xmpp::Stanza;
use xmpp_parsers::disco::{DiscoInfoResult, DiscoItemsResult, Feature, Identity};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::correlation::{
    CorrelationDirection, CorrelationError, CorrelationKey, CorrelationOwner, IqCorrelations,
    PendingRequest, RequestKind,
};
use super::local::{LocalDelivery, LocalOrigin};
use super::principal::RemotePrincipal;
use super::reach::{reach, InboundReach, LocalTarget};
use super::replies;
use super::rooms::{self, RemoteRooms, RoomBounds, RoomEgress, RoomRefusal};
use super::telemetry;
use crate::destination::{
    without_server_minted_credentials, InvalidDestination, RemoteOutbound, RemoteRoute,
    RemoteRouteOutcome, ServedDomains, UnroutableReason,
};
use crate::s2s::config::S2sConfig;
use crate::s2s::conn::{
    ConnectError, S2sHandle, SinkVerdict, StanzaSink, StreamDirection, StreamEnd, TransmitError,
    VerifiedPeerStream,
};

/// Bounds on the router's queues and pending requests.
///
/// The per-peer and per-resource bounds are shares of the global ones, so
/// that one remote domain, one local resource or remote senders' traffic
/// cannot take all of them: `pending_per_peer` is below
/// `pending_requests`, `pending_per_resource` below `queue_per_peer`, and
/// `reply_queues` below `peer_queues`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouterLimits {
    /// Stanzas waiting for one directional domain pair's stream. A full
    /// queue refuses further stanzas with `resource-constraint`.
    pub queue_per_peer: usize,
    /// Directional domain pairs with a queue at once.
    pub peer_queues: usize,
    /// A reply to a remote sender opens a pair's queue only while fewer
    /// than this many pairs have one, and is dropped otherwise, so remote
    /// senders alone never hold every pair's queue. A reply on a pair that
    /// already has a queue is not limited by it.
    pub reply_queues: usize,
    /// Requests to remote entities pending on this replica.
    pub pending_requests: usize,
    /// Requests to one remote domain pending on this replica, whichever
    /// local domain and resource sent them.
    pub pending_per_peer: usize,
    /// Requests to remote entities pending for one local resource.
    pub pending_per_resource: usize,
    /// How long a request to a remote entity waits for its response.
    pub request_ttl: Duration,
    /// Responses from remote entities being correlated at once; a response
    /// beyond this is dropped.
    pub completions: usize,
    /// How long a worker waits for a stream to take and write one stanza.
    pub transmit_deadline: Duration,
    /// How long an empty queue lives before its worker ends.
    pub queue_idle: Duration,
    /// How often expired requests are answered, requests another replica
    /// completed are released, and the replica records that it is alive.
    pub sweep_interval: Duration,
    /// How long after expiry a request is pruned once its replica has
    /// stopped recording that it is alive for as long.
    pub prune_after: Duration,
    /// Remote rooms one local resource may occupy or be joining at once; a
    /// further join is refused with `resource-constraint`.
    pub rooms_per_resource: usize,
    /// Remote rooms every local resource on this replica may occupy or be
    /// joining at once.
    pub rooms: usize,
    /// How long a remote room a resource left keeps admitting the room's
    /// answer to the leave.
    pub leave_grace: Duration,
    /// Stanzas from remote rooms waiting to be delivered to their
    /// occupants, in the order they arrived; a stanza beyond this is
    /// dropped.
    pub room_deliveries: usize,
}

impl Default for RouterLimits {
    fn default() -> Self {
        Self {
            queue_per_peer: 64,
            peer_queues: 64,
            reply_queues: 32,
            pending_requests: 4096,
            pending_per_peer: 1024,
            pending_per_resource: 16,
            request_ttl: Duration::from_secs(120),
            completions: 64,
            transmit_deadline: Duration::from_secs(10),
            queue_idle: Duration::from_secs(60),
            sweep_interval: Duration::from_secs(5),
            prune_after: Duration::from_secs(600),
            rooms_per_resource: 128,
            rooms: 16_384,
            leave_grace: Duration::from_secs(30),
            room_deliveries: 1024,
        }
    }
}

/// What the router runs with.
pub struct RouterOptions {
    /// The S2S configuration snapshot: local domains and peer admission.
    pub config: Arc<S2sConfig>,
    /// Every domain this deployment serves, with its services.
    pub served: ServedDomains,
    /// The correlation table every replica shares.
    pub correlations: Arc<dyn IqCorrelations>,
    pub limits: RouterLimits,
    /// Cancelled when the process starts to shut down.
    pub stop: Option<CancellationToken>,
}

/// One directional domain pair: stanzas from `local` to `remote`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Pair {
    local: CanonicalDomain,
    remote: CanonicalDomain,
}

/// A stanza waiting for its pair's stream, and why it is sent.
#[derive(Debug)]
struct Queued {
    stanza: Stanza,
    purpose: Purpose,
}

#[derive(Debug)]
enum Purpose {
    /// A local resource's request to a remote entity: registered for
    /// correlation before it is written, and answered with an error if it
    /// cannot be sent.
    Request {
        sender: FullJid,
        pending: PendingRequest,
        /// The reservation it holds on this replica.
        generation: u64,
    },
    /// A local entity's response to a remote request: sent only if it
    /// completes that pending request.
    Response {
        key: CorrelationKey,
        payload_namespace: Option<String>,
    },
    /// A response or error the router generated for a stanza it received.
    Reply,
    /// A local resource's message or presence for a remote room it
    /// occupies or is joining (ADR-0020 D7's room exception). If it cannot
    /// be sent, its sender is answered with an error, and a join that never
    /// left is forgotten.
    Room {
        sender: FullJid,
        /// The room and nickname of a join.
        join: Option<(BareJid, String)>,
    },
    /// The presence leaving a remote room for a local resource that ended:
    /// nothing is left to answer if it cannot be sent.
    Departure,
}

/// Why a queued request was not sent, as the error its sender receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// No route to the remote domain exists (RFC 6120 §10.4.3).
    NotFound,
    /// A route exists but no stream could carry the request in time
    /// (RFC 6120 §10.4.3).
    Timeout,
    /// The same request is already pending.
    Duplicate,
    /// The request's addresses or id are too long to correlate.
    TooLong,
    /// The router could not record or encode the request.
    Internal,
    /// No response arrived before the request expired.
    Expired,
}

impl Failure {
    fn from_connect(error: &ConnectError) -> Self {
        match error {
            ConnectError::NotAdmitted(_) | ConnectError::Resolve(_) | ConnectError::NotLocal => {
                Self::NotFound
            }
            ConnectError::ShuttingDown
            | ConnectError::BackingOff
            | ConnectError::InProgress
            | ConnectError::Busy
            | ConnectError::Failed(_) => Self::Timeout,
            // The process's rate of new outbound connections is spent, not
            // the peer's: nothing was dialled and no backoff is recorded,
            // so the sender may try again once the rate refills.
            ConnectError::RateLimited => Self::Timeout,
        }
    }

    fn error(self, home: &DomainRef) -> StanzaError {
        let (type_, condition, text) = match self {
            Self::NotFound => (
                ErrorType::Cancel,
                DefinedCondition::RemoteServerNotFound,
                "The remote domain cannot be reached from this server.",
            ),
            Self::Timeout => (
                ErrorType::Wait,
                DefinedCondition::RemoteServerTimeout,
                "No stream to the remote domain could be established in time.",
            ),
            Self::Duplicate => (
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                "A request with this id to this entity is already pending.",
            ),
            Self::TooLong => (
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                "The request's addresses or id are too long to correlate.",
            ),
            Self::Internal => (
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "The request could not be routed.",
            ),
            Self::Expired => (
                ErrorType::Wait,
                DefinedCondition::RemoteServerTimeout,
                "No response arrived from the remote entity in time.",
            ),
        };
        replies::error(home, type_, condition, text)
    }
}

/// A request reserved on this replica: counted against the limits and
/// answered at expiry if nothing else answered it.
#[derive(Debug)]
struct Reserved {
    sender: FullJid,
    peer: CanonicalDomain,
    expires_at_ms: i64,
    /// Tells this reservation apart from a later one with the same key.
    generation: u64,
    /// Whether its row is in the correlation table: set after the row is
    /// inserted, so a registered request whose row is missing was removed
    /// by whoever completed it.
    registered: bool,
}

#[derive(Debug, Default)]
struct PendingRequests {
    requests: HashMap<CorrelationKey, Reserved>,
    per_resource: HashMap<FullJid, usize>,
    per_peer: HashMap<CanonicalDomain, usize>,
}

/// Decrement `key`'s count in `counts`, forgetting it at zero.
fn decrement<K: std::hash::Hash + Eq>(counts: &mut HashMap<K, usize>, key: &K) {
    if let Some(count) = counts.get_mut(key) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(key);
        }
    }
}

struct Inner {
    config: Arc<S2sConfig>,
    served: ServedDomains,
    correlations: Arc<dyn IqCorrelations>,
    /// This router's name on the rows it registers.
    owner: CorrelationOwner,
    generations: AtomicU64,
    limits: RouterLimits,
    transport: OnceLock<S2sHandle>,
    local: OnceLock<Arc<dyn LocalDelivery>>,
    queues: Mutex<HashMap<Pair, mpsc::Sender<Queued>>>,
    pending: Mutex<PendingRequests>,
    /// The remote rooms local resources occupy or are joining. Taken
    /// before `queues` when both are held.
    rooms: Mutex<RemoteRooms>,
    /// Stanzas for the occupants of remote rooms, delivered in order.
    room_deliveries: mpsc::Sender<RoomDelivery>,
    completions: Arc<Semaphore>,
    tasks: TaskTracker,
    shutdown: CancellationToken,
}

/// Routes stanzas between local entities and S2S streams.
///
/// - As the [`StanzaSink`] of inbound streams it reads each stanza as a
///   [`RemotePrincipal`]'s, applies the identity profile to the addresses
///   it carries, and decides what it reaches ([`reach`]): the server
///   domain's service discovery and ping are answered, a response that
///   completes a local resource's pending request is delivered to that
///   resource, and everything else is refused (ADR-0020 D7 and D10).
/// - As the [`RemoteRoute`] of local entities it takes a stanza for a
///   remote domain, refuses what D7 refuses and what peer admission (D1)
///   refuses, and queues the rest on a bounded queue per directional
///   domain pair, without waiting for anything.
/// - One worker per queue dials the pair's stream on demand, records a
///   request for correlation before writing it, and answers a request that
///   cannot be sent with `remote-server-not-found` or
///   `remote-server-timeout` (RFC 6120 §10.4.3).
///
/// The handoff contract (custody, retry identity, ordering scope and error
/// responsibility) is recorded in `server/docs/planning/s2s-routing-2047.md`.
#[derive(Clone)]
pub struct S2sRouter {
    inner: Arc<Inner>,
}

impl S2sRouter {
    /// A router with no transport and no local delivery yet: until
    /// [`Self::attach_transport`], every stanza for a remote domain is
    /// refused with `resource-constraint`.
    pub fn new(options: RouterOptions) -> Self {
        let shutdown = options
            .stop
            .map_or_else(CancellationToken::new, |stop| stop.child_token());
        let (room_deliveries, room_receiver) = mpsc::channel(options.limits.room_deliveries.max(1));
        let inner = Arc::new(Inner {
            config: options.config,
            served: options.served,
            correlations: options.correlations,
            owner: CorrelationOwner::generate(),
            generations: AtomicU64::new(0),
            limits: options.limits,
            transport: OnceLock::new(),
            local: OnceLock::new(),
            queues: Mutex::new(HashMap::new()),
            pending: Mutex::new(PendingRequests::default()),
            rooms: Mutex::new(RemoteRooms::default()),
            room_deliveries,
            completions: Arc::new(Semaphore::new(options.limits.completions)),
            tasks: TaskTracker::new(),
            shutdown,
        });
        inner.tasks.spawn(sweep(Arc::clone(&inner)));
        inner
            .tasks
            .spawn(deliver_room_traffic(Arc::clone(&inner), room_receiver));
        Self { inner }
    }

    /// Give the router the connection manager that carries its stanzas.
    /// `false` when one was already attached.
    pub fn attach_transport(&self, transport: S2sHandle) -> bool {
        self.inner.transport.set(transport).is_ok()
    }

    /// Give the router the delivery path to local resources. `false` when
    /// one was already attached.
    pub fn attach_local(&self, local: Arc<dyn LocalDelivery>) -> bool {
        self.inner.local.set(local).is_ok()
    }

    /// Requests to remote entities pending on this replica.
    pub fn pending_requests(&self) -> usize {
        self.inner.pending().requests.len()
    }

    /// Remote rooms local resources on this replica occupy, are joining or
    /// have just left.
    pub fn remote_rooms(&self) -> usize {
        self.inner.rooms().len()
    }

    /// Stop taking stanzas, end every worker and wait for them, at most
    /// `budget`. Stanzas still queued are answered as if their stream could
    /// not be established. `true` when every task ended.
    pub async fn shutdown(&self, budget: Duration) -> bool {
        self.inner.shutdown.cancel();
        self.inner.tasks.close();
        tokio::time::timeout(budget, self.inner.tasks.wait())
            .await
            .is_ok()
    }
}

#[async_trait]
impl RemoteRoute for S2sRouter {
    async fn route(&self, outbound: RemoteOutbound) -> RemoteRouteOutcome {
        self.inner.route(outbound)
    }

    async fn resource_ended(&self, resource: &FullJid) {
        self.inner.leave_rooms(resource);
    }
}

impl StanzaSink for S2sRouter {
    fn offer(&self, stream: &VerifiedPeerStream, stanza: Element) -> SinkVerdict {
        self.inner.inbound(stream, stanza, false)
    }

    fn offer_malformed(&self, stream: &VerifiedPeerStream, stanza: Element) -> SinkVerdict {
        self.inner.inbound(stream, stanza, true)
    }

    fn stream_ended(&self, stream: &VerifiedPeerStream, end: StreamEnd) {
        if end == StreamEnd::Lost {
            self.inner.lose_rooms(stream);
        }
    }
}

/// A stanza for the occupant of a remote room.
type RoomDelivery = (FullJid, Stanza, LocalOrigin);

/// Milliseconds since the Unix epoch, the clock every replica's correlation
/// expiries share.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn duration_ms(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every update leaves the maps consistent, so a poisoned lock still
    // holds usable state.
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// `stanza` as an element qualified by `jabber:client`; the stream maps it
/// to its own content namespace.
fn stanza_element(stanza: &Stanza) -> Element {
    match stanza {
        Stanza::Iq(iq) => Element::from(iq.as_ref().clone()),
        Stanza::Message(message) => Element::from(message.clone()),
        Stanza::Presence(presence) => Element::from(presence.clone()),
    }
}

impl Inner {
    fn pending(&self) -> MutexGuard<'_, PendingRequests> {
        lock(&self.pending)
    }

    fn rooms(&self) -> MutexGuard<'_, RemoteRooms> {
        lock(&self.rooms)
    }

    fn room_bounds(&self) -> RoomBounds {
        RoomBounds {
            per_resource: self.limits.rooms_per_resource,
            total: self.limits.rooms,
            leave_grace: self.limits.leave_grace,
        }
    }

    fn home(&self) -> &DomainRef {
        self.served.local()
    }

    // --- remote route ---------------------------------------------------

    fn route(self: &Arc<Self>, outbound: RemoteOutbound) -> RemoteRouteOutcome {
        let refuse = |reason, outbound| RemoteRouteOutcome::Unroutable {
            reason,
            outbound: Box::new(outbound),
        };
        let (pair, purpose) = match self.plan(&outbound) {
            Ok(plan) => plan,
            Err(reason) => return refuse(reason, outbound),
        };
        let stanza = match outbound.stanza().clone() {
            Stanza::Iq(iq) => Stanza::Iq(Box::new(without_server_minted_credentials(*iq))),
            other => other,
        };
        if let Purpose::Room { sender, .. } = purpose {
            return self.route_room(pair, sender, stanza, outbound);
        }
        let reserved = match &purpose {
            Purpose::Request {
                pending,
                generation,
                ..
            } => Some((pending.key.clone(), *generation)),
            Purpose::Response { .. }
            | Purpose::Reply
            | Purpose::Room { .. }
            | Purpose::Departure => None,
        };
        if self.enqueue(pair, Queued { stanza, purpose }) {
            return RemoteRouteOutcome::Accepted;
        }
        if let Some((key, generation)) = reserved {
            self.release_if(&key, Some(generation));
        }
        refuse(UnroutableReason::EgressBusy, outbound)
    }

    /// Queue a local resource's message or presence for a remote room it
    /// occupies or is joining, and record what it does to the occupancy;
    /// anything else is account traffic, which D7 refuses. The occupancy is
    /// decided and recorded under one lock with the stanza's queueing, so a
    /// join is recorded exactly when it is queued.
    fn route_room(
        self: &Arc<Self>,
        pair: Pair,
        sender: FullJid,
        stanza: Stanza,
        outbound: RemoteOutbound,
    ) -> RemoteRouteOutcome {
        let refuse = |reason, outbound| RemoteRouteOutcome::Unroutable {
            reason,
            outbound: Box::new(outbound),
        };
        let bounds = self.room_bounds();
        let mut rooms = self.rooms();
        let egress = match rooms.outbound(
            &bounds,
            &sender,
            outbound.destination().address(),
            outbound.stanza(),
        ) {
            Ok(egress) => egress,
            // ADR-0020 D7: a local account's account traffic to remote JIDs
            // is refused, and nothing on its roster changes.
            Err(RoomRefusal::NotOccupied) => {
                return refuse(UnroutableReason::RefusedByPolicy, outbound)
            }
            Err(RoomRefusal::Bounds) => return refuse(UnroutableReason::EgressBusy, outbound),
        };
        let join = match &egress {
            RoomEgress::Join { room, nick } => Some((room.clone(), nick.clone())),
            RoomEgress::Leave { .. } | RoomEgress::Traffic => None,
        };
        let queued = Queued {
            stanza,
            purpose: Purpose::Room {
                sender: sender.clone(),
                join,
            },
        };
        if !self.enqueue(pair, queued) {
            return refuse(UnroutableReason::EgressBusy, outbound);
        }
        rooms.record(&sender, egress, Instant::now(), bounds.leave_grace);
        RemoteRouteOutcome::Accepted
    }

    /// Leave every remote room `resource` occupies or is joining, for a
    /// resource that ended (XEP-0045 §7.14), and forget its occupancies.
    fn leave_rooms(self: &Arc<Self>, resource: &FullJid) {
        let left = self.rooms().take_resource(resource);
        let Ok(local) = CanonicalDomain::from_domain(resource.domain()) else {
            return;
        };
        for (room, nick) in left {
            let (Some(stanza), Ok(remote)) = (
                rooms::leave_presence(resource, &room, &nick),
                CanonicalDomain::from_domain(room.domain()),
            ) else {
                continue;
            };
            let pair = Pair {
                local: local.clone(),
                remote,
            };
            let queued = Queued {
                stanza,
                purpose: Purpose::Departure,
            };
            if !self.enqueue(pair, queued) {
                tracing::debug!("dropping the leave of a remote room: the queue is full");
            }
        }
    }

    /// The stream between a local domain and a remote one failed: every
    /// local resource of that local domain in a room at that remote domain
    /// receives its own unavailable presence from the room with status 333
    /// (XEP-0045 §11.1), and its occupancy is forgotten. Stanzas either way
    /// may have been lost, and the room itself removes an occupant it
    /// cannot reach.
    fn lose_rooms(&self, stream: &VerifiedPeerStream) {
        let ended = self.rooms().take_lost(
            stream.local().as_domain_ref(),
            stream.remote().as_domain_ref(),
        );
        for occupancy in ended {
            if let Some(presence) = rooms::removed_by_error(&occupancy) {
                self.deliver_room(occupancy.resource, presence, LocalOrigin::Server);
            }
        }
    }

    /// Queue `stanza` for the occupant `to`, behind the room traffic already
    /// queued; dropped when the queue is full.
    fn deliver_room(&self, to: FullJid, stanza: Stanza, origin: LocalOrigin) {
        if self.room_deliveries.try_send((to, stanza, origin)).is_err() {
            tracing::debug!("dropping room traffic for a local occupant: the queue is full");
        }
    }

    /// Decide whether a local entity's stanza may go to its remote
    /// destination, and on which pair's queue.
    fn plan(&self, outbound: &RemoteOutbound) -> Result<(Pair, Purpose), UnroutableReason> {
        if self.transport.get().is_none() || self.shutdown.is_cancelled() {
            return Err(UnroutableReason::EgressBusy);
        }
        let remote =
            CanonicalDomain::from_domain(outbound.destination().domain()).map_err(|_| {
                UnroutableReason::InvalidDestination(InvalidDestination::UnpreparableDomain)
            })?;
        if let Some(reason) = InvalidDestination::for_kind(remote.kind()) {
            return Err(UnroutableReason::InvalidDestination(reason));
        }
        // A served domain is never routed to a peer (RFC 6120 §10.5).
        if self.served.serves(remote.as_domain_ref()) || self.config.is_local(&remote) {
            return Err(UnroutableReason::RefusedByPolicy);
        }
        if self.config.admission().decide(&remote).is_err() {
            return Err(UnroutableReason::PeerNotAdmitted);
        }
        let local = CanonicalDomain::from_domain(outbound.sender().domain())
            .map_err(|_| UnroutableReason::RefusedByPolicy)?;
        if !self.config.is_local(&local) {
            return Err(UnroutableReason::RefusedByPolicy);
        }
        let purpose = match outbound.stanza() {
            Stanza::Iq(iq) => match iq.as_ref() {
                Iq::Get { payload, .. } | Iq::Set { payload, .. } => {
                    self.reserve_request(outbound, iq, payload, &remote)?
                }
                Iq::Result { payload, .. } => Purpose::Response {
                    key: response_key(outbound, iq),
                    payload_namespace: payload.as_ref().map(Element::ns),
                },
                Iq::Error { .. } => Purpose::Response {
                    key: response_key(outbound, iq),
                    payload_namespace: None,
                },
            },
            // ADR-0020 D7: until account traffic is reviewed (gate G18), a
            // local account's messages and presence for remote JIDs are
            // refused, and nothing on its roster changes, unless they are
            // for a remote room the account's resource occupies or is
            // joining ([`Self::route_room`]). Only a bound resource of a
            // local account joins rooms.
            Stanza::Message(_) | Stanza::Presence(_) => {
                let Ok(sender) = outbound.sender().clone().try_into_full() else {
                    return Err(UnroutableReason::RefusedByPolicy);
                };
                if *sender.domain() != *self.served.local() {
                    return Err(UnroutableReason::RefusedByPolicy);
                }
                Purpose::Room { sender, join: None }
            }
        };
        Ok((Pair { local, remote }, purpose))
    }

    /// Reserve a local resource's request to a remote entity against the
    /// limits. Only a bound resource of a local account sends requests to
    /// remote entities, so only it can be answered.
    fn reserve_request(
        &self,
        outbound: &RemoteOutbound,
        iq: &Iq,
        payload: &Element,
        peer: &CanonicalDomain,
    ) -> Result<Purpose, UnroutableReason> {
        let Ok(sender) = outbound.sender().clone().try_into_full() else {
            return Err(UnroutableReason::RefusedByPolicy);
        };
        if *sender.domain() != *self.served.local() {
            return Err(UnroutableReason::RefusedByPolicy);
        }
        let kind = match iq {
            Iq::Set { .. } => RequestKind::Set,
            _ => RequestKind::Get,
        };
        let key = CorrelationKey {
            direction: CorrelationDirection::Outbound,
            local: Jid::from(sender.clone()),
            remote: outbound.destination().address().clone(),
            id: iq.id().to_owned(),
        };
        // An empty or overlong id, or an address too long for the table's
        // key, is refused before anything is queued.
        if !key.fits() {
            return Err(UnroutableReason::InvalidRequest);
        }
        let expires_at_ms = now_ms().saturating_add(duration_ms(self.limits.request_ttl));
        let mut pending = self.pending();
        if pending.requests.contains_key(&key) {
            return Err(UnroutableReason::InvalidRequest);
        }
        let per_resource = pending.per_resource.get(&sender).copied().unwrap_or(0);
        let per_peer = pending.per_peer.get(peer).copied().unwrap_or(0);
        if pending.requests.len() >= self.limits.pending_requests
            || per_peer >= self.limits.pending_per_peer
            || per_resource >= self.limits.pending_per_resource
        {
            return Err(UnroutableReason::EgressBusy);
        }
        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        pending.requests.insert(
            key.clone(),
            Reserved {
                sender: sender.clone(),
                peer: peer.clone(),
                expires_at_ms,
                generation,
                registered: false,
            },
        );
        *pending.per_resource.entry(sender.clone()).or_insert(0) += 1;
        *pending.per_peer.entry(peer.clone()).or_insert(0) += 1;
        Ok(Purpose::Request {
            sender,
            generation,
            pending: PendingRequest {
                key,
                kind,
                payload_namespace: payload.ns(),
                owner: self.owner.clone(),
                expires_at_ms,
            },
        })
    }

    /// Forget the request reserved under `key`, if it is still the
    /// reservation `generation` names (any reservation when `None`).
    fn release_if(&self, key: &CorrelationKey, generation: Option<u64>) {
        let mut pending = self.pending();
        let current = pending
            .requests
            .get(key)
            .map(|reserved| reserved.generation);
        if current.is_none() || generation.is_some_and(|generation| current != Some(generation)) {
            return;
        }
        let Some(reserved) = pending.requests.remove(key) else {
            return;
        };
        decrement(&mut pending.per_resource, &reserved.sender);
        decrement(&mut pending.per_peer, &reserved.peer);
    }

    /// Note that the reservation `generation` of `key` now has its row.
    fn mark_registered(&self, key: &CorrelationKey, generation: u64) {
        if let Some(reserved) = self.pending().requests.get_mut(key) {
            if reserved.generation == generation {
                reserved.registered = true;
            }
        }
    }

    /// Put `queued` on its pair's queue without waiting, starting the
    /// pair's worker if it has none: `false` when a full queue, too many
    /// pairs or shutdown refused it, and it was dropped.
    fn enqueue(self: &Arc<Self>, pair: Pair, queued: Queued) -> bool {
        let opens = if matches!(queued.purpose, Purpose::Reply) {
            self.limits.reply_queues
        } else {
            self.limits.peer_queues
        };
        let mut queues = lock(&self.queues);
        let queued = match queues.get(&pair) {
            Some(sender) => match sender.try_send(queued) {
                Ok(()) => return true,
                Err(TrySendError::Full(_)) => return false,
                // The worker ended; start another.
                Err(TrySendError::Closed(queued)) => {
                    queues.remove(&pair);
                    queued
                }
            },
            None => queued,
        };
        if queues.len() >= opens.min(self.limits.peer_queues)
            || self.tasks.is_closed()
            || self.shutdown.is_cancelled()
        {
            return false;
        }
        let (sender, receiver) = mpsc::channel(self.limits.queue_per_peer.max(1));
        if sender.try_send(queued).is_err() {
            return false;
        }
        let worker = PairWorker {
            router: Arc::clone(self),
            pair: pair.clone(),
            receiver,
            stream: None,
        };
        self.tasks.spawn(worker.run());
        queues.insert(pair, sender);
        true
    }

    /// End an idle pair's queue: `true` when it held nothing and no other
    /// stanza can reach it.
    fn retire(&self, pair: &Pair, receiver: &mut mpsc::Receiver<Queued>) -> bool {
        let mut queues = lock(&self.queues);
        if !receiver.is_empty() {
            return false;
        }
        queues.remove(pair);
        receiver.close();
        true
    }

    /// Answer a queued request that will not be sent. A registered request
    /// is answered only if this call withdraws it, so a request is never
    /// both answered and completed. Responses and replies are dropped:
    /// nothing answers them (RFC 6120 §8.3.1).
    async fn answer(&self, queued: Queued, failure: Failure, registered: bool) {
        let (sender, pending, generation) = match queued.purpose {
            Purpose::Request {
                sender,
                pending,
                generation,
            } => (sender, pending, generation),
            // A message or presence for a remote room that did not leave:
            // its sender learns why (RFC 6120 §10.4.3), and a join the room
            // never saw is forgotten.
            Purpose::Room { sender, join } => {
                if let Some((room, nick)) = &join {
                    self.rooms().forget_join(&sender, room, nick);
                }
                if let Some(reply) =
                    replies::error_reply(&queued.stanza, failure.error(self.home()))
                {
                    self.deliver_local(&sender, reply, LocalOrigin::Server)
                        .await;
                }
                return;
            }
            Purpose::Response { .. } | Purpose::Reply | Purpose::Departure => return,
        };
        if registered {
            match self.correlations.withdraw(&pending.key).await {
                Ok(true) => {}
                Ok(false) => {
                    self.release_if(&pending.key, Some(generation));
                    return;
                }
                // Unknown: the sweep answers it at expiry instead.
                Err(_) => return,
            }
        }
        self.release_if(&pending.key, Some(generation));
        if let Some(reply) = replies::error_reply(&queued.stanza, failure.error(self.home())) {
            self.deliver_local(&sender, reply, LocalOrigin::Server)
                .await;
        }
    }

    async fn deliver_local(&self, to: &FullJid, stanza: Stanza, origin: LocalOrigin) {
        if let Some(local) = self.local.get() {
            let _ = local.deliver(to, stanza, origin).await;
        }
    }

    // --- inbound streams ------------------------------------------------

    fn inbound(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        element: Element,
        malformed: bool,
    ) -> SinkVerdict {
        if stream.direction() != StreamDirection::Inbound {
            return SinkVerdict::Refuse;
        }
        let home = stream.local().as_domain_ref().to_owned();
        let mut element = into_client_content(element);
        // Gate G10: every address the stanza carries, not only its own,
        // must be in the identity profile before anything reads it.
        if malformed || enforce_stanza_addresses(&mut element).is_err() {
            self.refuse_unusable(stream, &element, replies::jid_malformed(&home));
            self.fail_unusable_response(stream, &element, &home);
            return SinkVerdict::Accepted;
        }
        let Some(stanza) = replies::parse_stanza(element.clone()) else {
            self.refuse_unusable(stream, &element, replies::bad_request(&home));
            return SinkVerdict::Accepted;
        };
        let (Some(from), Some(to)) = replies::addresses(&stanza) else {
            return SinkVerdict::Refuse;
        };
        let Ok(principal) = RemotePrincipal::from_stream(stream, from) else {
            return SinkVerdict::Refuse;
        };
        let destination = self.served.classify(Some(to));
        let Some(target) = LocalTarget::of(&destination) else {
            return SinkVerdict::Refuse;
        };
        // ADR-0020 D7's second exception: traffic from a remote room to the
        // full JID of a local resource that occupies, is joining or has just
        // left it, delivered to that resource only.
        if target == LocalTarget::AccountResource && !matches!(stanza, Stanza::Iq(_)) {
            if let Ok(occupant) = to.clone().try_into_full() {
                let admitted =
                    self.rooms()
                        .inbound(&occupant, principal.address(), &stanza, Instant::now());
                if admitted {
                    self.deliver_room(occupant, stanza, LocalOrigin::Remote);
                    return SinkVerdict::Accepted;
                }
            }
        }
        match reach(target, &stanza) {
            InboundReach::ServerDiscoInfo => self.answer_disco_info(stream, &stanza, &home),
            InboundReach::ServerDiscoItems => self.answer_disco_items(stream, &stanza, &home),
            InboundReach::ServerPing => {
                if let Some(result) = server_result(&stanza, None) {
                    self.reply(stream, result);
                }
            }
            InboundReach::CorrelatedResponse => {
                let payload_namespace = match &stanza {
                    Stanza::Iq(iq) => match iq.as_ref() {
                        Iq::Result {
                            payload: Some(payload),
                            ..
                        } => Some(payload.ns()),
                        _ => None,
                    },
                    Stanza::Message(_) | Stanza::Presence(_) => None,
                };
                self.complete_response(&principal, stanza, payload_namespace, LocalOrigin::Remote);
            }
            InboundReach::Refused(refusal) => {
                tracing::debug!(?refusal, "refusing a stanza from an s2s stream");
                // RFC 6121 §8.5.1 and ADR-0020 D10: refused presence is
                // ignored, never answered.
                if !matches!(stanza, Stanza::Presence(_)) {
                    if let Some(reply) =
                        replies::error_reply(&stanza, replies::service_unavailable(&home))
                    {
                        self.reply(stream, reply);
                    }
                }
            }
        }
        SinkVerdict::Accepted
    }

    /// Answer an element from `stream` that this server cannot use, with
    /// `error`. Presence is dropped instead and counted: an error presence
    /// that reaches a room as an occupant's answer tells the room the
    /// occupant is gone, and rooms remove such occupants.
    fn refuse_unusable(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        element: &Element,
        error: StanzaError,
    ) {
        if element.name() == "presence" {
            telemetry::unusable_presence_dropped();
            return;
        }
        if let Some(reply) = replies::error_reply_to_element(element, error) {
            self.reply(stream, reply);
        }
    }

    /// Queue a reply to a stanza that arrived on the inbound `stream`; it
    /// leaves on the outbound stream of the same pair. A full queue drops
    /// it: a reply is never worth blocking the stream that asked.
    fn reply(self: &Arc<Self>, stream: &VerifiedPeerStream, reply: Stanza) {
        let pair = Pair {
            local: stream.local().clone(),
            remote: stream.remote().clone(),
        };
        let queued = Queued {
            stanza: reply,
            purpose: Purpose::Reply,
        };
        if !self.enqueue(pair, queued) {
            tracing::debug!("dropping a reply to an s2s stanza: the queue is full");
        }
    }

    /// XEP-0030 §3.1: the server domain's identity and the one feature a
    /// remote entity can rely on here. Further features are advertised to
    /// remote askers only once verification has shown them (gate G21).
    fn answer_disco_info(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        stanza: &Stanza,
        home: &DomainRef,
    ) {
        if has_node(stanza) {
            return self.answer_item_not_found(stream, stanza, home);
        }
        let result = DiscoInfoResult {
            node: None,
            identities: vec![Identity {
                category: "server".to_owned(),
                type_: "im".to_owned(),
                lang: None,
                name: None,
            }],
            features: vec![Feature::new(DISCO_INFO_NS)],
            extensions: Vec::new(),
        };
        if let Some(reply) = server_result(stanza, Some(Element::from(result))) {
            self.reply(stream, reply);
        }
    }

    /// XEP-0030 §4: no item is listed to remote askers until a service is
    /// reachable by them.
    fn answer_disco_items(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        stanza: &Stanza,
        home: &DomainRef,
    ) {
        if has_node(stanza) {
            return self.answer_item_not_found(stream, stanza, home);
        }
        let result = DiscoItemsResult {
            node: None,
            items: Vec::new(),
            rsm: None,
        };
        if let Some(reply) = server_result(stanza, Some(Element::from(result))) {
            self.reply(stream, reply);
        }
    }

    fn answer_item_not_found(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        stanza: &Stanza,
        home: &DomainRef,
    ) {
        let error = replies::error(
            home,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "No such node.",
        );
        if let Some(reply) = replies::error_reply(stanza, error) {
            self.reply(stream, reply);
        }
    }

    /// A response this server refuses to process, because an address it
    /// carries is outside the identity profile, still ends the request it
    /// answers: the requester receives `service-unavailable` from this
    /// server now, instead of `remote-server-timeout` at expiry. It
    /// completes only what the response itself could have completed: the
    /// same sender, recipient and id, and a payload in the request's
    /// namespace.
    fn fail_unusable_response(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        element: &Element,
        home: &DomainRef,
    ) {
        let payload_namespace = match (element.name(), element.attr("type")) {
            ("iq", Some("result")) => element.children().next().map(Element::ns),
            ("iq", Some("error")) => None,
            _ => return,
        };
        let address = |name: &str| {
            element
                .attr(name)
                .and_then(|value| waddle_xmpp::identity::parse_jid(value).ok())
        };
        let (Some(from), Some(to), Some(id)) = (address("from"), address("to"), element.attr("id"))
        else {
            return;
        };
        let Ok(principal) = RemotePrincipal::from_stream(stream, &from) else {
            return;
        };
        let error = Stanza::Iq(Box::new(Iq::Error {
            from: Some(from),
            to: Some(to.clone()),
            id: id.to_owned(),
            payload: None,
            error: replies::unusable_response(home),
        }));
        let Some(target) = LocalTarget::of(&self.served.classify(Some(&to))) else {
            return;
        };
        if reach(target, &error) == InboundReach::CorrelatedResponse {
            self.complete_response(&principal, error, payload_namespace, LocalOrigin::Server);
        }
    }

    /// Deliver an IQ response to the local resource it is addressed to, if
    /// and only if it completes a request that resource sent to its sender
    /// with a payload in `payload_namespace` (`None` for a response without
    /// one). Correlation runs off the connection's task; when too many
    /// responses are being correlated, this one is dropped.
    fn complete_response(
        self: &Arc<Self>,
        principal: &RemotePrincipal,
        stanza: Stanza,
        payload_namespace: Option<String>,
        origin: LocalOrigin,
    ) {
        let Stanza::Iq(iq) = &stanza else {
            return;
        };
        let Some(Ok(requester)) = iq.to().map(|to| to.clone().try_into_full()) else {
            return;
        };
        let key = CorrelationKey {
            direction: CorrelationDirection::Outbound,
            local: Jid::from(requester.clone()),
            remote: principal.address().clone(),
            id: iq.id().to_owned(),
        };
        let Ok(permit) = Arc::clone(&self.completions).try_acquire_owned() else {
            tracing::debug!("dropping an s2s response: too many being correlated");
            return;
        };
        let router = Arc::clone(self);
        self.tasks.spawn(async move {
            let _permit = permit;
            // The reservation the response may complete, taken before the
            // table is changed: once the row is removed, its key may be
            // reserved again for a new request that this response must not
            // release.
            let generation = router
                .pending()
                .requests
                .get(&key)
                .map(|reserved| reserved.generation);
            match router
                .correlations
                .complete(&key, payload_namespace.as_deref(), now_ms())
                .await
            {
                Ok(true) => {
                    if let Some(generation) = generation {
                        router.release_if(&key, Some(generation));
                    }
                    router.deliver_local(&requester, stanza, origin).await;
                }
                Ok(false) => tracing::debug!("dropping an uncorrelated s2s response"),
                Err(_) => tracing::warn!("dropping an s2s response: correlation failed"),
            }
        });
    }

    // --- expiry -----------------------------------------------------------

    /// Answer every request reserved on this replica that has expired,
    /// unless a response, possibly on another replica, completed it first.
    async fn sweep_expired(&self) {
        let now = now_ms();
        let expired: Vec<(CorrelationKey, FullJid, u64)> = self
            .pending()
            .requests
            .iter()
            .filter(|(_, reserved)| reserved.expires_at_ms <= now)
            .map(|(key, reserved)| (key.clone(), reserved.sender.clone(), reserved.generation))
            .collect();
        for (key, sender, generation) in expired {
            match self.correlations.expire(&key, now).await {
                Ok(true) => {
                    self.release_if(&key, Some(generation));
                    let reply = Iq::Error {
                        from: Some(key.remote.clone()),
                        to: Some(Jid::from(sender.clone())),
                        id: key.id.clone(),
                        payload: None,
                        error: Failure::Expired.error(self.home()),
                    };
                    self.deliver_local(&sender, Stanza::Iq(Box::new(reply)), LocalOrigin::Server)
                        .await;
                }
                // Completed, withdrawn or never registered.
                Ok(false) => self.release_if(&key, Some(generation)),
                // Tried again at the next sweep.
                Err(_) => {}
            }
        }
    }

    /// Release every registered request on this replica whose row another
    /// replica removed: a response that reached that replica completed it.
    /// The candidates are taken before the table is read, and each was
    /// registered before it was marked, so a request missing from the
    /// table was removed, never not yet inserted.
    async fn sweep_completed_elsewhere(&self) {
        let registered: Vec<(CorrelationKey, u64)> = self
            .pending()
            .requests
            .iter()
            .filter(|(_, reserved)| reserved.registered)
            .map(|(key, reserved)| (key.clone(), reserved.generation))
            .collect();
        if registered.is_empty() {
            return;
        }
        let Ok(outstanding) = self.correlations.outstanding(&self.owner).await else {
            // Tried again at the next sweep.
            return;
        };
        let outstanding: HashSet<CorrelationKey> = outstanding.into_iter().collect();
        for (key, generation) in registered {
            if !outstanding.contains(&key) {
                self.release_if(&key, Some(generation));
            }
        }
    }
}

/// The key of the pending inbound request a local response answers.
fn response_key(outbound: &RemoteOutbound, iq: &Iq) -> CorrelationKey {
    CorrelationKey {
        direction: CorrelationDirection::Inbound,
        local: outbound.sender().clone(),
        remote: outbound.destination().address().clone(),
        id: iq.id().to_owned(),
    }
}

/// Whether an IQ request's query names a node.
fn has_node(stanza: &Stanza) -> bool {
    match stanza {
        Stanza::Iq(iq) => match iq.as_ref() {
            Iq::Get { payload, .. } | Iq::Set { payload, .. } => {
                payload.attr("node").is_some()
                    && (payload.is("query", DISCO_INFO_NS) || payload.is("query", DISCO_ITEMS_NS))
            }
            Iq::Result { .. } | Iq::Error { .. } => false,
        },
        Stanza::Message(_) | Stanza::Presence(_) => false,
    }
}

/// The result answering an IQ request with `payload`, from its recipient.
fn server_result(stanza: &Stanza, payload: Option<Element>) -> Option<Stanza> {
    let Stanza::Iq(iq) = stanza else {
        return None;
    };
    Some(Stanza::Iq(Box::new(Iq::Result {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_owned(),
        payload,
    })))
}

/// Until shutdown: answer expired requests, release those another replica
/// completed, record that this replica is alive, and prune what stopped
/// replicas left.
async fn sweep(router: Arc<Inner>) {
    let period = router.limits.sweep_interval.max(Duration::from_millis(10));
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_prune = Instant::now();
    loop {
        tokio::select! {
            () = router.shutdown.cancelled() => return,
            _ = interval.tick() => {}
        }
        router.sweep_expired().await;
        router.sweep_completed_elsewhere().await;
        router.rooms().sweep(Instant::now());
        if router
            .correlations
            .heartbeat(&router.owner, now_ms())
            .await
            .is_err()
        {
            tracing::warn!("recording the s2s router's liveness failed");
        }
        if last_prune.elapsed() >= router.limits.prune_after {
            last_prune = Instant::now();
            let before = now_ms().saturating_sub(duration_ms(router.limits.prune_after));
            if router.correlations.prune(before).await.is_err() {
                tracing::warn!("pruning expired s2s correlations failed");
            }
            if router
                .correlations
                .prune_own(&router.owner, before)
                .await
                .is_err()
            {
                tracing::warn!("pruning this replica's expired s2s correlations failed");
            }
        }
    }
}

/// Until shutdown: deliver the traffic of remote rooms to their local
/// occupants, one stanza at a time in the order it arrived, so an occupant
/// sees a room's presence and messages in the room's order.
async fn deliver_room_traffic(router: Arc<Inner>, mut receiver: mpsc::Receiver<RoomDelivery>) {
    loop {
        let next = tokio::select! {
            biased;
            () = router.shutdown.cancelled() => return,
            next = receiver.recv() => next,
        };
        let Some((to, stanza, origin)) = next else {
            return;
        };
        router.deliver_local(&to, stanza, origin).await;
    }
}

/// The worker of one directional pair's queue.
struct PairWorker {
    router: Arc<Inner>,
    pair: Pair,
    receiver: mpsc::Receiver<Queued>,
    /// The pair's established outbound stream, once dialled.
    stream: Option<VerifiedPeerStream>,
}

impl PairWorker {
    async fn run(mut self) {
        loop {
            let idle = self.router.limits.queue_idle;
            let next = tokio::select! {
                biased;
                () = self.router.shutdown.cancelled() => None,
                next = tokio::time::timeout(idle, self.receiver.recv()) => match next {
                    Ok(next) => next,
                    Err(_) if self.router.retire(&self.pair, &mut self.receiver) => None,
                    Err(_) => continue,
                },
            };
            let Some(queued) = next else {
                break;
            };
            self.send(queued).await;
        }
        // Shutdown or retirement: answer whatever is still queued.
        self.receiver.close();
        while let Ok(queued) = self.receiver.try_recv() {
            self.router.answer(queued, Failure::Timeout, false).await;
        }
    }

    /// The pair's stream, dialled on demand.
    async fn stream(&mut self, transport: &S2sHandle) -> Result<VerifiedPeerStream, Failure> {
        if let Some(stream) = &self.stream {
            return Ok(stream.clone());
        }
        let connect = transport.connect(&self.pair.local, &self.pair.remote);
        let stream = tokio::select! {
            () = self.router.shutdown.cancelled() => return Err(Failure::Timeout),
            connected = connect => connected.map_err(|error| Failure::from_connect(&error))?,
        };
        self.stream = Some(stream.clone());
        Ok(stream)
    }

    /// Answer every stanza queued behind one whose stream could not be
    /// established with the same failure.
    async fn fail_backlog(&mut self, failure: Failure) {
        while let Ok(queued) = self.receiver.try_recv() {
            self.router.answer(queued, failure, false).await;
        }
    }

    /// Send one stanza: dial the pair if needed, record a request for
    /// correlation (or complete the request a response answers) before the
    /// first byte, and write it. A stanza the stream provably did not take
    /// is offered once more on a new stream; one whose write outcome is
    /// unknown is never sent again.
    async fn send(&mut self, queued: Queued) {
        let Some(transport) = self.router.transport.get().cloned() else {
            return self.router.answer(queued, Failure::Timeout, false).await;
        };
        // A local response leaves only if it completes a pending remote
        // request, and that is settled before anything is dialled. If the
        // stream then cannot be established the response is lost, as it
        // would be anyway: nothing else could carry it.
        if let Purpose::Response {
            key,
            payload_namespace,
        } = &queued.purpose
        {
            let completed = self
                .router
                .correlations
                .complete(key, payload_namespace.as_deref(), now_ms())
                .await;
            if !matches!(completed, Ok(true)) {
                // Not a response to a pending remote request: dropped,
                // never forwarded.
                return;
            }
        }
        let mut stream = match self.stream(&transport).await {
            Ok(stream) => stream,
            Err(failure) => {
                self.router.answer(queued, failure, false).await;
                return self.fail_backlog(failure).await;
            }
        };
        let now = now_ms();
        let registered = match &queued.purpose {
            Purpose::Request {
                pending,
                generation,
                ..
            } => {
                if pending.expires_at_ms <= now {
                    return self.router.answer(queued, Failure::Expired, false).await;
                }
                match self.router.correlations.register(pending, now).await {
                    Ok(()) => {
                        self.router.mark_registered(&pending.key, *generation);
                        true
                    }
                    Err(CorrelationError::Duplicate) => {
                        return self.router.answer(queued, Failure::Duplicate, false).await
                    }
                    Err(CorrelationError::KeyTooLong) => {
                        return self.router.answer(queued, Failure::TooLong, false).await
                    }
                    Err(CorrelationError::Storage(_)) => {
                        return self.router.answer(queued, Failure::Internal, false).await
                    }
                }
            }
            Purpose::Response { .. }
            | Purpose::Reply
            | Purpose::Room { .. }
            | Purpose::Departure => false,
        };
        let mut element = stanza_element(&queued.stanza);
        let mut offered_again = false;
        loop {
            let deadline = self.router.limits.transmit_deadline;
            let written =
                tokio::time::timeout(deadline, transport.transmit(&stream, element)).await;
            match written {
                Ok(Ok(())) => return,
                Ok(Err(TransmitError::NotTaken(returned))) if !offered_again => {
                    self.stream = None;
                    offered_again = true;
                    element = returned;
                    stream = match self.stream(&transport).await {
                        Ok(stream) => stream,
                        Err(failure) => {
                            return self.router.answer(queued, failure, registered).await
                        }
                    };
                }
                Ok(Err(TransmitError::NotTaken(_))) => {
                    self.stream = None;
                    return self
                        .router
                        .answer(queued, Failure::Timeout, registered)
                        .await;
                }
                Ok(Err(TransmitError::Refused(_))) => {
                    return self
                        .router
                        .answer(queued, Failure::Internal, registered)
                        .await
                }
                // The stream may have written it: it is not sent again and
                // not answered here. A request stays pending until its
                // response or its expiry.
                Ok(Err(TransmitError::Unknown)) | Err(_) => {
                    self.stream = None;
                    return;
                }
            }
        }
    }
}
