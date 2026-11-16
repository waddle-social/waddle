//! The S2S router: the stanza sink of inbound streams and the remote route
//! of local entities.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jid::{BareJid, DomainRef, FullJid, Jid};
use minidom::Element;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::OwnedPermit;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::identity::{enforce_stanza_addresses, CanonicalDomain};
use waddle_xmpp::s2s::into_client_content;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::disco::{DiscoInfoResult, DiscoItemsResult, Feature, Identity};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::correlation::{
    CorrelationDirection, CorrelationError, CorrelationKey, CorrelationOwner, IqCorrelations,
    PendingRequest, RequestKind,
};
use super::local::{LocalDelivery, LocalOrigin};
use super::local_rooms::{CarriedRoomOutcome, CarriedRoomRequest, LocalRooms};
use super::occupancy::{
    InsertOutcome, Occupancy, OccupancyPhase, RemoteRoomStore, RoomLimits, RoomVerdict,
    MAX_JOIN_ID_BYTES,
};
use super::placement::{PlacementBounds, Placements};
use super::principal::RemotePrincipal;
use super::reach::{reach, InboundReach, LocalTarget, Refusal};
use super::replies;
use super::room_delivery::{InboundTraffic, Placed, RoomQueues, RoomWork};
use super::rooms::{self, Asked, Change, RoomEgress, VerificationAnswer};
use super::seats::{SeatIndex, SeatStore};
use super::telemetry;
use crate::destination::{
    without_server_minted_credentials, InvalidDestination, RemoteOutbound, RemoteRoute,
    RemoteRouteOutcome, SeatAdoption, ServedDomains, ServedService, UnroutableReason,
};
use crate::s2s::config::S2sConfig;
use crate::s2s::conn::{
    ConnectError, S2sHandle, SinkVerdict, SinkWait, StanzaSink, StreamDirection, StreamEnd,
    TransmitError, VerifiedPeerStream, Waited,
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
    /// Directional domain pairs with a queue at once. The S2S snapshot's
    /// `[limits] max_pair_queues` sets it for a deployment.
    pub peer_queues: usize,
    /// A reply to a remote sender opens a pair's queue only while fewer
    /// than this many pairs have one, and is dropped otherwise, so remote
    /// senders alone never hold every pair's queue. A reply on a pair that
    /// already has a queue is not limited by it. A remote occupant's join
    /// opens its pair's queue within the same bound, and a pair keeps its
    /// queue while this replica seats an occupant of its remote domain, so
    /// this is also the most remote domains one replica seats occupants
    /// for.
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
    /// Remote rooms one local resource may occupy, be joining or have
    /// just left at once; a further join is refused with
    /// `resource-constraint`.
    pub rooms_per_resource: usize,
    /// Remote rooms every resource of one local account may occupy, be
    /// joining or have just left at once.
    pub rooms_per_account: usize,
    /// Remote room occupancies of every local resource of the deployment,
    /// on every replica.
    pub rooms: usize,
    /// How long a remote room a resource left keeps admitting the room's
    /// answer to the leave.
    pub leave_grace: Duration,
    /// How long a join that left waits for the room's own presence before
    /// it is forgotten and its sender answered with
    /// `remote-server-timeout`.
    pub join_timeout: Duration,
    /// How long this server waits for a remote domain's answer to whether
    /// it is a multi-user chat service, and then for a room's answer to
    /// whether it is a room, before the join held for it is answered with
    /// `remote-server-timeout`.
    pub verification_timeout: Duration,
    /// Remote services and rooms this replica is asking at once what they
    /// are; a join that would ask another is refused with
    /// `resource-constraint`. A join to a room not asked before uses one
    /// for its service, unless that is known, and then one for the room.
    pub verifications: usize,
    /// How long an answer that a remote domain is a multi-user chat
    /// service, or that a JID at one is a room, is trusted.
    pub room_verdict_ttl: Duration,
    /// How long an answer that a remote domain or JID is not one is
    /// trusted.
    pub not_room_verdict_ttl: Duration,
    /// Stanzas from remote JIDs to local full JIDs waiting to be decided as
    /// room traffic and delivered, on this replica, shared equally by the
    /// workers. A stream whose stanza finds no place beyond this, or beyond
    /// one of the shares below, stops reading until it fits.
    pub room_deliveries: usize,
    /// Of those, the stanzas from one room.
    pub room_deliveries_per_room: usize,
    /// Of one worker's share, the stanzas from one remote domain, so that
    /// one peer cannot fill a worker's queue for the rooms of others. The
    /// default is a quarter of a worker's share; a value above the
    /// worker's share counts as the whole share.
    pub room_deliveries_per_peer: usize,
    /// How long a stream waits for one stanza to fit before the stanza is
    /// given up and the stream closed with `resource-constraint`. The bound
    /// is per stanza: a stream whose next stanza also finds the queues full
    /// waits again, so a stream may spend longer than this waiting in all.
    pub room_delivery_wait: Duration,
    /// Workers deciding and delivering room traffic; each serves its own
    /// share of rooms in order.
    pub room_delivery_workers: usize,
    /// How long delivering one stanza of room traffic to its occupant may
    /// take before the worker moves on.
    pub room_delivery_timeout: Duration,
    /// Remote occupants one local room seats; a further join is refused
    /// with `resource-constraint`.
    pub seats_per_room: usize,
    /// Seats of one remote domain, in every local room.
    pub seats_per_peer: usize,
    /// Seats of one remote account, in every local room and under every
    /// resource.
    pub seats_per_account: usize,
    /// Seats of every remote occupant of the deployment, on every replica.
    pub seats: usize,
    /// Stanzas remote principals addressed to the MUC service waiting to
    /// be decided by the room host, on this replica, shared equally by the
    /// workers. A stream whose stanza finds no place beyond this, or beyond
    /// one of the shares below, stops reading until it fits.
    pub room_requests: usize,
    /// Of those, the stanzas for one room.
    pub room_requests_per_room: usize,
    /// Of one worker's share, the stanzas from one remote domain.
    pub room_requests_per_peer: usize,
    /// Workers handing requests to the room host; each serves its own
    /// share of rooms in order.
    pub room_request_workers: usize,
    /// How long a stream waits for one such stanza to fit, or for its
    /// domain's request rate, before the stanza is given up and the stream
    /// closed with `resource-constraint`.
    pub room_request_wait: Duration,
    /// How long one item of a worker that hands requests to the room host
    /// may take: deciding one request, with every ask of the host and the
    /// seat table and the wait for a place for its answer, before the
    /// worker moves on. It is well under `room_request_wait`, so a slow
    /// room holds back the rooms of its shard for at most this per item
    /// and never long enough for a stream to be closed. The same bound
    /// applies to each step of a seat's end, which other workers run.
    pub room_request_timeout: Duration,
    /// Stanzas one remote domain may address to the MUC service a second,
    /// on average; a stream beyond it stops reading until the rate allows
    /// its next stanza.
    pub room_request_rate: u32,
    /// How many such stanzas one remote domain may send at once before its
    /// rate applies.
    pub room_request_burst: u32,
    /// Of its domain's rate, the stanzas one remote account may address to
    /// the MUC service a second, on average: requests, messages, chat
    /// states and markers alike. A stanza beyond it is not taken, and the
    /// domain's rate is not spent on it, so one account cannot spend what
    /// the other accounts of its domain may send. Never above the domain's
    /// rate.
    pub room_request_rate_per_account: u32,
    /// How many such stanzas one remote account may send at once before
    /// its rate applies. Never above the domain's burst.
    pub room_request_burst_per_account: u32,
    /// Of its domain's rate, the available presence one remote account may
    /// address to occupant JIDs a second, on average: joins, changes of
    /// availability or nickname, and entering a room again, each of which
    /// makes the room send presence to its occupants. A change counts
    /// against this and the account's other share; a join against this
    /// alone, so that an account's other stanzas never cost it a join. A
    /// change beyond it is dropped. A join beyond it is answered with
    /// `resource-constraint` while its domain's allowance of answered
    /// refusals lasts, and otherwise waits for it, holding its stream: a
    /// join is never dropped. Never above the domain's rate.
    pub room_presence_rate_per_account: u32,
    /// How many such presence stanzas one remote account may send at once
    /// before its rate applies: by default as many as the seats it may
    /// hold, so that a client entering every room it may at once is not
    /// refused. Never above the domain's burst.
    pub room_presence_burst_per_account: u32,
    /// Rooms whose requests are being placed at once: asked of the replica
    /// that holds them, in a deployment that keeps a room on one replica.
    pub room_placements: usize,
    /// Of those, the rooms being placed for requests of one remote domain,
    /// so that a domain whose rooms are held by a replica that does not
    /// answer leaves the places of every other domain alone.
    pub room_placements_per_peer: usize,
    /// How long a request may wait for one of those places, and then how
    /// long asking the room host where it is decided may take, before its
    /// sender is answered with `resource-constraint`. Only an ask that
    /// outlasts it, or that the host reports as unanswered, marks the room
    /// as one whose holder did not answer.
    pub room_placement_timeout: Duration,
    /// How long requests for a room whose holder did not answer are
    /// answered with `resource-constraint` without asking it again.
    pub room_placement_backoff: Duration,
    /// Stanzas of local rooms waiting for one directional pair's stream:
    /// answers to remote requests and the rooms' traffic to the seats of
    /// that remote domain. A stanza for a seat that finds no place ends
    /// the seat.
    pub seat_traffic_per_peer: usize,
    /// How long an answer to a remote request waits for a place among
    /// them before it is given up, never past the item's budget
    /// (`room_request_timeout`).
    pub room_reply_wait: Duration,
    /// Copies of chat states and markers one local room may have queued
    /// for remote seats a second, on average, and four seconds' worth at
    /// once. A copy beyond it is dropped.
    pub seat_ephemeral_rate_per_room: u32,
    /// Of those, the copies of the chat states and markers of one
    /// occupant, on the same terms.
    pub seat_ephemeral_rate_per_sender: u32,
    /// How long a stopping replica waits for the notices that tell its
    /// remote occupants so to be taken by their streams.
    pub shutdown_notice_wait: Duration,
    /// How often each seat held here is checked against the room that
    /// seats it.
    pub seat_reconcile_interval: Duration,
    /// How long after its last record that it is alive a replica's seat
    /// may be taken over by a join that reaches another replica.
    pub seat_owner_grace: Duration,
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
            rooms_per_account: 512,
            rooms: 65_536,
            leave_grace: Duration::from_secs(30),
            join_timeout: Duration::from_secs(60),
            verification_timeout: Duration::from_secs(30),
            verifications: 256,
            room_verdict_ttl: Duration::from_secs(3600),
            not_room_verdict_ttl: Duration::from_secs(300),
            room_deliveries: 1024,
            room_deliveries_per_room: 64,
            room_deliveries_per_peer: 64,
            room_delivery_wait: Duration::from_secs(30),
            room_delivery_workers: 4,
            room_delivery_timeout: Duration::from_secs(5),
            seats_per_room: 256,
            seats_per_peer: 4096,
            seats_per_account: 64,
            seats: 65_536,
            room_requests: 1024,
            room_requests_per_room: 64,
            room_requests_per_peer: 64,
            room_request_workers: 4,
            room_request_wait: Duration::from_secs(30),
            room_request_timeout: Duration::from_secs(5),
            room_request_rate: 50,
            room_request_burst: 200,
            room_request_rate_per_account: 10,
            room_request_burst_per_account: 40,
            room_presence_rate_per_account: 1,
            room_presence_burst_per_account: 64,
            room_placements: 64,
            room_placements_per_peer: 8,
            room_placement_timeout: Duration::from_secs(5),
            room_placement_backoff: Duration::from_secs(5),
            seat_traffic_per_peer: 4096,
            room_reply_wait: Duration::from_secs(10),
            seat_ephemeral_rate_per_room: 1024,
            seat_ephemeral_rate_per_sender: 256,
            shutdown_notice_wait: Duration::from_secs(2),
            seat_reconcile_interval: Duration::from_secs(60),
            seat_owner_grace: Duration::from_secs(30),
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
    /// The remote room occupancies every replica shares.
    pub rooms: Arc<dyn RemoteRoomStore>,
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
        /// The join this stanza is, when it put the occupancy in the
        /// joining phase.
        join: Option<JoinAttempt>,
    },
    /// The presence leaving a remote room for a local resource that ended:
    /// nothing is left to answer if it cannot be sent.
    Departure,
    /// This server's `disco#info` to the domain of a remote room that a
    /// local resource joins, or then to the room, registered for
    /// correlation before it is written. If it cannot be sent, the joins it
    /// holds are answered with the error.
    Verify {
        target: BareJid,
        pending: PendingRequest,
    },
    /// A local room's stanza for the seat of a remote occupant (ADR-0020
    /// D10): its presence and messages, one stanza for each seat. It is
    /// sent only while the seat is held here, or as the stanza that ends
    /// it, and while peer admission admits the domain. If it cannot be
    /// handed to the pair's stream the seat ends.
    Seat {
        room: BareJid,
        occupant: FullJid,
        session: OccupancySessionGeneration,
        /// The room's own presence that ends the seat: sent although the
        /// seat is no longer held.
        last: bool,
    },
    /// The answer of the MUC service, a room or an occupant JID to a
    /// remote entity's request. Nothing answers it if it cannot be sent.
    RoomReply,
    /// A stopping replica's notice to a remote occupant that its seat ended
    /// with the replica (XEP-0045 §11.2). Sent as an answer is; the replica
    /// waits until it is written or given up: `_held` is released when the
    /// pair's worker is done with the stanza.
    Notice { _held: hosting::Notice },
}

/// The two queues of one directional domain pair, served by one worker.
#[derive(Debug, Clone)]
struct PairQueue {
    /// Requests, responses, replies and the stanzas of local resources for
    /// remote rooms.
    sender: mpsc::Sender<Queued>,
    /// The stanzas of local rooms: answers to remote requests and the
    /// rooms' traffic to seats.
    seats: mpsc::Sender<Queued>,
}

/// The rooms this replica hosts for remote occupants: the host that
/// decides their requests and the shared table of their seats.
struct Hosting {
    host: Arc<dyn LocalRooms>,
    store: Arc<dyn SeatStore>,
}

/// A token bucket: one remote domain's allowance of stanzas for the MUC
/// service, one account's share of it, or a room's or a sender's allowance
/// of chat states and markers for remote seats.
#[derive(Debug, Clone, Copy)]
struct Allowance {
    tokens: f64,
    refilled: Instant,
    /// For a remote domain: the refusals of its accounts' stanzas that may
    /// still be answered.
    refusals: f64,
}

/// The join a queued join presence is.
#[derive(Debug, Clone)]
struct JoinAttempt {
    room: BareJid,
    attempt: String,
}

/// A join held on this replica until its room's service, and then the
/// room, is verified.
#[derive(Debug)]
struct HeldJoin {
    occupant: FullJid,
    room: BareJid,
    attempt: String,
    pair: Pair,
    stanza: Stanza,
}

/// This replica's question to one remote domain, is it a multi-user chat
/// service, or to one bare JID at such a service, is it a room?
#[derive(Debug)]
struct Verification {
    key: CorrelationKey,
    expires_at_ms: i64,
    /// Whether its row is in the correlation table.
    registered: bool,
    held: Vec<HeldJoin>,
}

/// How a verification ended, for the joins it held.
#[derive(Debug, Clone)]
enum Conclusion {
    Verdict(RoomVerdict),
    /// No verdict; each held join is answered with this error.
    Failed(Box<StanzaError>),
}

/// How many times a compare-and-set on an occupancy is tried against
/// concurrent writers before the change gives up.
const EDIT_ATTEMPTS: usize = 4;

/// Occupancies the sweep ends at once.
const SWEEP_BATCH: usize = 256;

/// Workers that end remote occupants' seats off the room shards.
const SEAT_END_WORKERS: usize = 4;

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
    queues: Mutex<HashMap<Pair, PairQueue>>,
    pending: Mutex<PendingRequests>,
    /// The room host and the seat table, once this replica hosts rooms for
    /// remote occupants. Until then the MUC service refuses remote senders.
    hosting: OnceLock<Hosting>,
    /// The seats of the remote occupants this replica seats.
    seats: Arc<SeatIndex>,
    /// Stanzas for local rooms, decided by the room host in order per
    /// room.
    local_rooms: RoomQueues,
    /// Each remote domain's allowance of stanzas for the MUC service.
    /// Taken before `account_allowances` when both are held.
    allowances: Mutex<HashMap<String, Allowance>>,
    /// Each remote account's share of its domain's allowance. Taken
    /// before `presence_allowances` when both are held.
    account_allowances: Mutex<HashMap<BareJid, Allowance>>,
    /// Each remote account's share of its domain's allowance for the
    /// presence it addresses to occupant JIDs.
    presence_allowances: Mutex<HashMap<BareJid, Allowance>>,
    /// Requests for local rooms waiting to be placed, where the room host
    /// keeps rooms on other replicas.
    placements: Placements,
    /// What rooms and their senders may still have queued of chat states
    /// and markers for remote seats.
    ephemeral: Mutex<hosting::EphemeralBudgets>,
    /// Whether a pass that checks seats against their rooms is running.
    reconciling: AtomicBool,
    /// The room the last such pass stopped at, when it did not reach the
    /// last one.
    reconcile_after: Mutex<Option<BareJid>>,
    /// The row the last check of this replica's rows stopped at.
    rows_after: Mutex<Option<(BareJid, FullJid)>>,
    /// Seat ends and removals of occupants that hold no seat, for the
    /// workers that end seats off the room shards.
    seat_ends: hosting::SeatEnds,
    /// Rooms whose removal of an occupant could not be confirmed: the next
    /// pass over the seats asks them, whether or not a seat is held there.
    recheck: Mutex<HashSet<BareJid>>,
    /// The notices of a stopping replica not yet written or given up.
    notices: Arc<hosting::Notices>,
    /// Set once this replica has begun to stop: no seat is opened any
    /// more.
    stopping: AtomicBool,
    /// Cancelled once a stopping replica has told its remote occupants so
    /// and given the notices their time: the streams may close then.
    drained: CancellationToken,
    /// The remote room occupancies every replica shares.
    rooms: Arc<dyn RemoteRoomStore>,
    /// The verifications this replica is waiting on, by the service's
    /// domain JID or the room's bare JID asked. Taken before `queues` when
    /// both are held.
    verifications: Mutex<HashMap<BareJid, Verification>>,
    /// Stanzas that may be room traffic, decided and delivered in order
    /// per room.
    room_queues: RoomQueues,
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
        // The router stops in two steps once the process stop fires: it
        // tells the remote occupants it seats that it is stopping, and
        // only then ends its workers.
        let shutdown = CancellationToken::new();
        let room_queues = RoomQueues::new(
            options.limits.room_delivery_workers,
            options.limits.room_deliveries,
            options.limits.room_deliveries_per_room,
            options.limits.room_deliveries_per_peer,
        );
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
            hosting: OnceLock::new(),
            seats: Arc::new(SeatIndex::default()),
            local_rooms: RoomQueues::new(
                options.limits.room_request_workers,
                options.limits.room_requests,
                options.limits.room_requests_per_room,
                options.limits.room_requests_per_peer,
            ),
            allowances: Mutex::new(HashMap::new()),
            account_allowances: Mutex::new(HashMap::new()),
            presence_allowances: Mutex::new(HashMap::new()),
            placements: Placements::new(PlacementBounds {
                waiting: options.limits.room_requests,
                waiting_per_room: options.limits.room_requests_per_room,
                waiting_per_peer: options.limits.room_requests_per_peer,
                placing: options.limits.room_placements,
                placing_per_peer: options.limits.room_placements_per_peer,
            }),
            ephemeral: Mutex::new(hosting::EphemeralBudgets::default()),
            reconciling: AtomicBool::new(false),
            reconcile_after: Mutex::new(None),
            rows_after: Mutex::new(None),
            seat_ends: hosting::SeatEnds::default(),
            recheck: Mutex::new(HashSet::new()),
            notices: Arc::new(hosting::Notices::default()),
            stopping: AtomicBool::new(false),
            drained: CancellationToken::new(),
            rooms: options.rooms,
            verifications: Mutex::new(HashMap::new()),
            room_queues,
            completions: Arc::new(Semaphore::new(options.limits.completions)),
            tasks: TaskTracker::new(),
            shutdown,
        });
        inner.tasks.spawn(sweep(Arc::clone(&inner)));
        if let Some(stop) = options.stop {
            let router = Arc::clone(&inner);
            inner.tasks.spawn(async move {
                tokio::select! {
                    () = router.shutdown.cancelled() => return,
                    () = stop.cancelled() => {}
                }
                router.stop().await;
            });
        }
        for shard in 0..inner.room_queues.shards() {
            inner
                .tasks
                .spawn(deliver_room_traffic(Arc::clone(&inner), shard));
        }
        for shard in 0..inner.local_rooms.shards() {
            inner
                .tasks
                .spawn(hosting::serve_local_rooms(Arc::clone(&inner), shard));
        }
        for _ in 0..SEAT_END_WORKERS {
            inner
                .tasks
                .spawn(hosting::serve_seat_ends(Arc::clone(&inner)));
        }
        Self { inner }
    }

    /// The token the S2S connection manager drains on: with a router, the
    /// router's own once it has told its remote occupants that it is
    /// stopping ([`Self::drained`]), since those notices need the streams;
    /// without one, the process stop itself.
    pub fn service_stop(router: Option<&Self>, stop: &CancellationToken) -> CancellationToken {
        router.map_or_else(|| stop.clone(), Self::drained)
    }

    /// Host rooms for remote occupants: `host` decides what remote
    /// principals ask of the MUC service, and `store` holds their seats for
    /// every replica. Until this is called the MUC service refuses remote
    /// senders. `false` when a host was already attached.
    pub fn attach_rooms(&self, host: Arc<dyn LocalRooms>, store: Arc<dyn SeatStore>) -> bool {
        self.inner.hosting.set(Hosting { host, store }).is_ok()
    }

    /// The seats of the remote occupants this replica seats, with the
    /// stream evidence each was admitted under.
    pub fn seats(&self) -> Arc<SeatIndex> {
        Arc::clone(&self.inner.seats)
    }

    /// Offer a local room's stanza for `occupant`. It is taken only when
    /// `occupant` holds a seat here in the room the stanza comes from; any
    /// other stanza is handed back, so nothing a room emits reaches a
    /// remote JID it did not seat. A stanza taken is queued for the seat's
    /// remote domain without waiting, one stanza for one seat; if it finds
    /// no place the seat ends.
    pub fn offer_to_seat(&self, occupant: &FullJid, stanza: Stanza) -> Result<(), Box<Stanza>> {
        self.inner.offer_to_seat(occupant, stanza)
    }

    /// Take a stanza another replica of this deployment verified on an
    /// inbound stream and carried here because this replica holds the room
    /// it addresses, with what that stream proved of its sender. It is
    /// decided, seated and answered as a stanza from a stream of this
    /// replica is, and never carried again.
    pub fn carried_room_request(&self, request: CarriedRoomRequest) -> CarriedRoomOutcome {
        self.inner.carried_room_request(request)
    }

    /// The local room `room` stopped being open to accounts at other
    /// servers: every remote occupant this replica seats in it is removed
    /// and told so, behind what the room already decides.
    pub fn close_room_seats(&self, room: &BareJid) {
        self.inner.close_room_seats(room);
    }

    /// Whether [`Self::offer_to_seat`] would take `stanza` for `occupant`.
    pub fn seats_recipient(&self, occupant: &FullJid, stanza: &Stanza) -> bool {
        self.inner.seat_of(occupant, stanza).is_some()
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

    /// Requests for local rooms waiting on this replica for the replica
    /// that holds their room to be found.
    pub fn requests_being_placed(&self) -> usize {
        self.inner.placements.waiting()
    }

    /// Requests to remote entities pending on this replica.
    pub fn pending_requests(&self) -> usize {
        self.inner.pending().requests.len()
    }

    /// Remote room occupancies of every local resource of the deployment,
    /// in every phase; `0` when the store cannot be read.
    pub async fn remote_rooms(&self) -> usize {
        self.inner.rooms.count().await.unwrap_or(0)
    }

    /// Cancelled once the router, having begun to stop, has told the
    /// remote occupants it seats that the service is shutting down and has
    /// given those notices their time to leave. The connection manager
    /// that carries the router's stanzas drains on this token, so its
    /// streams are still open while the notices are written.
    pub fn drained(&self) -> CancellationToken {
        self.inner.drained.clone()
    }

    /// Tell the remote occupants this replica seats that it is stopping,
    /// stop taking stanzas, end every worker and wait for them, at most
    /// `budget`. Stanzas still queued are answered as if their stream could
    /// not be established. `true` when every task ended.
    pub async fn shutdown(&self, budget: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + budget;
        // Within the budget; the workers end whether or not it finished.
        let _ = tokio::time::timeout_at(deadline, self.inner.stop()).await;
        self.inner.shutdown.cancel();
        self.inner.drained.cancel();
        self.inner.tasks.close();
        let ended = tokio::time::timeout_at(deadline, self.inner.tasks.wait())
            .await
            .is_ok();
        // The rooms this replica hosts end with it, so their remote
        // occupants hold no seat any more. Rows that stay behind are
        // removed as a stopped replica's.
        if let Some(hosting) = self.inner.hosting.get() {
            let removal = hosting.store.remove_owned(self.inner.owner.as_str());
            if !matches!(tokio::time::timeout_at(deadline, removal).await, Ok(Ok(_))) {
                tracing::warn!("removing this replica's remote occupants' seats failed");
            }
        }
        ended
    }
}

#[async_trait]
impl RemoteRoute for S2sRouter {
    async fn route(&self, outbound: RemoteOutbound) -> RemoteRouteOutcome {
        self.inner.route(outbound).await
    }

    async fn resource_ended(&self, resource: &FullJid) {
        self.inner.leave_rooms(resource).await;
    }

    async fn adopt_seat(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        session: OccupancySessionGeneration,
    ) -> SeatAdoption {
        self.inner.adopt_seat(room, occupant, session).await
    }

    async fn end_adopted_seats(&self, room: &BareJid, occupants: &[FullJid]) {
        self.inner.end_adopted_seats(room, occupants);
    }

    async fn carried_room_request(&self, request: CarriedRoomRequest) -> CarriedRoomOutcome {
        self.inner.carried_room_request(request)
    }

    async fn resource_resumed(&self, resource: &FullJid) {
        if self
            .inner
            .rooms
            .adopt_resource(resource, self.inner.owner.as_str(), now_ms())
            .await
            .is_err()
        {
            tracing::warn!(
                "recording the replica routing a resumed resource's remote rooms failed"
            );
        }
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
            self.inner.lose_seats(stream);
        }
    }
}

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
    /// Stop: tell the remote occupants this replica seats, then end the
    /// workers and let the streams close. Whoever calls it first runs it;
    /// a later call ends the workers at once.
    async fn stop(self: &Arc<Self>) {
        self.farewell().await;
        self.shutdown.cancel();
        self.drained.cancel();
    }

    fn pending(&self) -> MutexGuard<'_, PendingRequests> {
        lock(&self.pending)
    }

    fn verifications(&self) -> MutexGuard<'_, HashMap<BareJid, Verification>> {
        lock(&self.verifications)
    }

    fn room_limits(&self) -> RoomLimits {
        RoomLimits {
            per_resource: self.limits.rooms_per_resource,
            per_account: self.limits.rooms_per_account,
            total: self.limits.rooms,
        }
    }

    fn home(&self) -> &DomainRef {
        self.served.local()
    }

    // --- remote route ---------------------------------------------------

    async fn route(self: &Arc<Self>, outbound: RemoteOutbound) -> RemoteRouteOutcome {
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
            return match self
                .route_room(pair, &sender, outbound.destination().address(), stanza)
                .await
            {
                Ok(()) => RemoteRouteOutcome::Accepted,
                Err(reason) => refuse(reason, outbound),
            };
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
            | Purpose::Departure
            | Purpose::Verify { .. }
            | Purpose::Seat { .. }
            | Purpose::RoomReply
            | Purpose::Notice { .. } => None,
        };
        if self.enqueue(pair, Queued { stanza, purpose }) {
            return RemoteRouteOutcome::Accepted;
        }
        if let Some((key, generation)) = reserved {
            self.release_if(&key, Some(generation));
        }
        refuse(UnroutableReason::EgressBusy, outbound)
    }

    // --- outbound room traffic -------------------------------------------

    /// Route a local resource's message or presence for the remote JID
    /// `to`: room traffic for a room the resource joins or occupies is
    /// queued and its effect on the occupancy recorded; anything else is
    /// account traffic, which D7 refuses.
    async fn route_room(
        self: &Arc<Self>,
        pair: Pair,
        sender: &FullJid,
        to: &Jid,
        stanza: Stanza,
    ) -> Result<(), UnroutableReason> {
        let Some((room, egress)) = rooms::egress(to, &stanza) else {
            return Err(UnroutableReason::RefusedByPolicy);
        };
        match egress {
            RoomEgress::Join { nick, id } => self.join(pair, sender, room, nick, id, stanza).await,
            RoomEgress::Leave => self.leave(pair, sender, room, stanza).await,
            other => {
                let current = self
                    .rooms
                    .get(sender, &room)
                    .await
                    .map_err(|_| UnroutableReason::EgressBusy)?;
                // ADR-0020 D7: messages and presence for a remote JID the
                // resource does not occupy as a room are account traffic.
                let Some(current) = current else {
                    return Err(UnroutableReason::RefusedByPolicy);
                };
                if !other.permitted_in(current.phase) {
                    return Err(UnroutableReason::RefusedByPolicy);
                }
                let queued = Queued {
                    stanza,
                    purpose: Purpose::Room {
                        sender: sender.clone(),
                        join: None,
                    },
                };
                if !self.enqueue(pair, queued) {
                    return Err(UnroutableReason::EgressBusy);
                }
                self.adopt(&current).await;
                Ok(())
            }
        }
    }

    /// Record that this replica now routes the occupant's stanzas for the
    /// room, such as after its session resumed here.
    async fn adopt(&self, occupancy: &Occupancy) {
        if occupancy.owner != self.owner.as_str()
            && self
                .rooms
                .adopt(
                    &occupancy.occupant,
                    &occupancy.room,
                    self.owner.as_str(),
                    now_ms(),
                )
                .await
                .is_err()
        {
            tracing::debug!("recording the replica routing a remote room occupancy failed");
        }
    }

    /// A join presence (XEP-0045 §7.2) from `sender` for `room` as `nick`.
    ///
    /// - A join for a room the resource has joined is sent and changes
    ///   nothing: a client resynchronising after it reconnected under the
    ///   same full JID keeps the room's traffic while the answer is on its
    ///   way.
    /// - A join while joining or leaving is sent and waits for the room's
    ///   answer anew; after a leave, the leave's answer still to come is
    ///   absorbed rather than taken for the end of the new join.
    /// - Otherwise the JID's domain must be known to be a multi-user chat
    ///   service and the JID to be one of its rooms: cached verdicts decide
    ///   at once, and without them the join is held while this server asks
    ///   the domain's `disco#info` and then the room's, and nothing reaches
    ///   the JID.
    async fn join(
        self: &Arc<Self>,
        pair: Pair,
        sender: &FullJid,
        room: BareJid,
        nick: String,
        id: Option<String>,
        stanza: Stanza,
    ) -> Result<(), UnroutableReason> {
        let busy = |_| UnroutableReason::EgressBusy;
        let join_id = id.filter(|id| id.len() <= MAX_JOIN_ID_BYTES);
        let attempt = uuid::Uuid::new_v4().to_string();
        for _ in 0..EDIT_ATTEMPTS {
            let now = now_ms();
            let current = self.rooms.get(sender, &room).await.map_err(busy)?;
            let Some(current) = current else {
                let service = self
                    .rooms
                    .verdict(&service_of(&room), now)
                    .await
                    .map_err(busy)?;
                let named = self.rooms.verdict(&room, now).await.map_err(busy)?;
                let verdict = match (service, named) {
                    (Some(RoomVerdict::NotRoom), _) | (_, Some(RoomVerdict::NotRoom)) => {
                        Some(RoomVerdict::NotRoom)
                    }
                    (Some(RoomVerdict::Room), Some(RoomVerdict::Room)) => Some(RoomVerdict::Room),
                    _ => None,
                };
                // What is asked first: the service, unless it is known to
                // be one.
                let target = if service == Some(RoomVerdict::Room) {
                    room.clone()
                } else {
                    service_of(&room)
                };
                let mut occupancy = Occupancy {
                    occupant: sender.clone(),
                    room: room.clone(),
                    nick: nick.clone(),
                    phase: OccupancyPhase::Verifying,
                    affiliation: xmpp_parsers::muc::user::Affiliation::None,
                    attempt: attempt.clone(),
                    join_id: join_id.clone(),
                    leaves_pending: 0,
                    leave_until_ms: None,
                    owner: self.owner.as_str().to_owned(),
                    deadline_ms: None,
                    updated_at_ms: now,
                    revision: 0,
                };
                match verdict {
                    Some(RoomVerdict::NotRoom) => return Err(UnroutableReason::NotARoom),
                    Some(RoomVerdict::Room) => {
                        let Some(permit) = self.reserve(pair.clone(), false) else {
                            return Err(UnroutableReason::EgressBusy);
                        };
                        occupancy.phase = OccupancyPhase::Joining;
                        occupancy.deadline_ms = Some(now + duration_ms(self.limits.join_timeout));
                        match self
                            .rooms
                            .insert(&occupancy, &self.room_limits())
                            .await
                            .map_err(busy)?
                        {
                            InsertOutcome::Inserted => {
                                permit.send(self.join_queued(sender, &room, &attempt, stanza));
                                return Ok(());
                            }
                            InsertOutcome::Exists => continue,
                            InsertOutcome::Bounds => return Err(UnroutableReason::EgressBusy),
                        }
                    }
                    None => {
                        // The row is written, within the bounds, before the
                        // join is held and anything asked. A verification
                        // that ended before the join was held is either
                        // cached, which settles the join at once, or not,
                        // and then the join starts a new one.
                        occupancy.deadline_ms =
                            Some(now + 2 * duration_ms(self.limits.verification_timeout));
                        match self
                            .rooms
                            .insert(&occupancy, &self.room_limits())
                            .await
                            .map_err(busy)?
                        {
                            InsertOutcome::Inserted => {}
                            InsertOutcome::Exists => continue,
                            InsertOutcome::Bounds => return Err(UnroutableReason::EgressBusy),
                        }
                        if let Err(reason) =
                            self.hold(&pair, sender, &room, &attempt, stanza.clone(), &target)
                        {
                            self.discard(&occupancy).await;
                            return Err(reason);
                        }
                        self.settle_if_verified(&room, &attempt, &target).await;
                        return Ok(());
                    }
                }
            };
            match current.phase {
                OccupancyPhase::Joined => {
                    let queued = Queued {
                        stanza,
                        purpose: Purpose::Room {
                            sender: sender.clone(),
                            join: None,
                        },
                    };
                    if !self.enqueue(pair, queued) {
                        return Err(UnroutableReason::EgressBusy);
                    }
                    self.adopt(&current).await;
                    return Ok(());
                }
                OccupancyPhase::Verifying => {
                    // A newer join replaces the one held, with a deadline
                    // of its own.
                    let mut next = current.clone();
                    next.nick = nick.clone();
                    next.attempt = attempt.clone();
                    next.join_id = join_id.clone();
                    next.owner = self.owner.as_str().to_owned();
                    next.deadline_ms =
                        Some(now + 2 * duration_ms(self.limits.verification_timeout));
                    next.updated_at_ms = now;
                    if !self.rooms.replace(&next).await.map_err(busy)? {
                        continue;
                    }
                    next.revision = next.revision.saturating_add(1);
                    let target = match self.rooms.verdict(&service_of(&room), now).await {
                        Ok(Some(RoomVerdict::Room)) => room.clone(),
                        Ok(_) | Err(_) => service_of(&room),
                    };
                    // The row now waits on this join; one that cannot be
                    // held is forgotten and refused, as a new join is.
                    if let Err(reason) =
                        self.hold(&pair, sender, &room, &attempt, stanza.clone(), &target)
                    {
                        self.discard(&next).await;
                        return Err(reason);
                    }
                    self.settle_if_verified(&room, &attempt, &target).await;
                    return Ok(());
                }
                OccupancyPhase::Joining | OccupancyPhase::Leaving => {
                    let Some(permit) = self.reserve(pair.clone(), false) else {
                        return Err(UnroutableReason::EgressBusy);
                    };
                    let mut next = current.clone();
                    if current.phase == OccupancyPhase::Leaving {
                        // The leave's answer still comes first; it is
                        // absorbed while the leave's grace lasts.
                        next.leaves_pending = current.leaves_pending.saturating_add(1);
                        next.leave_until_ms = current.deadline_ms.max(current.leave_until_ms);
                    }
                    next.phase = OccupancyPhase::Joining;
                    next.nick = nick.clone();
                    next.attempt = attempt.clone();
                    next.join_id = join_id.clone();
                    next.owner = self.owner.as_str().to_owned();
                    next.deadline_ms = Some(now + duration_ms(self.limits.join_timeout));
                    next.updated_at_ms = now;
                    if self.rooms.replace(&next).await.map_err(busy)? {
                        permit.send(self.join_queued(sender, &room, &attempt, stanza));
                        return Ok(());
                    }
                }
            }
        }
        Err(UnroutableReason::EgressBusy)
    }

    fn join_queued(
        &self,
        sender: &FullJid,
        room: &BareJid,
        attempt: &str,
        stanza: Stanza,
    ) -> Queued {
        Queued {
            stanza,
            purpose: Purpose::Room {
                sender: sender.clone(),
                join: Some(JoinAttempt {
                    room: room.clone(),
                    attempt: attempt.to_owned(),
                }),
            },
        }
    }

    /// Unavailable presence from `sender` to an occupant JID of `room`
    /// (XEP-0045 §7.14). A join still held for verification is dropped
    /// with nothing sent; otherwise the leave is sent and the room's answer
    /// admitted for the leave's grace.
    async fn leave(
        self: &Arc<Self>,
        pair: Pair,
        sender: &FullJid,
        room: BareJid,
        stanza: Stanza,
    ) -> Result<(), UnroutableReason> {
        let busy = |_| UnroutableReason::EgressBusy;
        for _ in 0..EDIT_ATTEMPTS {
            let now = now_ms();
            let Some(current) = self.rooms.get(sender, &room).await.map_err(busy)? else {
                return Err(UnroutableReason::RefusedByPolicy);
            };
            if current.phase == OccupancyPhase::Verifying {
                if self.rooms.remove(&current).await.map_err(busy)? {
                    self.unhold(&room, &current.attempt);
                    return Ok(());
                }
                continue;
            }
            let Some(permit) = self.reserve(pair.clone(), false) else {
                return Err(UnroutableReason::EgressBusy);
            };
            let mut next = current.clone();
            next.phase = OccupancyPhase::Leaving;
            next.owner = self.owner.as_str().to_owned();
            next.deadline_ms = Some(now + duration_ms(self.limits.leave_grace));
            next.updated_at_ms = now;
            if self.rooms.replace(&next).await.map_err(busy)? {
                permit.send(Queued {
                    stanza,
                    purpose: Purpose::Room {
                        sender: sender.clone(),
                        join: None,
                    },
                });
                return Ok(());
            }
        }
        Err(UnroutableReason::EgressBusy)
    }

    // --- room verification -------------------------------------------------

    /// Hold `stanza`, a join from `occupant` named `attempt`, until
    /// `target` (the room's service, then the room) is verified, asking
    /// its `disco#info` if this replica is not asking already. A join the
    /// same occupant held for the same room before is replaced, whichever
    /// stage it was held at.
    fn hold(
        self: &Arc<Self>,
        pair: &Pair,
        occupant: &FullJid,
        room: &BareJid,
        attempt: &str,
        stanza: Stanza,
        target: &BareJid,
    ) -> Result<(), UnroutableReason> {
        let mut verifications = self.verifications();
        if !verifications.contains_key(target) {
            if verifications.len() >= self.limits.verifications {
                return Err(UnroutableReason::EgressBusy);
            }
            let local = Jid::from(BareJid::from_parts(None, pair.local.as_domain_ref()));
            let key = CorrelationKey {
                direction: CorrelationDirection::Outbound,
                local: local.clone(),
                remote: Jid::from(target.clone()),
                id: ["room-", &uuid::Uuid::new_v4().simple().to_string()].concat(),
            };
            if !key.fits() {
                return Err(UnroutableReason::InvalidRequest);
            }
            let expires_at_ms =
                now_ms().saturating_add(duration_ms(self.limits.verification_timeout));
            let queued = Queued {
                stanza: rooms::disco_request(local, target, key.id.clone()),
                purpose: Purpose::Verify {
                    target: target.clone(),
                    pending: PendingRequest {
                        key: key.clone(),
                        kind: RequestKind::Get,
                        payload_namespace: DISCO_INFO_NS.to_owned(),
                        owner: self.owner.clone(),
                        expires_at_ms,
                    },
                },
            };
            if !self.enqueue(pair.clone(), queued) {
                return Err(UnroutableReason::EgressBusy);
            }
            verifications.insert(
                target.clone(),
                Verification {
                    key,
                    expires_at_ms,
                    registered: false,
                    held: Vec::new(),
                },
            );
        }
        for stage in [service_of(room), room.clone()] {
            if let Some(verification) = verifications.get_mut(&stage) {
                verification
                    .held
                    .retain(|held| held.occupant != *occupant || held.room != *room);
            }
        }
        if let Some(verification) = verifications.get_mut(target) {
            verification.held.push(HeldJoin {
                occupant: occupant.clone(),
                room: room.clone(),
                attempt: attempt.to_owned(),
                pair: pair.clone(),
                stanza,
            });
        }
        Ok(())
    }

    /// Take the join named `attempt` for `room` off this replica's held
    /// joins, at either stage.
    fn unhold(&self, room: &BareJid, attempt: &str) -> Option<HeldJoin> {
        let mut verifications = self.verifications();
        for stage in [service_of(room), room.clone()] {
            let Some(verification) = verifications.get_mut(&stage) else {
                continue;
            };
            if let Some(index) = verification
                .held
                .iter()
                .position(|held| held.attempt == attempt)
            {
                return Some(verification.held.swap_remove(index));
            }
        }
        None
    }

    /// Whether this replica holds the join named `attempt` for `room`, at
    /// either stage.
    fn holds(&self, room: &BareJid, attempt: &str) -> bool {
        let verifications = self.verifications();
        [service_of(room), room.clone()].iter().any(|stage| {
            verifications.get(stage).is_some_and(|verification| {
                verification.held.iter().any(|held| held.attempt == attempt)
            })
        })
    }

    /// Settle the held join `attempt` for `room` at once if `target`, the
    /// stage it is held at, already has a verdict, such as one cached while
    /// this join was being recorded.
    async fn settle_if_verified(self: &Arc<Self>, room: &BareJid, attempt: &str, target: &BareJid) {
        let verdict = match self.rooms.verdict(target, now_ms()).await {
            Ok(Some(verdict)) => verdict,
            Ok(None) => return,
            Err(_) => {
                // The verification's own answer or expiry settles it.
                tracing::warn!("reading a remote room verdict failed");
                return;
            }
        };
        let Some(held) = self.unhold(room, attempt) else {
            return;
        };
        self.settle(held, &Conclusion::Verdict(verdict), target)
            .await;
    }

    /// End this replica's verification of `target` named by `key`, and
    /// settle every join it held.
    async fn conclude(
        self: &Arc<Self>,
        target: &BareJid,
        key: &CorrelationKey,
        conclusion: Conclusion,
    ) {
        let held = {
            let mut verifications = self.verifications();
            match verifications.get(target) {
                Some(verification) if verification.key == *key => verifications
                    .remove(target)
                    .map(|verification| verification.held)
                    .unwrap_or_default(),
                _ => return,
            }
        };
        for join in held {
            self.settle(join, &conclusion, target).await;
        }
    }

    /// Act on a held join once the verification of `asked`, its service or
    /// its room, ended, if the occupancy still waits on that join:
    ///
    /// - a service that is a multi-user chat service moves the join on to
    ///   its room: a cached verdict on the room decides at once, otherwise
    ///   the join is held while the room is asked;
    /// - a room is sent the join;
    /// - a JID whose service or room is not one, or that did not answer,
    ///   is sent nothing, and the join is answered with `item-not-found`
    ///   or the failure.
    async fn settle(self: &Arc<Self>, held: HeldJoin, conclusion: &Conclusion, asked: &BareJid) {
        let mut conclusion = conclusion.clone();
        if Asked::of(asked) == Asked::Service
            && matches!(conclusion, Conclusion::Verdict(RoomVerdict::Room))
        {
            match self.rooms.verdict(&held.room, now_ms()).await {
                Ok(Some(verdict)) => conclusion = Conclusion::Verdict(verdict),
                Ok(None) | Err(_) => return self.ask_room(held).await,
            }
        }
        self.finish(held, &conclusion).await;
    }

    /// Send a held join to its room, or refuse it, as `conclusion` says,
    /// if the occupancy still waits on that join.
    async fn finish(self: &Arc<Self>, held: HeldJoin, conclusion: &Conclusion) {
        let home = self.home().to_owned();
        let room = &held.room;
        for _ in 0..EDIT_ATTEMPTS {
            let current = match self.rooms.get(&held.occupant, room).await {
                Ok(Some(current)) => current,
                Ok(None) => return,
                Err(_) => {
                    // The row's deadline ends the join.
                    tracing::warn!("reading a held remote room join failed");
                    return;
                }
            };
            if current.phase != OccupancyPhase::Verifying || current.attempt != held.attempt {
                // Left, ended, or replaced by a later join.
                return;
            }
            let error = match conclusion {
                Conclusion::Verdict(RoomVerdict::Room) => {
                    let Some(permit) = self.reserve(held.pair.clone(), false) else {
                        if self.discard(&current).await {
                            self.answer_join(&current, Failure::Timeout.error(&home))
                                .await;
                        }
                        return;
                    };
                    let mut next = current.clone();
                    let now = now_ms();
                    next.phase = OccupancyPhase::Joining;
                    next.deadline_ms = Some(now + duration_ms(self.limits.join_timeout));
                    next.updated_at_ms = now;
                    match self.rooms.replace(&next).await {
                        Ok(true) => {
                            let queued = self.join_queued(
                                &held.occupant,
                                room,
                                &held.attempt,
                                held.stanza.clone(),
                            );
                            permit.send(queued);
                            return;
                        }
                        Ok(false) => continue,
                        Err(_) => {
                            tracing::warn!("sending a held remote room join failed");
                            return;
                        }
                    }
                }
                Conclusion::Verdict(RoomVerdict::NotRoom) => rooms::not_a_room(&home),
                Conclusion::Failed(error) => (**error).clone(),
            };
            match self.rooms.remove(&current).await {
                Ok(true) => {
                    self.answer_join(&current, error).await;
                    return;
                }
                Ok(false) => continue,
                Err(_) => {
                    tracing::warn!("ending a refused remote room join failed");
                    return;
                }
            }
        }
    }

    /// Hold a join whose service is a multi-user chat service until its
    /// room has answered whether it is one, if the occupancy still waits on
    /// that join. The row's deadline starts again for the second question;
    /// a join that cannot be held is forgotten and answered with
    /// `resource-constraint`.
    async fn ask_room(self: &Arc<Self>, held: HeldJoin) {
        for _ in 0..EDIT_ATTEMPTS {
            let current = match self.rooms.get(&held.occupant, &held.room).await {
                Ok(Some(current)) => current,
                Ok(None) => return,
                Err(_) => {
                    tracing::warn!("reading a held remote room join failed");
                    return;
                }
            };
            if current.phase != OccupancyPhase::Verifying || current.attempt != held.attempt {
                return;
            }
            let mut next = current.clone();
            let now = now_ms();
            next.deadline_ms = Some(now + 2 * duration_ms(self.limits.verification_timeout));
            next.updated_at_ms = now;
            match self.rooms.replace(&next).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => {
                    tracing::warn!("recording a remote room join's second question failed");
                    return;
                }
            }
            next.revision = next.revision.saturating_add(1);
            let room = held.room.clone();
            let attempt = held.attempt.clone();
            if self
                .hold(
                    &held.pair,
                    &held.occupant,
                    &room,
                    &attempt,
                    held.stanza,
                    &room,
                )
                .is_err()
            {
                if self.discard(&next).await {
                    self.answer_join(&next, rooms::busy(self.home())).await;
                }
                return;
            }
            // A verdict on the room cached while the join was being moved
            // on decides at once.
            if let Ok(Some(verdict)) = self.rooms.verdict(&room, now_ms()).await {
                if let Some(held) = self.unhold(&room, &attempt) {
                    self.finish(held, &Conclusion::Verdict(verdict)).await;
                }
            }
            return;
        }
        tracing::warn!("holding a remote room join for its room lost to concurrent changes");
    }

    /// Answer the join `occupancy` waited on with `error`, waiting at most
    /// the delivery timeout.
    async fn answer_join(&self, occupancy: &Occupancy, error: StanzaError) {
        if let Some(reply) = rooms::join_error(occupancy, error) {
            self.deliver_within(&occupancy.occupant, reply, LocalOrigin::Server)
                .await;
        }
    }

    /// Remove the row of a join that is refused or cannot go on, at the
    /// revision and attempt read: `true` when this call removed it. A
    /// failure is logged and the row's deadline ends it.
    async fn discard(&self, occupancy: &Occupancy) -> bool {
        match self.rooms.remove(occupancy).await {
            Ok(removed) => removed,
            Err(_) => {
                tracing::warn!("forgetting a remote room join failed");
                false
            }
        }
    }

    /// Complete this server's room verification with a response from a
    /// remote entity to the server domain, on whichever replica it
    /// arrived: the verdict is cached for every replica, and the joins this
    /// replica holds for it are settled. A replica holding joins for a
    /// verification another replica completed reads the cached verdict at
    /// its next sweep. `usable` is `false` for the error standing in for a
    /// response this server refuses to process.
    fn complete_verification(
        self: &Arc<Self>,
        principal: &RemotePrincipal,
        stanza: Stanza,
        payload_namespace: Option<String>,
        usable: bool,
    ) {
        let Stanza::Iq(iq) = &stanza else {
            return;
        };
        let Some(local) = iq.to().cloned() else {
            return;
        };
        // The service's domain JID or the room's bare JID, which the
        // request was sent to.
        let target = principal.address().to_bare();
        let key = CorrelationKey {
            direction: CorrelationDirection::Outbound,
            local,
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
            let now = now_ms();
            match router
                .correlations
                .complete(&key, payload_namespace.as_deref(), now)
                .await
            {
                Ok(true) => {
                    let asked = Asked::of(&target);
                    let conclusion = match rooms::verification_answer(&stanza, usable, asked) {
                        VerificationAnswer::Verdict(verdict) => {
                            let ttl = match verdict {
                                RoomVerdict::Room => router.limits.room_verdict_ttl,
                                RoomVerdict::NotRoom => router.limits.not_room_verdict_ttl,
                            };
                            if router
                                .rooms
                                .record_verdict(&target, verdict, now + duration_ms(ttl))
                                .await
                                .is_err()
                            {
                                tracing::warn!("recording a remote room verdict failed");
                            }
                            Conclusion::Verdict(verdict)
                        }
                        VerificationAnswer::Unavailable => {
                            Conclusion::Failed(Box::new(rooms::unanswered(router.home())))
                        }
                    };
                    router.conclude(&target, &key, conclusion).await;
                }
                Ok(false) => tracing::debug!("dropping an uncorrelated s2s response"),
                Err(_) => tracing::warn!("dropping an s2s response: correlation failed"),
            }
        });
    }

    /// Conclude this replica's verifications that expired or that another
    /// replica completed: a verdict cached by then decides. A verification
    /// another replica completed without a verdict cached yet waits for one
    /// until it expires; at expiry without a verdict, the held joins are
    /// answered with `remote-server-timeout`.
    async fn sweep_verifications(self: &Arc<Self>) {
        let now = now_ms();
        let pending: Vec<(BareJid, CorrelationKey, i64, bool)> = self
            .verifications()
            .iter()
            .map(|(room, verification)| {
                (
                    room.clone(),
                    verification.key.clone(),
                    verification.expires_at_ms,
                    verification.registered,
                )
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        let outstanding: Option<HashSet<CorrelationKey>> =
            if pending.iter().any(|(_, _, _, registered)| *registered) {
                self.correlations
                    .outstanding(&self.owner)
                    .await
                    .ok()
                    .map(|keys| keys.into_iter().collect())
            } else {
                None
            };
        for (room, key, expires_at_ms, registered) in pending {
            let ended = if expires_at_ms <= now {
                match self.correlations.expire(&key, now).await {
                    // Never answered.
                    Ok(true) => {
                        self.conclude(
                            &room,
                            &key,
                            Conclusion::Failed(Box::new(rooms::unanswered(self.home()))),
                        )
                        .await;
                        continue;
                    }
                    // Answered elsewhere, or never registered.
                    Ok(false) => true,
                    // Tried again at the next sweep.
                    Err(_) => false,
                }
            } else {
                registered
                    && outstanding
                        .as_ref()
                        .is_some_and(|outstanding| !outstanding.contains(&key))
            };
            if !ended {
                continue;
            }
            let conclusion = match self.rooms.verdict(&room, now).await {
                Ok(Some(verdict)) => Conclusion::Verdict(verdict),
                // The replica that completed it may not have recorded its
                // verdict yet: wait for it until the verification expires.
                Ok(None) | Err(_) if expires_at_ms > now => continue,
                Ok(None) | Err(_) => Conclusion::Failed(Box::new(rooms::unanswered(self.home()))),
            };
            self.conclude(&room, &key, conclusion).await;
        }
    }

    /// Note that this replica's verification of `target` now has its row.
    fn mark_verification_registered(&self, target: &BareJid, key: &CorrelationKey) {
        if let Some(verification) = self.verifications().get_mut(target) {
            if verification.key == *key {
                verification.registered = true;
            }
        }
    }

    // --- ending occupancies ------------------------------------------------

    /// Leave every remote room `resource` occupies or is joining, for a
    /// resource that ended (XEP-0045 §7.14), and forget its occupancies,
    /// whichever replica routed them. A join still held for verification
    /// is dropped; nothing reached the room.
    async fn leave_rooms(self: &Arc<Self>, resource: &FullJid) {
        let Ok(occupancies) = self.rooms.of_resource(resource).await else {
            tracing::warn!("reading the remote rooms of an ended resource failed");
            return;
        };
        for occupancy in occupancies {
            // Whatever the room's traffic changed since the read, the
            // occupancy ends, unless a later join replaced it or a session
            // that resumed elsewhere adopted it since: those belong to a
            // session that lives on.
            let Some(occupancy) = self
                .remove_current(&occupancy, |current| {
                    current.attempt == occupancy.attempt && current.owner == occupancy.owner
                })
                .await
            else {
                continue;
            };
            match occupancy.phase {
                OccupancyPhase::Joining | OccupancyPhase::Joined => self.depart(&occupancy),
                OccupancyPhase::Verifying => {
                    self.unhold(&occupancy.room, &occupancy.attempt);
                }
                OccupancyPhase::Leaving => {}
            }
        }
    }

    /// Queue the unavailable presence that leaves `occupancy`'s room for
    /// its resource.
    fn depart(self: &Arc<Self>, occupancy: &Occupancy) {
        let (Some(stanza), Ok(local), Ok(remote)) = (
            rooms::leave_presence(&occupancy.occupant, &occupancy.room, &occupancy.nick),
            CanonicalDomain::from_domain(occupancy.occupant.domain()),
            CanonicalDomain::from_domain(occupancy.room.domain()),
        ) else {
            return;
        };
        let queued = Queued {
            stanza,
            purpose: Purpose::Departure,
        };
        if !self.enqueue(Pair { local, remote }, queued) {
            tracing::debug!("dropping the leave of a remote room: the queue is full");
        }
    }

    /// The stream between a local domain and a remote one failed: every
    /// occupancy of a resource of that local domain in a room at that
    /// remote domain ends, and the occupant receives its own unavailable
    /// presence from the room with status 333 (XEP-0045 §11.1). An inbound
    /// stream carried the rooms' traffic for every occupant, so all of
    /// them end; an outbound one carried the stanzas of the occupants this
    /// replica routes. Each end is queued behind the room's traffic already
    /// queued, exempt from the queue's bounds.
    fn lose_rooms(self: &Arc<Self>, stream: &VerifiedPeerStream) {
        let router = Arc::clone(self);
        // The table holds the domains as the JIDs spell them.
        let local = stream.local().as_domain_ref().as_str().to_owned();
        let remote = stream.remote().as_domain_ref().as_str().to_owned();
        let only_ours = stream.direction() == StreamDirection::Outbound;
        self.tasks.spawn(async move {
            let owner = only_ours.then(|| router.owner.as_str().to_owned());
            let Ok(occupancies) = router
                .rooms
                .of_pair(&local, &remote, owner.as_deref())
                .await
            else {
                tracing::warn!("reading the remote rooms behind a lost stream failed");
                return;
            };
            for occupancy in occupancies {
                if occupancy.phase.occupies() {
                    router
                        .room_queues
                        .push_exempt(occupancy.room.clone(), RoomWork::Ended(Box::new(occupancy)));
                }
            }
        });
    }

    /// End joins the room never answered and leaves whose grace passed,
    /// on whichever replica routed them: a join is forgotten and answered
    /// with `remote-server-timeout`. A join this replica still holds for
    /// verification is left to its verification.
    async fn sweep_due_rooms(self: &Arc<Self>) {
        let now = now_ms();
        let Ok(due) = self.rooms.due(now, SWEEP_BATCH).await else {
            tracing::warn!("reading the remote room occupancies past their deadline failed");
            return;
        };
        for occupancy in due {
            if occupancy.phase == OccupancyPhase::Verifying
                && self.holds(&occupancy.room, &occupancy.attempt)
            {
                continue;
            }
            if !self.discard(&occupancy).await {
                continue;
            }
            match occupancy.phase {
                OccupancyPhase::Verifying | OccupancyPhase::Joining => {
                    self.answer_join(&occupancy, rooms::unanswered(self.home()))
                        .await;
                }
                OccupancyPhase::Joined | OccupancyPhase::Leaving => {}
            }
        }
    }

    /// End occupancies a stopped replica left: their resource leaves the
    /// room and is told it was removed (status 333), in case its session
    /// lives on elsewhere.
    async fn sweep_orphaned_rooms(self: &Arc<Self>, before_ms: i64) {
        let Ok(orphaned) = self.rooms.orphaned(before_ms, SWEEP_BATCH).await else {
            tracing::warn!("reading the remote room occupancies stopped replicas left failed");
            return;
        };
        for occupancy in orphaned {
            if !self.discard(&occupancy).await {
                continue;
            }
            match occupancy.phase {
                OccupancyPhase::Joining | OccupancyPhase::Joined => {
                    self.depart(&occupancy);
                    if let Some(presence) = rooms::removed_by_error(&occupancy) {
                        self.deliver_within(&occupancy.occupant, presence, LocalOrigin::Server)
                            .await;
                    }
                }
                OccupancyPhase::Verifying => {
                    self.answer_join(&occupancy, rooms::unanswered(self.home()))
                        .await;
                }
                OccupancyPhase::Leaving => {}
            }
        }
    }

    // --- inbound room traffic ------------------------------------------------

    /// Decide one stanza that may be a room's traffic for `occupant`,
    /// apply what it does to the occupancy, and deliver it if it is
    /// admitted; refuse it as account traffic otherwise (D7: a message
    /// receives `service-unavailable`, presence is dropped).
    async fn room_traffic(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        from: &Jid,
        occupant: &FullJid,
        stanza: Stanza,
    ) {
        let room = from.to_bare();
        let mut admitted = None;
        for _ in 0..EDIT_ATTEMPTS {
            let now = now_ms();
            let current = match self.rooms.get(occupant, &room).await {
                Ok(current) => current,
                // Unknown: dropped unanswered rather than refused.
                Err(_) => {
                    telemetry::room_traffic_store_failed();
                    tracing::warn!("dropping a remote room's stanza: the occupancy store failed");
                    return;
                }
            };
            let Some(current) = current else {
                admitted = Some(false);
                break;
            };
            let (admits, change) = rooms::inbound(&current, from, &stanza, now);
            admitted = Some(admits);
            let applied = match change {
                None => Ok(true),
                Some(Change::Remove) => self.rooms.remove(&current).await,
                Some(Change::Update(next)) => self.rooms.replace(&next).await,
            };
            match applied {
                Ok(false) => continue,
                Ok(true) => break,
                Err(_) => {
                    tracing::warn!("recording what a remote room's stanza changed failed");
                    break;
                }
            }
        }
        if admitted == Some(true) {
            self.deliver_room(occupant, stanza, LocalOrigin::Remote)
                .await;
            return;
        }
        tracing::debug!(refusal = ?Refusal::AccountTraffic, "refusing a stanza from an s2s stream");
        // RFC 6121 §8.5.1 and ADR-0020 D10: refused presence is ignored,
        // never answered.
        if matches!(stanza, Stanza::Message(_)) {
            let home = stream.local().as_domain_ref().to_owned();
            if let Some(reply) = replies::error_reply(&stanza, replies::service_unavailable(&home))
            {
                self.reply(stream, reply);
            }
        }
    }

    /// End `occupancy` after its stream failed, if it is still as read,
    /// and tell the occupant (XEP-0045 §11.1).
    async fn end_lost(&self, occupancy: Occupancy) {
        // The room's traffic queued before this may have moved the
        // occupancy on; it still ends unless a later join replaced the one
        // the stream carried, or the occupancy no longer occupies the room.
        let ended = self
            .remove_current(&occupancy, |current| {
                current.attempt == occupancy.attempt && current.phase.occupies()
            })
            .await;
        if let Some(ended) = ended {
            if let Some(presence) = rooms::removed_by_error(&ended) {
                self.deliver_room(&ended.occupant, presence, LocalOrigin::Server)
                    .await;
            }
        }
    }

    /// Remove `occupancy`'s row at whatever revision it now has, while
    /// `still` holds for it, and return the row removed.
    async fn remove_current(
        &self,
        occupancy: &Occupancy,
        still: impl Fn(&Occupancy) -> bool,
    ) -> Option<Occupancy> {
        for _ in 0..EDIT_ATTEMPTS {
            let current = match self.rooms.get(&occupancy.occupant, &occupancy.room).await {
                Ok(Some(current)) => current,
                Ok(None) => return None,
                Err(_) => continue,
            };
            if !still(&current) {
                return None;
            }
            if let Ok(true) = self.rooms.remove(&current).await {
                return Some(current);
            }
        }
        tracing::warn!("ending a remote room occupancy lost to concurrent changes");
        None
    }

    /// Deliver room traffic to its occupant, waiting at most the delivery
    /// timeout so that one slow delivery does not hold back the rooms
    /// behind it.
    async fn deliver_room(&self, to: &FullJid, stanza: Stanza, origin: LocalOrigin) {
        if !self.deliver_within(to, stanza, origin).await {
            telemetry::room_delivery_timed_out();
        }
    }

    /// Deliver `stanza` to a local resource, waiting at most the delivery
    /// timeout, so that a slow delivery holds back neither a room's worker
    /// nor the sweep: `false` when it was abandoned.
    async fn deliver_within(&self, to: &FullJid, stanza: Stanza, origin: LocalOrigin) -> bool {
        let delivery = self.deliver_local(to, stanza, origin);
        tokio::time::timeout(self.limits.room_delivery_timeout, delivery)
            .await
            .is_ok()
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
        let reply = matches!(queued.purpose, Purpose::Reply);
        match self.reserve(pair, reply) {
            Some(permit) => {
                permit.send(queued);
                true
            }
            None => false,
        }
    }

    /// Reserve a place on `pair`'s queue without waiting, starting the
    /// pair's worker if it has none: `None` when the queue is full, too
    /// many pairs have one, or the router is shutting down. A `reply`
    /// opens a queue only within the reply bound. A reservation keeps the
    /// queue from being retired until it is used or dropped.
    fn reserve(self: &Arc<Self>, pair: Pair, reply: bool) -> Option<OwnedPermit<Queued>> {
        let opens = if reply {
            self.limits.reply_queues
        } else {
            self.limits.peer_queues
        };
        self.reserve_on(pair, opens, |queue| &queue.sender)
    }

    /// Reserve a place on the queue `pick` names of `pair`'s two, without
    /// waiting, starting the pair's worker if it has none and fewer than
    /// `opens` pairs have one.
    fn reserve_on(
        self: &Arc<Self>,
        pair: Pair,
        opens: usize,
        pick: impl Fn(&PairQueue) -> &mpsc::Sender<Queued>,
    ) -> Option<OwnedPermit<Queued>> {
        let mut queues = lock(&self.queues);
        if let Some(queue) = queues.get(&pair) {
            match pick(queue).clone().try_reserve_owned() {
                Ok(permit) => return Some(permit),
                Err(TrySendError::Full(_)) => return None,
                // The worker ended; start another.
                Err(TrySendError::Closed(_)) => {
                    queues.remove(&pair);
                }
            }
        }
        if queues.len() >= opens.min(self.limits.peer_queues)
            || self.tasks.is_closed()
            || self.shutdown.is_cancelled()
        {
            return None;
        }
        let (sender, receiver) = mpsc::channel(self.limits.queue_per_peer.max(1));
        let (seats, seat_receiver) = mpsc::channel(self.limits.seat_traffic_per_peer.max(1));
        let queue = PairQueue { sender, seats };
        let permit = pick(&queue).clone().try_reserve_owned().ok()?;
        let worker = PairWorker {
            router: Arc::clone(self),
            pair: pair.clone(),
            receiver,
            seats: seat_receiver,
            stream: None,
        };
        self.tasks.spawn(worker.run());
        queues.insert(pair, queue);
        Some(permit)
    }

    /// End an idle pair's queue: `true` when it holds nothing, no place on
    /// it is reserved, this replica seats no occupant of its remote domain
    /// in a room of its local one, and no other stanza can reach it.
    fn retire(
        &self,
        pair: &Pair,
        receiver: &mut mpsc::Receiver<Queued>,
        seats: &mut mpsc::Receiver<Queued>,
    ) -> bool {
        // A pair whose remote domain this replica seats occupants for
        // keeps its queue: were it retired, the room's next stanza for
        // them might find every queue taken and end their seats.
        let seated = self.seats.seats_pair(
            pair.local.as_domain_ref().as_str(),
            pair.remote.as_domain_ref().as_str(),
        );
        let mut queues = lock(&self.queues);
        let reserved = queues.get(pair).is_some_and(|queue| {
            queue.sender.capacity() < queue.sender.max_capacity()
                || queue.seats.capacity() < queue.seats.max_capacity()
        });
        if !receiver.is_empty() || !seats.is_empty() || reserved || seated {
            return false;
        }
        queues.remove(pair);
        receiver.close();
        seats.close();
        true
    }

    /// Answer a queued request that will not be sent. A registered request
    /// is answered only if this call withdraws it, so a request is never
    /// both answered and completed. Responses and replies are dropped:
    /// nothing answers them (RFC 6120 §8.3.1).
    async fn answer(self: &Arc<Self>, queued: Queued, failure: Failure, registered: bool) {
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
                if let Some(join) = &join {
                    self.forget_join(&sender, join).await;
                }
                if let Some(reply) =
                    replies::error_reply(&queued.stanza, failure.error(self.home()))
                {
                    self.deliver_local(&sender, reply, LocalOrigin::Server)
                        .await;
                }
                return;
            }
            // The joins the verification held are answered with the
            // failure; nothing reached the JID.
            Purpose::Verify { target, pending } => {
                if registered && !matches!(self.correlations.withdraw(&pending.key).await, Ok(true))
                {
                    // Completed elsewhere, or unknown: the sweep concludes
                    // it from the cached verdict or at expiry.
                    return;
                }
                let error = failure.error(self.home());
                self.conclude(&target, &pending.key, Conclusion::Failed(Box::new(error)))
                    .await;
                return;
            }
            // A room's stanza that did not reach its seat: the occupant may
            // have missed it, so the seat ends and the occupant joins again.
            Purpose::Seat {
                room,
                occupant,
                session,
                last,
            } => {
                if !last {
                    self.seat_undeliverable(&room, &occupant, session);
                }
                return;
            }
            Purpose::Response { .. }
            | Purpose::Reply
            | Purpose::Departure
            | Purpose::RoomReply
            | Purpose::Notice { .. } => return,
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

    /// Forget a join that never left, if the occupancy still waits on it.
    async fn forget_join(&self, sender: &FullJid, join: &JoinAttempt) {
        for _ in 0..EDIT_ATTEMPTS {
            let Ok(Some(current)) = self.rooms.get(sender, &join.room).await else {
                return;
            };
            if current.phase != OccupancyPhase::Joining || current.attempt != join.attempt {
                return;
            }
            match self.rooms.remove(&current).await {
                Ok(false) => continue,
                Ok(true) => return,
                Err(_) => {
                    tracing::warn!("forgetting a remote room join that never left failed");
                    return;
                }
            }
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
        // left it, delivered to that resource only. Whether it is room
        // traffic is decided against the shared occupancy table, off the
        // stream's task and in the room's order. A stanza that finds the
        // queues full holds its stream until it fits: a room's presence
        // burst is never cut short.
        if target == LocalTarget::AccountResource && !matches!(stanza, Stanza::Iq(_)) {
            if let Ok(occupant) = to.clone().try_into_full() {
                let from = principal.address().clone();
                let room = from.to_bare();
                let peer = stream.remote().as_domain_ref().as_str().to_owned();
                let work = RoomWork::Traffic(Box::new(InboundTraffic {
                    stream: stream.clone(),
                    from,
                    occupant,
                    stanza,
                }));
                let Err(work) = self.room_queues.try_push(room.clone(), work, &peer) else {
                    return SinkVerdict::Accepted;
                };
                telemetry::room_traffic_waited();
                let router = Arc::clone(self);
                let deadline = tokio::time::Instant::now() + self.limits.room_delivery_wait;
                return SinkVerdict::Wait(SinkWait::new(async move {
                    let placed = router
                        .room_queues
                        .push_or_wait(room, work, &peer, deadline, &router.shutdown)
                        .await;
                    match placed {
                        Placed::Queued => Waited::Taken,
                        Placed::TimedOut => {
                            telemetry::room_traffic_overloaded();
                            Waited::Overloaded
                        }
                        Placed::Stopped => Waited::Stopping,
                    }
                }));
            }
        }
        match reach(target, &stanza, self.reachable()) {
            InboundReach::Room(operation) => {
                return self.room_request(stream, principal, operation, stanza);
            }
            InboundReach::ServerDiscoInfo => self.answer_disco_info(stream, &stanza, &home),
            InboundReach::ServerDiscoItems => self.answer_disco_items(stream, &stanza, &home),
            InboundReach::ServerPing => {
                if let Some(result) = server_result(&stanza, None) {
                    self.reply(stream, result);
                }
            }
            InboundReach::ServerResponse => {
                let payload_namespace = response_namespace(&stanza);
                self.complete_verification(&principal, stanza, payload_namespace, true);
            }
            InboundReach::CorrelatedResponse => {
                let payload_namespace = response_namespace(&stanza);
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

    /// XEP-0030 §4: an item is listed to remote askers only for a service
    /// they reach.
    fn answer_disco_items(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        stanza: &Stanza,
        home: &DomainRef,
    ) {
        if has_node(stanza) {
            return self.answer_item_not_found(stream, stanza, home);
        }
        // A service is listed once remote senders reach it on this replica:
        // the MUC service, while rooms are hosted for remote occupants.
        let items = if self.reachable().rooms {
            self.served
                .domains_of(ServedService::Muc)
                .map(|domain| xmpp_parsers::disco::Item {
                    jid: Jid::from(BareJid::from_parts(None, domain)),
                    node: None,
                    name: None,
                })
                .collect()
        } else {
            Vec::new()
        };
        let result = DiscoItemsResult {
            node: None,
            items,
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
        match reach(target, &error, self.reachable()) {
            InboundReach::CorrelatedResponse => {
                self.complete_response(&principal, error, payload_namespace, LocalOrigin::Server);
            }
            InboundReach::ServerResponse => {
                self.complete_verification(&principal, error, payload_namespace, false);
            }
            _ => {}
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
                    self.deliver_within(&sender, Stanza::Iq(Box::new(reply)), LocalOrigin::Server)
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

/// The multi-user chat service that hosts `room`: its domain, which this
/// server asks whether it is one (XEP-0045 §6.2).
fn service_of(room: &BareJid) -> BareJid {
    BareJid::from_parts(None, room.domain())
}

/// The namespace of an IQ result's payload, if it carries one.
fn response_namespace(stanza: &Stanza) -> Option<String> {
    match stanza {
        Stanza::Iq(iq) => match iq.as_ref() {
            Iq::Result {
                payload: Some(payload),
                ..
            } => Some(payload.ns()),
            _ => None,
        },
        Stanza::Message(_) | Stanza::Presence(_) => None,
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
    let mut last_reconcile = Instant::now();
    loop {
        tokio::select! {
            () = router.shutdown.cancelled() => return,
            _ = interval.tick() => {}
        }
        // Liveness first: the sweeps below deliver to local resources and
        // may take a while, and another replica takes this one's room
        // occupancies for a stopped replica's once it stops recording it.
        if router
            .correlations
            .heartbeat(&router.owner, now_ms())
            .await
            .is_err()
        {
            tracing::warn!("recording the s2s router's liveness failed");
        }
        router.sweep_expired().await;
        router.sweep_completed_elsewhere().await;
        router.sweep_verifications().await;
        router.sweep_due_rooms().await;
        router.sweep_lost_seats().await;
        // On a task of its own: a pass asks rooms, and this loop records
        // that the replica is alive.
        if last_reconcile.elapsed() >= router.limits.seat_reconcile_interval {
            last_reconcile = Instant::now();
            router.start_reconcile();
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
            router.sweep_orphaned_rooms(before).await;
            router.sweep_orphaned_seats(before).await;
            if router.rooms.prune_verdicts(now_ms()).await.is_err() {
                tracing::warn!("pruning expired remote room verdicts failed");
            }
        }
    }
}

/// Until shutdown: decide and deliver the work of one shard of rooms, one
/// item at a time, so an occupant sees a room's presence and messages in
/// the room's order.
async fn deliver_room_traffic(router: Arc<Inner>, shard: usize) {
    loop {
        let work = tokio::select! {
            biased;
            () = router.shutdown.cancelled() => return,
            work = router.room_queues.next(shard) => work,
        };
        match work {
            RoomWork::Traffic(traffic) => {
                let InboundTraffic {
                    stream,
                    from,
                    occupant,
                    stanza,
                } = *traffic;
                router.room_traffic(&stream, &from, &occupant, stanza).await
            }
            RoomWork::Ended(occupancy) => router.end_lost(*occupancy).await,
            // Work of the queues that serve local rooms.
            RoomWork::Request(_) | RoomWork::Unseat(_) | RoomWork::Departure(_) => {}
        }
    }
}

/// The worker of one directional pair's queues.
struct PairWorker {
    router: Arc<Inner>,
    pair: Pair,
    receiver: mpsc::Receiver<Queued>,
    /// The stanzas of local rooms for the pair's remote domain.
    seats: mpsc::Receiver<Queued>,
    /// The pair's established outbound stream, once dialled.
    stream: Option<VerifiedPeerStream>,
}

impl PairWorker {
    async fn run(mut self) {
        loop {
            let idle = self.router.limits.queue_idle;
            // Requests and replies first: the rooms' traffic to a domain
            // with many seats must not hold back the answers it waits for.
            let next = tokio::select! {
                biased;
                () = self.router.shutdown.cancelled() => None,
                next = self.receiver.recv() => next,
                next = self.seats.recv() => next,
                () = tokio::time::sleep(idle) => {
                    if self.router.retire(&self.pair, &mut self.receiver, &mut self.seats) {
                        None
                    } else {
                        continue;
                    }
                }
            };
            let Some(queued) = next else {
                break;
            };
            self.send(queued).await;
        }
        // Shutdown or retirement: answer whatever is still queued.
        self.receiver.close();
        self.seats.close();
        while let Ok(queued) = self.receiver.try_recv() {
            self.router.answer(queued, Failure::Timeout, false).await;
        }
        // A stopping replica's seats end with it; nothing is unseated.
        if !self.router.shutdown.is_cancelled() {
            while let Ok(queued) = self.seats.try_recv() {
                self.router.answer(queued, Failure::Timeout, false).await;
            }
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
        while let Ok(queued) = self.seats.try_recv() {
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
        // The effect boundary of a local room's stanza for a remote JID: it
        // leaves only for a domain peer admission admits now (ADR-0020 D1)
        // and, when it is a room's traffic, only for a seat this replica
        // still holds. Whatever queued it decided earlier.
        match &queued.purpose {
            Purpose::Seat {
                room,
                occupant,
                session,
                last,
            } => {
                let held = self
                    .router
                    .seats
                    .get(room, occupant)
                    .is_some_and(|seat| seat.session == *session);
                if !self.router.admits(&self.pair.remote) || !(*last || held) {
                    telemetry::seat_traffic_refused();
                    return;
                }
            }
            Purpose::RoomReply | Purpose::Notice { .. } => {
                if !self.router.admits(&self.pair.remote) {
                    telemetry::seat_traffic_refused();
                    return;
                }
            }
            Purpose::Request { .. }
            | Purpose::Response { .. }
            | Purpose::Reply
            | Purpose::Room { .. }
            | Purpose::Departure
            | Purpose::Verify { .. } => {}
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
            Purpose::Verify { target, pending } => {
                if pending.expires_at_ms <= now {
                    return self.router.answer(queued, Failure::Expired, false).await;
                }
                match self.router.correlations.register(pending, now).await {
                    Ok(()) => {
                        self.router
                            .mark_verification_registered(target, &pending.key);
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
            | Purpose::Departure
            | Purpose::Seat { .. }
            | Purpose::RoomReply
            | Purpose::Notice { .. } => false,
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
                // response or its expiry. A room's stanza for a seat may be
                // lost, so the seat ends and the occupant joins again.
                Ok(Err(TransmitError::Unknown)) | Err(_) => {
                    self.stream = None;
                    if let Purpose::Seat {
                        room,
                        occupant,
                        session,
                        last: false,
                    } = &queued.purpose
                    {
                        self.router.seat_undeliverable(room, occupant, *session);
                    }
                    return;
                }
            }
        }
    }
}

mod hosting;
