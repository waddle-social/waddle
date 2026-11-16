//! Hosting local rooms for remote occupants: the router's part (ADR-0020
//! D10; XEP-0045).
//!
//! Inbound, a stanza for the MUC service is counted against its remote
//! domain's request rate and its account's share of it, is placed (where a
//! deployment of several replicas keeps a room on one of them) off the
//! workers that serve the room host, and then waits for the host on a
//! bounded queue, in its room's order. A join opens a seat within the seat
//! bounds, and only where a place for the room's stanzas to the occupant's
//! domain exists, before the host decides it; the seat stays only if the
//! room seated the occupant.
//!
//! Outbound, a local room's stanza leaves for a remote JID only as the
//! answer to that JID's own request or for a seat this replica holds, one
//! stanza for each seat, on a bounded queue of the seat's domain pair. A
//! stanza for a seat that cannot be queued or handed to the stream ends the
//! seat, so an occupant is never left seated behind a gap; chat states and
//! markers, which nothing depends on, are dropped first.
//!
//! A seat also ends when the room's own presence says so and the room
//! confirms that it no longer seats that occupancy, when a stream of its
//! domain pair is lost, when the occupant's server answers the room with a
//! delivery error, when the room no longer seats the occupant, when this
//! replica takes it over from one that stopped, and when this replica
//! stops, which tells the occupant so.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::muc::presence::NS_MUC_USER;
use waddle_xmpp::xep::xep0085::NS_CHATSTATES;
use waddle_xmpp::xep::xep0333::NS_CHAT_MARKERS;
use waddle_xmpp::xep::xep0334::NS_HINTS;
use waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::xep0359::NS_SID;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::muc::user::{Affiliation, Item, MucUser, Role, Status};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::{
    duration_ms, lock, now_ms, Allowance, Hosting, Inner, Pair, Purpose, Queued, SWEEP_BATCH,
};
use crate::destination::ServedService;
use crate::permissions::host_grant::StreamEvidence;
use crate::s2s::conn::{SinkVerdict, SinkWait, VerifiedPeerStream, Waited};
use crate::s2s::route::local_rooms::{
    CarriedRoomOutcome, CarriedRoomRequest, RoomPlacement, RoomRequest, SeatChange, SeatEnd,
};
use crate::s2s::route::placement::{Offered, Waiting};
use crate::s2s::route::principal::RemotePrincipal;
use crate::s2s::route::reach::{reach, InboundReach, LocalTarget};
use crate::s2s::route::reach::{Reachable, RoomOperation};
use crate::s2s::route::replies;
use crate::s2s::route::room_delivery::{LocalRoomRequest, Placed, RoomWork};
use crate::s2s::route::seats::{Deferred, Seat, SeatInsert, SeatLimits, SeatStoreError};
use crate::s2s::route::telemetry;

/// XEP-0045 status codes (§12) the seat reads in the room's own presence.
const STATUS_SELF: &str = "110";
const STATUS_NEW_NICK: &str = "303";

/// Remote domains whose request allowance is kept before idle ones are
/// forgotten.
const ALLOWANCES: usize = 4096;

/// Remote accounts whose share of their domain's request allowance is
/// kept. A domain keeps an account's entry only while the account spends,
/// so the entries of every admitted domain together stay far below this.
const ACCOUNT_ALLOWANCES: usize = 16_384;

/// Rooms and senders whose allowance of chat states and markers for remote
/// seats is kept.
const EPHEMERAL_ALLOWANCES: usize = 4096;

/// A sender's share of chat states and markers, and a room's, may be spent
/// at once up to this many seconds of its rate.
const EPHEMERAL_BURST_SECONDS: f64 = 4.0;

/// Of a remote domain's request rate, the part its refused accounts'
/// stanzas may be answered at: one error for this many requests.
const REFUSALS_PER_REQUEST: f64 = 0.1;

/// Rooms whose seats one pass checks against the room.
const RECONCILE_ROOMS: usize = 256;

/// How often the wait for a stopping replica's last stanzas looks at its
/// queues.
const NOTICE_POLL: Duration = Duration::from_millis(20);

/// Until shutdown: hand the work of one shard of local rooms to the room
/// host, one item at a time, so a room decides its requests in the order
/// they arrived.
pub(super) async fn serve_local_rooms(router: Arc<Inner>, shard: usize) {
    loop {
        let work = tokio::select! {
            biased;
            () = router.shutdown.cancelled() => return,
            work = router.local_rooms.next(shard) => work,
        };
        match work {
            RoomWork::Request(request) => router.decide_room_request(*request).await,
            RoomWork::Unseat(ended) => {
                let (seat, end) = *ended;
                router.unseat(seat, end).await;
            }
            RoomWork::Departure(left) => {
                let (seat, stanza) = *left;
                router.confirm_departure(seat, stanza).await;
            }
            // Work of the queues that serve remote rooms.
            RoomWork::Traffic(_) | RoomWork::Ended(_) => {}
        }
    }
}

/// Place the requests waiting for `room`, oldest first, until none waits.
pub(super) async fn place_room(router: Arc<Inner>, room: BareJid) {
    while let Some(waiting) = router.placements.next(&room) {
        // A stopping replica places nothing more; the lane is emptied so
        // that it closes.
        if router.shutdown.is_cancelled() {
            continue;
        }
        router.place_request(&room, waiting).await;
    }
}

/// The `muc#user` element of a presence, if any.
fn muc_user(stanza: &Stanza) -> Option<&Element> {
    let Stanza::Presence(presence) = stanza else {
        return None;
    };
    presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
}

fn has_status(user: &Element, code: &str) -> bool {
    user.children()
        .any(|child| child.is("status", NS_MUC_USER) && child.attr("code") == Some(code))
}

/// Whether `stanza` is the room's own presence telling the occupant that
/// its occupancy ended: unavailable presence with status 110 that is not a
/// nickname change (XEP-0045 §7.14, §8.2, §9.1, §9.4, §10.9).
fn ends_seat(stanza: &Stanza) -> bool {
    let Stanza::Presence(presence) = stanza else {
        return false;
    };
    presence.type_ == PresenceType::Unavailable
        && muc_user(stanza)
            .is_some_and(|user| has_status(user, STATUS_SELF) && !has_status(user, STATUS_NEW_NICK))
}

/// The nickname the room's own presence says the occupant now holds
/// (XEP-0045 §7.6: unavailable presence with statuses 303 and 110 whose
/// item names the new nickname).
fn new_nick(stanza: &Stanza) -> Option<String> {
    let user = muc_user(stanza)?;
    if !(has_status(user, STATUS_SELF) && has_status(user, STATUS_NEW_NICK)) {
        return None;
    }
    user.children()
        .find(|child| child.is("item", NS_MUC_USER))
        .and_then(|item| item.attr("nick"))
        .map(str::to_owned)
}

/// The identifier `room`'s archive gave a `groupchat` message (XEP-0359),
/// which every copy of it carries.
fn archive_id(stanza: &Stanza, room: &BareJid) -> Option<String> {
    let Stanza::Message(message) = stanza else {
        return None;
    };
    if message.type_ != MessageType::Groupchat {
        return None;
    }
    waddle_xmpp_core::xep0359::extract_stanza_ids(message)
        .into_iter()
        .find(|id| id.by == *room)
        .map(|id| id.id)
}

/// Whether `stanza` is a room message that carries a chat state (XEP-0085)
/// or a marker that answers a message (XEP-0333) and nothing an occupant
/// would miss: no text, no subject and no other payload than the
/// identifiers the room stamps, the thread it belongs to and processing
/// hints. An occupant that does not receive one misses nothing it could
/// not do without, so these are what a congested path drops first.
fn is_ephemeral(stanza: &Stanza) -> bool {
    let Stanza::Message(message) = stanza else {
        return false;
    };
    if message.type_ != MessageType::Groupchat
        || !message.bodies.is_empty()
        || !message.subjects.is_empty()
    {
        return false;
    }
    let mut signal = false;
    for payload in &message.payloads {
        let ns = payload.ns();
        if ns == NS_CHATSTATES || (ns == NS_CHAT_MARKERS && payload.name() != "markable") {
            signal = true;
        } else if ns != NS_SID
            && ns != NS_OCCUPANT_ID
            && ns != NS_HINTS
            && payload.name() != "thread"
        {
            return false;
        }
    }
    signal
}

/// The nickname a presence asks for: available presence to an occupant
/// JID (XEP-0045 §7.2). From a sender without a seat it is a join.
fn presence_nick(stanza: &Stanza, to: &Jid) -> Option<String> {
    let Stanza::Presence(presence) = stanza else {
        return None;
    };
    if presence.type_ != PresenceType::None || to.node().is_none() {
        return None;
    }
    to.resource().map(|nick| nick.as_str().to_owned())
}

/// Whether `stanza` is an occupant's server telling the room that its
/// traffic does not reach the occupant: an error with one of the
/// delivery-related conditions of XEP-0045 §17.1.2.
fn is_delivery_error(stanza: &Stanza) -> bool {
    let payloads = match stanza {
        Stanza::Presence(presence) if presence.type_ == PresenceType::Error => &presence.payloads,
        Stanza::Message(message) if message.type_ == MessageType::Error => &message.payloads,
        Stanza::Presence(_) | Stanza::Message(_) | Stanza::Iq(_) => return false,
    };
    payloads
        .iter()
        .filter(|payload| payload.name() == "error")
        .filter_map(|payload| StanzaError::try_from(payload.clone()).ok())
        .any(|error| {
            matches!(
                error.defined_condition,
                DefinedCondition::Gone { .. }
                    | DefinedCondition::ItemNotFound
                    | DefinedCondition::RecipientUnavailable
                    | DefinedCondition::Redirect { .. }
                    | DefinedCondition::RemoteServerNotFound
                    | DefinedCondition::RemoteServerTimeout
            )
        })
}

/// Whether `stanza` counts against its account's share of its domain's
/// request rate. A leave and an error an occupant's server sends for it
/// end a seat and are always taken; they count against the domain alone.
fn counts_against_account(stanza: &Stanza) -> bool {
    match stanza {
        Stanza::Iq(_) => true,
        Stanza::Message(message) => message.type_ != MessageType::Error,
        Stanza::Presence(presence) => presence.type_ == PresenceType::None,
    }
}

/// Whether a stanza refused for its account's spent share is answered: a
/// request that expects an answer and a message that carries something. A
/// chat state, a marker and presence are dropped unanswered.
fn answers_refusal(stanza: &Stanza) -> bool {
    match stanza {
        Stanza::Iq(_) => true,
        Stanza::Message(_) => !is_ephemeral(stanza),
        Stanza::Presence(_) => false,
    }
}

fn stanza_from(stanza: &Stanza) -> Option<&Jid> {
    replies::addresses(stanza).0
}

fn set_to(stanza: &mut Stanza, to: Jid) {
    match stanza {
        Stanza::Iq(iq) => *iq.to_mut() = Some(to),
        Stanza::Message(message) => message.to = Some(to),
        Stanza::Presence(presence) => presence.to = Some(to),
    }
}

/// The token bucket of `key` among `buckets`, refilled to `now` at `rate`
/// a second up to `burst`. When `buckets` holds `cap` entries and none for
/// `key`, the entries that have refilled are forgotten first; `None` when
/// every entry is still spending, so that the table never grows beyond
/// `cap`.
fn bucket<'a, K: Hash + Eq + Clone>(
    buckets: &'a mut HashMap<K, Allowance>,
    key: &K,
    rate: f64,
    burst: f64,
    now: Instant,
    cap: usize,
) -> Option<&'a mut Allowance> {
    if buckets.len() >= cap && !buckets.contains_key(key) {
        buckets.retain(|_, allowance| {
            allowance.tokens + now.duration_since(allowance.refilled).as_secs_f64() * rate < burst
        });
        if buckets.len() >= cap {
            return None;
        }
    }
    // An answered refusal costs one of these; they refill at a tenth of
    // the rate.
    let refusals = (burst * REFUSALS_PER_REQUEST).max(1.0);
    let allowance = buckets.entry(key.clone()).or_insert(Allowance {
        tokens: burst,
        refilled: now,
        refusals,
    });
    let elapsed = now.duration_since(allowance.refilled).as_secs_f64();
    allowance.tokens = (allowance.tokens + elapsed * rate).min(burst);
    allowance.refusals = (allowance.refusals + elapsed * rate * REFUSALS_PER_REQUEST).min(refusals);
    allowance.refilled = now;
    Some(allowance)
}

/// What became of one stanza's claim on the request allowances.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Spend {
    /// A stanza of the domain's rate and of the account's share was taken.
    Taken,
    /// The domain's rate is spent for this long.
    Wait(Duration),
    /// The account has spent its share: the stanza is not taken, and is
    /// answered with an error only when `answered`.
    AccountSpent { answered: bool },
}

/// The chat states and markers rooms and their senders may still have
/// queued for remote seats.
#[derive(Debug, Default)]
pub(super) struct EphemeralBudgets {
    rooms: HashMap<BareJid, Allowance>,
    senders: HashMap<Jid, Allowance>,
}

impl Inner {
    /// What this replica lets remote senders reach beyond the server
    /// domain: the MUC service, once a room host is attached.
    pub(super) fn reachable(&self) -> Reachable {
        Reachable {
            rooms: self.hosting.get().is_some(),
        }
    }

    /// Whether peer admission admits `remote` now (ADR-0020 D1).
    pub(super) fn admits(&self, remote: &CanonicalDomain) -> bool {
        self.config.admission().decide(remote).is_ok()
    }

    fn seat_limits(&self) -> SeatLimits {
        SeatLimits {
            per_room: self.limits.seats_per_room,
            per_peer: self.limits.seats_per_peer,
            per_account: self.limits.seats_per_account,
            total: self.limits.seats,
        }
    }

    fn busy(&self, text: &str) -> StanzaError {
        replies::error(
            self.home(),
            ErrorType::Wait,
            DefinedCondition::ResourceConstraint,
            text,
        )
    }

    // --- inbound requests ---------------------------------------------------

    /// Take a stanza for the MUC service: for the stage that places it, in
    /// a deployment that keeps rooms on one replica each, or else for the
    /// room host's queue. A stanza that finds its domain's rate spent or
    /// the queues full holds its stream until it fits. A stanza beyond its
    /// account's share of the domain's rate is not taken at all: the
    /// stream goes on, and the other accounts of the domain with it.
    pub(super) fn room_request(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        principal: RemotePrincipal,
        operation: RoomOperation,
        stanza: Stanza,
    ) -> SinkVerdict {
        let Some(hosting) = self.hosting.get() else {
            return SinkVerdict::Accepted;
        };
        let Some(to) = replies::addresses(&stanza).1 else {
            return SinkVerdict::Accepted;
        };
        let room = to.to_bare();
        // The service itself is every replica's; only a room is held by
        // one of them.
        let staged = to.node().is_some() && hosting.host.carries();
        let peer = stream.remote().as_domain_ref().as_str().to_owned();
        let pair = Pair {
            local: stream.local().clone(),
            remote: stream.remote().clone(),
        };
        let account =
            counts_against_account(&stanza).then(|| principal.account().as_bare_jid().clone());
        let answers = answers_refusal(&stanza);
        let mut request = LocalRoomRequest {
            principal,
            operation,
            stanza,
            local: pair.local.clone(),
            remote: pair.remote.clone(),
        };
        let mut pause = match self.take_allowance(&peer, account.as_ref(), answers) {
            Spend::Taken => None,
            Spend::Wait(wait) => Some(wait),
            Spend::AccountSpent { answered } => {
                self.refuse_spent(&pair, &request.stanza, answered);
                return SinkVerdict::Accepted;
            }
        };
        if pause.is_none() {
            match self.admit(&room, request, &peer, staged) {
                Ok(()) => return SinkVerdict::Accepted,
                Err(refused) => request = *refused,
            }
        }
        telemetry::room_request_waited();
        let router = Arc::clone(self);
        let deadline = tokio::time::Instant::now() + self.limits.room_request_wait;
        SinkVerdict::Wait(SinkWait::new(async move {
            while let Some(wait) = pause {
                if tokio::time::Instant::now() + wait > deadline {
                    telemetry::room_request_overloaded();
                    return Waited::Overloaded;
                }
                tokio::select! {
                    biased;
                    () = router.shutdown.cancelled() => return Waited::Stopping,
                    () = tokio::time::sleep(wait) => {}
                }
                pause = match router.take_allowance(&peer, account.as_ref(), answers) {
                    Spend::Taken => None,
                    Spend::Wait(wait) => Some(wait),
                    Spend::AccountSpent { answered } => {
                        router.refuse_spent(&pair, &request.stanza, answered);
                        return Waited::Taken;
                    }
                };
            }
            let placed = if staged {
                let waiting = Waiting {
                    request,
                    peer: peer.clone(),
                    arrived: tokio::time::Instant::now(),
                };
                let (placed, started) = router
                    .placements
                    .offer_or_wait(&room, waiting, deadline, &router.shutdown)
                    .await;
                if started {
                    router
                        .tasks
                        .spawn(place_room(Arc::clone(&router), room.clone()));
                }
                placed
            } else {
                let work = RoomWork::Request(Box::new(request));
                router
                    .local_rooms
                    .push_or_wait(room, work, &peer, deadline, &router.shutdown)
                    .await
            };
            match placed {
                Placed::Queued => Waited::Taken,
                Placed::TimedOut => {
                    telemetry::room_request_overloaded();
                    Waited::Overloaded
                }
                Placed::Stopped => Waited::Stopping,
            }
        }))
    }

    /// Queue `request` without waiting: for placement when the host keeps
    /// rooms on other replicas (`staged`), or else for the room host.
    fn admit(
        self: &Arc<Self>,
        room: &BareJid,
        request: LocalRoomRequest,
        peer: &str,
        staged: bool,
    ) -> Result<(), Box<LocalRoomRequest>> {
        if !staged {
            let work = RoomWork::Request(Box::new(request));
            return match self.local_rooms.try_push(room.clone(), work, peer) {
                Ok(()) => Ok(()),
                Err(RoomWork::Request(refused)) => Err(refused),
                // Only the work offered is handed back.
                Err(_) => Ok(()),
            };
        }
        let waiting = Waiting {
            request,
            peer: peer.to_owned(),
            arrived: tokio::time::Instant::now(),
        };
        match self.placements.offer(room, waiting) {
            Offered::Queued => Ok(()),
            Offered::Started => {
                self.tasks.spawn(place_room(Arc::clone(self), room.clone()));
                Ok(())
            }
            Offered::Refused(refused) => Err(Box::new(refused.request)),
        }
    }

    /// Find where one request for `room` is decided and hand it there: to
    /// this replica's room host, or to the replica that holds the room,
    /// which then took it. The whole of it, waiting for a place among the
    /// rooms being placed included, is bounded by the placement timeout; a
    /// request whose holder does not answer within it is answered with
    /// `resource-constraint`, and so is everything else that waits for the
    /// room then.
    async fn place_request(self: &Arc<Self>, room: &BareJid, waiting: Waiting) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let Waiting {
            request,
            peer,
            arrived,
        } = waiting;
        if self.placements.silent(room) {
            telemetry::room_request_not_carried();
            self.refuse_unplaced(&request, self.busy(HOLDER_SILENT));
            return;
        }
        let placing = async {
            let _place = self.placements.place(&peer).await?;
            Some(
                hosting
                    .host
                    .place(&request.principal, &request.stanza)
                    .await,
            )
        };
        let placed = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return,
            placed = tokio::time::timeout(self.limits.room_placement_timeout, placing) => placed,
        };
        match placed {
            Ok(Some(RoomPlacement::Here)) => {
                let refusal = replies::error_reply(
                    &request.stanza,
                    self.busy("The room is busy; try again later."),
                );
                let pair = Pair {
                    local: request.local.clone(),
                    remote: request.remote.clone(),
                };
                let deadline = arrived + self.limits.room_request_wait;
                let work = RoomWork::Request(Box::new(request));
                let queued = self
                    .local_rooms
                    .push_or_wait(room.clone(), work, &peer, deadline, &self.shutdown)
                    .await;
                // The stream that carried the stanza is not waiting for
                // it any more, so a stanza that finds no place in time is
                // answered rather than its stream closed.
                if queued == Placed::TimedOut {
                    telemetry::room_request_overloaded();
                    if let Some(refusal) = refusal {
                        self.offer_room_reply(&pair, refusal);
                    }
                }
            }
            Ok(Some(RoomPlacement::Carried)) => telemetry::room_request_carried(),
            Ok(Some(RoomPlacement::Refused(error))) => {
                telemetry::room_request_not_carried();
                self.refuse_unplaced(&request, *error);
            }
            // The stage is closed: the replica is stopping.
            Ok(None) => {}
            Err(_) => {
                telemetry::room_placement_timed_out();
                self.placements.mark_silent(
                    room,
                    tokio::time::Instant::now() + self.limits.room_placement_backoff,
                );
                self.refuse_unplaced(&request, self.busy(HOLDER_SILENT));
                // What waited behind it asked the same holder.
                for behind in self.placements.drain(room) {
                    telemetry::room_request_not_carried();
                    self.refuse_unplaced(&behind.request, self.busy(HOLDER_SILENT));
                }
            }
        }
    }

    /// Answer a request that was not placed with `error`, if a place for
    /// the answer is free.
    fn refuse_unplaced(self: &Arc<Self>, request: &LocalRoomRequest, error: StanzaError) {
        let Some(reply) = replies::error_reply(&request.stanza, error) else {
            return;
        };
        let pair = Pair {
            local: request.local.clone(),
            remote: request.remote.clone(),
        };
        self.offer_room_reply(&pair, reply);
    }

    /// Answer a stanza that was not taken because its account had spent
    /// its share of its domain's request rate, when it is one that is
    /// answered and the domain's allowance of such answers allowed it.
    fn refuse_spent(self: &Arc<Self>, pair: &Pair, stanza: &Stanza, answered: bool) {
        telemetry::room_request_account_limited();
        if !answered {
            return;
        }
        let error = self.busy("Too many requests from this account; try again later.");
        if let Some(reply) = replies::error_reply(stanza, error) {
            self.offer_room_reply(pair, reply);
        }
    }

    /// Queue a stanza another replica verified on an inbound stream and
    /// carried here because this replica holds the room it addresses. It
    /// is queued behind the room's earlier requests, against its remote
    /// domain's share of the queues, and decided as one from a stream of
    /// this replica would be: the host decides peer admission and the
    /// sender's standing again, under the snapshot this replica runs, on
    /// the carried facts of the stream. It is never placed again.
    ///
    /// The carrying replica spent its domain's request rate on the stanza;
    /// it is not spent again here.
    pub(super) fn carried_room_request(&self, request: CarriedRoomRequest) -> CarriedRoomOutcome {
        if self.hosting.get().is_none() {
            return CarriedRoomOutcome::Refused;
        }
        let CarriedRoomRequest { principal, stanza } = request;
        let Ok(principal) = RemotePrincipal::carried(principal) else {
            return CarriedRoomOutcome::Refused;
        };
        // The stanza's sender is the principal, and it addresses a room at
        // the MUC service this replica serves: what a stream of this
        // replica is checked for before its stanza reaches the router. A
        // local domain that is not the MUC service's hosts no room.
        let (from, to) = replies::addresses(&stanza);
        let (Some(from), Some(to)) = (from, to) else {
            return CarriedRoomOutcome::Refused;
        };
        if from != principal.address() || to.node().is_none() {
            return CarriedRoomOutcome::Refused;
        }
        let Ok(local) = CanonicalDomain::from_domain(to.domain()) else {
            return CarriedRoomOutcome::Refused;
        };
        let muc = LocalTarget::Service(ServedService::Muc);
        let target = LocalTarget::of(&self.served.classify(Some(to)));
        if target != Some(muc) || !self.config.is_local(&local) {
            return CarriedRoomOutcome::Refused;
        }
        let InboundReach::Room(operation) = reach(muc, &stanza, self.reachable()) else {
            return CarriedRoomOutcome::Refused;
        };
        let room = to.to_bare();
        let remote = principal.home().clone();
        let peer = remote.as_domain_ref().as_str().to_owned();
        let work = RoomWork::Request(Box::new(LocalRoomRequest {
            principal,
            operation,
            stanza,
            local,
            remote,
        }));
        match self.local_rooms.try_push(room, work, &peer) {
            Ok(()) => CarriedRoomOutcome::Queued,
            Err(_) => CarriedRoomOutcome::Busy,
        }
    }

    /// Take one stanza of `peer`'s allowance for the MUC service and, for
    /// a stanza that counts against one, of `account`'s share of it; or
    /// say how long until the domain's rate allows the next, or that the
    /// account has spent its share. `answers` says whether the stanza is
    /// one whose refusal is answered.
    fn take_allowance(&self, peer: &str, account: Option<&BareJid>, answers: bool) -> Spend {
        let rate = f64::from(self.limits.room_request_rate.max(1));
        let burst = f64::from(self.limits.room_request_burst.max(1));
        let account_rate = f64::from(self.limits.room_request_rate_per_account.max(1)).min(rate);
        let account_burst = f64::from(self.limits.room_request_burst_per_account.max(1)).min(burst);
        let now = Instant::now();
        let mut allowances = lock(&self.allowances);
        let mut accounts = lock(&self.account_allowances);
        let peer = peer.to_owned();
        // A domain table full of spending domains cannot be: an admitted
        // domain has an entry only while it spends, and far fewer are
        // admitted than the table holds.
        let Some(of_peer) = bucket(&mut allowances, &peer, rate, burst, now, ALLOWANCES) else {
            return Spend::Wait(Duration::from_secs_f64(1.0 / rate));
        };
        let of_account = match account {
            Some(account) => {
                let share = bucket(
                    &mut accounts,
                    account,
                    account_rate,
                    account_burst,
                    now,
                    ACCOUNT_ALLOWANCES,
                );
                match share {
                    Some(share) if share.tokens >= 1.0 => Some(share),
                    // Spent, or no entry can be kept for it.
                    _ => {
                        let answered = answers && of_peer.refusals >= 1.0;
                        if answered {
                            of_peer.refusals -= 1.0;
                        }
                        return Spend::AccountSpent { answered };
                    }
                }
            }
            None => None,
        };
        if of_peer.tokens < 1.0 {
            return Spend::Wait(Duration::from_secs_f64((1.0 - of_peer.tokens) / rate));
        }
        of_peer.tokens -= 1.0;
        if let Some(share) = of_account {
            share.tokens -= 1.0;
        }
        Spend::Taken
    }

    /// Have the room host decide one stanza and queue its answer for the
    /// sender.
    async fn decide_room_request(self: &Arc<Self>, request: LocalRoomRequest) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let LocalRoomRequest {
            principal,
            operation,
            stanza,
            local,
            remote,
        } = request;
        let Some(to) = replies::addresses(&stanza).1.cloned() else {
            return;
        };
        let room = to.to_bare();
        let pair = Pair { local, remote };
        let occupant = principal.full();
        let mut seat = occupant
            .as_ref()
            .and_then(|occupant| self.seats.get(&room, occupant));
        if let Some(held) = &seat {
            if is_delivery_error(&stanza) {
                self.unseat(held.clone(), SeatEnd::OccupantError).await;
                return;
            }
            // A seat stands for what the stream that carried its join
            // proved. A stanza a stream of other strength or trust profile
            // carries is not the seat's: the seat ends, and the stanza is
            // decided as one from an entity without a seat, so presence
            // enters again on the evidence of the stream that carries it
            // now.
            if held.evidence != StreamEvidence::of(&principal) {
                self.unseat(held.clone(), SeatEnd::EvidenceChanged).await;
                seat = None;
            }
        }
        let sender = principal.address().clone();
        let nick = presence_nick(&stanza, &to);
        // The room's messages handed to the sender's seat from here on are
        // those this request caused.
        let mut copies = occupant
            .as_ref()
            .map_or(0, |occupant| self.seats.copy_mark(&room, occupant));
        // The seat the occupant held when its presence turned out to enter
        // the room again: the room may still seat that occupancy.
        let mut given_up: Option<Seat> = None;
        let mut joining = false;
        let answer = loop {
            if seat.is_none() {
                if let (Some(occupant), Some(nick)) = (occupant.as_ref(), nick.clone()) {
                    let opened = self
                        .open_seat(hosting, &principal, &pair, &room, occupant, nick)
                        .await;
                    match opened {
                        Ok(opened) => {
                            seat = Some(opened);
                            joining = true;
                            copies = 0;
                        }
                        Err(error) => {
                            if let Some(reply) = replies::error_reply(&stanza, *error) {
                                self.queue_room_reply(&pair, reply).await;
                            }
                            if let Some(left) = given_up {
                                self.unseat(left, SeatEnd::Refused).await;
                            }
                            return;
                        }
                    }
                }
            }
            let handled = hosting.host.handle(RoomRequest {
                principal: principal.clone(),
                operation,
                stanza: stanza.clone(),
                seat: seat.clone(),
                joining,
            });
            let Ok(answer) = tokio::time::timeout(self.limits.room_request_timeout, handled).await
            else {
                telemetry::room_request_timed_out();
                // The room may have seated the occupant before the host was
                // abandoned.
                if let Some(opened) = seat.filter(|_| joining) {
                    self.unseat(opened, SeatEnd::Undeliverable).await;
                }
                if let Some(left) = given_up {
                    self.unseat(left, SeatEnd::Refused).await;
                }
                return;
            };
            // Presence of a seated occupant that enters the room again is
            // decided as a join: a seat is opened for it, held as pending,
            // so the room's answer (the occupants, then the occupant's own
            // presence) precedes whatever the room sends the new seat
            // (XEP-0045 §7.1, §7.2.3). The seat the occupant held is not
            // given up first: the one opened for the join takes its row
            // and its place over, so the room's traffic finds a seat
            // throughout.
            if answer.seat == SeatChange::Rejoin && !joining && given_up.is_none() {
                if let Some(held) = seat.take() {
                    given_up = Some(held);
                    continue;
                }
            }
            break answer;
        };
        // A seat stays only if the room seats the occupant. One opened for
        // a join the room did not accept is dropped. One the occupant held
        // before, for which the host refused the occupant itself, ends, and
        // the room removes the occupant: without that the room would keep
        // an occupant nothing can reach.
        let mut refused: Option<Seat> = None;
        match (answer.seat, joining) {
            (SeatChange::Seated, _) => {}
            (_, true) => {
                if let Some(opened) = seat.take() {
                    self.close_seat(hosting, &opened).await;
                }
                joining = false;
            }
            (SeatChange::Refused, false) => refused = seat.take(),
            (SeatChange::Unchanged | SeatChange::Rejoin, false) => {}
        }
        if answer.seat != SeatChange::Seated {
            // The room did not seat the occupant again: it must not keep
            // the occupancy whose seat was given up for the join.
            refused = refused.or(given_up);
        }
        let mut answered = true;
        for mut reply in answer.stanzas {
            // Only what the MUC service sends the entity it answers leaves.
            let from_service = stanza_from(&reply).is_some_and(|from| {
                CanonicalDomain::from_domain(from.domain()).is_ok_and(|domain| domain == pair.local)
            });
            if !from_service {
                telemetry::seat_traffic_refused();
                continue;
            }
            // A copy of the room's message that was handed to the seat at
            // its place in the room's fan-out, while this request was
            // decided, is not sent again as the answer.
            if let (Some(held), Some(id)) = (&seat, archive_id(&reply, &room)) {
                if !self
                    .seats
                    .answers_with(&held.room, &held.occupant, &id, copies)
                {
                    continue;
                }
            }
            set_to(&mut reply, sender.clone());
            let last = seat.is_some() && ends_seat(&reply);
            if let (Some(held), Some(nick)) = (&seat, new_nick(&reply)) {
                self.rename_seat(hosting, held, &nick).await;
            }
            if !self.queue_room_reply(&pair, reply).await {
                // The sender missed part of its answer: a seated occupant
                // cannot be kept in step and joins again.
                answered = false;
                break;
            }
            if last {
                if let Some(held) = seat.take() {
                    self.close_seat(hosting, &held).await;
                }
            }
        }
        if let Some(left) = refused {
            self.unseat(left, SeatEnd::Refused).await;
        }
        if !answered {
            if let Some(held) = seat.take() {
                self.unseat(held, SeatEnd::Undeliverable).await;
            }
            return;
        }
        // The join's answer is queued: what the room sent the occupant
        // meanwhile follows it, in order.
        if let Some(held) = seat.filter(|_| joining) {
            while let Some(kept) = self
                .seats
                .settle(&held.room, &held.occupant, held.session)
                .filter(|kept| !kept.is_empty())
            {
                for stanza in kept {
                    self.queue_seat(&held, stanza);
                }
            }
        }
    }

    /// Open the seat a join asks for, within the seat bounds, and hold it
    /// while the join is decided. The error is the join's answer.
    async fn open_seat(
        self: &Arc<Self>,
        hosting: &Hosting,
        principal: &RemotePrincipal,
        pair: &Pair,
        room: &BareJid,
        occupant: &FullJid,
        nick: String,
    ) -> Result<Seat, Box<StanzaError>> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(Box::new(
                self.busy("This server is shutting down; try again later."),
            ));
        }
        // A seat is opened only where the room's stanzas for it can leave:
        // a place on a queue for the occupant's domain, which a join opens
        // within the bound on queues remote senders open. Without one the
        // seat would end at the room's first stanza for it.
        let reachable =
            self.reserve_on(pair.clone(), self.limits.reply_queues, |queue| &queue.seats);
        if reachable.is_none() {
            telemetry::seat_refused_unreachable();
            return Err(Box::new(self.busy(
                "This server holds no place for more traffic to your server now; try again later.",
            )));
        }
        drop(reachable);
        let seat = Seat {
            room: room.clone(),
            occupant: occupant.clone(),
            nick,
            session: OccupancySessionGeneration::mint(),
            evidence: StreamEvidence::of(principal),
            snapshot: principal.generation(),
            owner: self.owner.as_str().to_owned(),
            lost_at_ms: None,
            updated_at_ms: now_ms(),
        };
        let limits = self.seat_limits();
        // A row for the same room and occupant is one this replica left
        // behind or one of a replica that stopped, which is replaced, or
        // one a live replica holds, which is that replica's.
        for _ in 0..2 {
            match hosting.store.insert(&seat, &limits).await {
                Ok(SeatInsert::Inserted) => {
                    self.seats.hold_pending(seat.clone());
                    return Ok(seat);
                }
                Ok(SeatInsert::Bounds) => {
                    return Err(Box::new(
                        self.busy("Too many occupants from other servers; try again later."),
                    ));
                }
                Ok(SeatInsert::Exists) => {
                    let existing = hosting.store.get(room, occupant).await;
                    let stale = match existing {
                        Ok(None) => continue,
                        Ok(Some(row)) if row.owner == seat.owner => Some(row),
                        Ok(Some(_)) => {
                            let now = now_ms();
                            let before =
                                now.saturating_sub(duration_ms(self.limits.seat_owner_grace));
                            hosting
                                .store
                                .adopt(room, occupant, &seat.owner, before, now)
                                .await
                                .ok()
                                .flatten()
                        }
                        Err(_) => None,
                    };
                    let Some(stale) = stale else {
                        return Err(Box::new(self.busy(
                            "This occupant is seated through another server node; try again later.",
                        )));
                    };
                    if hosting.store.remove(&stale).await.is_err() {
                        break;
                    }
                }
                Err(SeatStoreError::TooLong) => {
                    return Err(Box::new(replies::error(
                        self.home(),
                        ErrorType::Modify,
                        DefinedCondition::NotAcceptable,
                        "The address or nickname is too long.",
                    )));
                }
                Err(SeatStoreError::Storage(_)) => break,
            }
        }
        tracing::warn!("opening a remote occupant's seat failed");
        Err(Box::new(replies::error(
            self.home(),
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "The room could not be joined now; try again later.",
        )))
    }

    /// Stop holding `seat` and remove its row: the room does not seat the
    /// occupant, or has just told it that its occupancy ended. A row that
    /// could not be removed is removed when this replica's rows are next
    /// checked against the seats it holds.
    async fn close_seat(&self, hosting: &Hosting, seat: &Seat) {
        self.seats.release(&seat.room, &seat.occupant, seat.session);
        if hosting.store.remove(seat).await.is_err() {
            tracing::warn!("removing a remote occupant's seat failed");
        }
    }

    async fn rename_seat(&self, hosting: &Hosting, seat: &Seat, nick: &str) {
        self.seats.rename(&seat.room, &seat.occupant, nick);
        if hosting.store.rename(seat, nick, now_ms()).await.is_err() {
            tracing::debug!("recording a remote occupant's nickname failed");
        }
    }

    /// End `seat` without the room deciding it: the seat is given up and
    /// the room removes the occupant, telling those who stay (XEP-0045
    /// §11.1).
    async fn unseat(self: &Arc<Self>, seat: Seat, end: SeatEnd) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        self.seats.release(&seat.room, &seat.occupant, seat.session);
        // Where the occupant's server may still be reached and was not told
        // by a failed stream or by its own error, the occupant is told that
        // the service removed it (XEP-0045 §11.1), ahead of anything a
        // later join is answered with. It is not waited for: nothing
        // depends on its arrival.
        match end {
            SeatEnd::Undeliverable
            | SeatEnd::EvidenceChanged
            | SeatEnd::RoomClosed
            | SeatEnd::Refused
            | SeatEnd::HolderStopped => self.tell_removed(&seat, Status::ServiceErrorKick),
            SeatEnd::StreamLost | SeatEnd::OccupantError => {}
        }
        if hosting.store.remove(&seat).await.is_err() {
            tracing::warn!("removing an ended remote occupant's seat failed");
        }
        let removal = hosting.host.unseat(&seat, end);
        if tokio::time::timeout(self.limits.room_request_timeout, removal)
            .await
            .is_err()
        {
            tracing::warn!("removing an ended remote occupant from its room timed out");
        }
    }

    /// The pair a room's stanzas for `seat` leave on.
    fn seat_pair(seat: &Seat) -> Option<Pair> {
        let local = CanonicalDomain::from_domain(seat.room.domain()).ok()?;
        let remote = CanonicalDomain::from_domain(seat.occupant.domain()).ok()?;
        Some(Pair { local, remote })
    }

    /// The occupant's own unavailable presence for a seat the service
    /// ended, with status 110 and `status`, and the pair it leaves on.
    fn own_unavailable(seat: &Seat, status: Status) -> Option<(Pair, Stanza)> {
        let from = seat.room.with_resource_str(&seat.nick).ok()?;
        let pair = Self::seat_pair(seat)?;
        let mut presence = Presence::new(PresenceType::Unavailable);
        presence.from = Some(Jid::from(from));
        presence.to = Some(Jid::from(seat.occupant.clone()));
        let user = MucUser::new()
            .with_items(vec![Item::new(Affiliation::None, Role::None)])
            .with_statuses(vec![Status::SelfPresence, status]);
        presence.payloads.push(Element::from(user));
        Some((pair, Stanza::Presence(presence)))
    }

    /// Queue the occupant's own unavailable presence with statuses 110 and
    /// `status` for a seat the service ended, if a place is free for it.
    fn tell_removed(self: &Arc<Self>, seat: &Seat, status: Status) {
        if let Some((pair, stanza)) = Self::own_unavailable(seat, status) {
            self.offer_room_reply(&pair, stanza);
        }
    }

    // --- egress -----------------------------------------------------------

    /// The seat `occupant` holds here in the room `stanza` comes from, for
    /// a stanza a room sends its occupants: presence and groupchat
    /// messages. Private messages, invitations and IQs relayed between
    /// occupants are not a room's traffic to a seat: their answers would be
    /// account traffic, which remote senders do not reach (ADR-0020 D7).
    pub(super) fn seat_of(&self, occupant: &FullJid, stanza: &Stanza) -> Option<Seat> {
        self.hosting.get()?;
        match stanza {
            Stanza::Presence(_) => {}
            Stanza::Message(message) if message.type_ == MessageType::Groupchat => {}
            Stanza::Message(_) | Stanza::Iq(_) => return None,
        }
        let room = stanza_from(stanza)?.to_bare();
        self.seats.get(&room, occupant)
    }

    pub(super) fn offer_to_seat(
        self: &Arc<Self>,
        occupant: &FullJid,
        stanza: Stanza,
    ) -> Result<(), Box<Stanza>> {
        let Some(seat) = self.seat_of(occupant, &stanza) else {
            return Err(Box::new(stanza));
        };
        match self.seats.defer(&seat.room, occupant, stanza) {
            Deferred::Ready(stanza) => self.queue_seat(&seat, stanza),
            Deferred::Kept => {}
            Deferred::Overflow => {
                self.seat_undeliverable(&seat.room, occupant, seat.session);
            }
            Deferred::NotHeld(stanza) => return Err(Box::new(stanza)),
        }
        Ok(())
    }

    /// Take one chat state or marker of `room` from `sender` for a remote
    /// seat out of the room's allowance and the sender's: `false` when
    /// either is spent, and the copy is dropped.
    fn spend_ephemeral(&self, room: &BareJid, sender: Option<&Jid>) -> bool {
        let room_rate = f64::from(self.limits.seat_ephemeral_rate_per_room.max(1));
        let sender_rate = f64::from(self.limits.seat_ephemeral_rate_per_sender.max(1));
        let now = Instant::now();
        let mut budgets = lock(&self.ephemeral);
        let EphemeralBudgets { rooms, senders } = &mut *budgets;
        // A room or a sender no entry can be kept for is not counted: the
        // table holds only those still spending.
        let of_room = bucket(
            rooms,
            room,
            room_rate,
            room_rate * EPHEMERAL_BURST_SECONDS,
            now,
            EPHEMERAL_ALLOWANCES,
        );
        if of_room.as_ref().is_some_and(|of_room| of_room.tokens < 1.0) {
            return false;
        }
        let of_sender = sender.and_then(|sender| {
            bucket(
                senders,
                sender,
                sender_rate,
                sender_rate * EPHEMERAL_BURST_SECONDS,
                now,
                EPHEMERAL_ALLOWANCES,
            )
        });
        if of_sender
            .as_ref()
            .is_some_and(|of_sender| of_sender.tokens < 1.0)
        {
            return false;
        }
        if let Some(of_room) = of_room {
            of_room.tokens -= 1.0;
        }
        if let Some(of_sender) = of_sender {
            of_sender.tokens -= 1.0;
        }
        true
    }

    /// Whether `pair`'s queue of local rooms' stanzas is more than three
    /// quarters full.
    fn seat_queue_congested(&self, pair: &Pair) -> bool {
        lock(&self.queues)
            .get(pair)
            .is_some_and(|queue| queue.seats.capacity() < queue.seats.max_capacity().div_ceil(4))
    }

    /// Queue one stanza of a room for `seat`, without waiting. A copy of a
    /// room message already handed to the seat is not queued again. A chat
    /// state or a marker beyond its room's or its sender's allowance, or
    /// for a queue that is filling, is dropped. Any other stanza that finds
    /// no place ends the seat. The room's own presence that ends the
    /// occupancy is not queued here: it is checked against the room first
    /// ([`Self::confirm_departure`]).
    fn queue_seat(self: &Arc<Self>, seat: &Seat, mut stanza: Stanza) {
        set_to(&mut stanza, Jid::from(seat.occupant.clone()));
        if ends_seat(&stanza) {
            self.local_rooms.push_exempt(
                seat.room.clone(),
                RoomWork::Departure(Box::new((seat.clone(), stanza))),
            );
            return;
        }
        let Some(pair) = Self::seat_pair(seat) else {
            return;
        };
        let ephemeral = is_ephemeral(&stanza);
        if ephemeral
            && (self.seat_queue_congested(&pair)
                || !self.spend_ephemeral(&seat.room, stanza_from(&stanza)))
        {
            telemetry::seat_ephemeral_dropped();
            return;
        }
        if let Some(id) = archive_id(&stanza, &seat.room) {
            if !self.seats.note_copy(&seat.room, &seat.occupant, &id) {
                return;
            }
        }
        let renamed = new_nick(&stanza);
        let queued = Queued {
            stanza,
            purpose: Purpose::Seat {
                room: seat.room.clone(),
                occupant: seat.occupant.clone(),
                session: seat.session,
                last: false,
            },
        };
        let placed = self.reserve_on(pair, self.limits.peer_queues, |queue| &queue.seats);
        match placed {
            Some(permit) => {
                permit.send(queued);
            }
            None if ephemeral => {
                telemetry::seat_ephemeral_dropped();
                return;
            }
            None => {
                self.seat_undeliverable(&seat.room, &seat.occupant, seat.session);
                return;
            }
        }
        if let Some(nick) = renamed {
            let router = Arc::clone(self);
            let seat = seat.clone();
            self.tasks.spawn(async move {
                if let Some(hosting) = router.hosting.get() {
                    router.rename_seat(hosting, &seat, &nick).await;
                }
            });
        }
    }

    /// The room's own presence that tells `seat`'s occupant its occupancy
    /// ended reached the router apart from the occupant's own request: a
    /// kick, a ban, a revoked membership, the room's destruction, or the
    /// same told again for an occupancy that ended earlier. A stanza
    /// carries no occupancy generation, so the room is asked. Only when it
    /// no longer seats the seat's generation is the presence sent and the
    /// seat ended; presence for an earlier occupancy of an occupant that
    /// entered again is dropped, and neither ends the new seat nor reaches
    /// its occupant.
    async fn confirm_departure(self: &Arc<Self>, seat: Seat, stanza: Stanza) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        if !self
            .seats
            .holds_under(&seat.room, &seat.occupant, seat.session)
        {
            return;
        }
        let asked = hosting.host.seats(&seat);
        match tokio::time::timeout(self.limits.room_delivery_timeout, asked).await {
            Ok(Some(false)) => {
                if let Some(pair) = Self::seat_pair(&seat) {
                    let queued = Queued {
                        stanza,
                        purpose: Purpose::Seat {
                            room: seat.room.clone(),
                            occupant: seat.occupant.clone(),
                            session: seat.session,
                            last: true,
                        },
                    };
                    let placed =
                        self.reserve_on(pair, self.limits.peer_queues, |queue| &queue.seats);
                    if let Some(permit) = placed {
                        permit.send(queued);
                    }
                }
                self.close_seat(hosting, &seat).await;
            }
            Ok(Some(true)) => telemetry::seat_departure_stale(),
            // Whether the room seats the occupant cannot be told: the seat
            // ends and the room removes the occupant if it still seats it,
            // so neither is left without the other.
            Ok(None) | Err(_) => self.unseat(seat, SeatEnd::Undeliverable).await,
        }
    }

    /// Queue `stanza`, an answer of the MUC service or its notice to an
    /// occupant whose seat it ended, for the entity's domain without
    /// waiting: dropped when no place is free.
    fn offer_room_reply(self: &Arc<Self>, pair: &Pair, stanza: Stanza) {
        let placed = self.reserve_on(pair.clone(), self.limits.reply_queues, |queue| &queue.seats);
        if let Some(permit) = placed {
            permit.send(Queued {
                stanza,
                purpose: Purpose::RoomReply,
            });
        }
    }

    /// Queue the MUC service's answer to a remote entity's request for the
    /// entity's domain, waiting for a place at most the reply wait: `false`
    /// when it found none and was dropped.
    async fn queue_room_reply(self: &Arc<Self>, pair: &Pair, stanza: Stanza) -> bool {
        let deadline = tokio::time::Instant::now() + self.limits.room_reply_wait;
        let mut queued = Queued {
            stanza,
            purpose: Purpose::RoomReply,
        };
        loop {
            let placed =
                self.reserve_on(pair.clone(), self.limits.reply_queues, |queue| &queue.seats);
            if let Some(permit) = placed {
                permit.send(queued);
                return true;
            }
            // The pair has a queue that is full, or none may be opened for
            // a reply now.
            let Some(sender) = lock(&self.queues)
                .get(pair)
                .map(|queue| queue.seats.clone())
            else {
                return false;
            };
            let waited = tokio::select! {
                biased;
                () = self.shutdown.cancelled() => return false,
                waited = tokio::time::timeout_at(deadline, sender.send(queued)) => waited,
            };
            match waited {
                Ok(Ok(())) => return true,
                // The pair's worker ended meanwhile: open another.
                Ok(Err(returned)) => queued = returned.0,
                Err(_) => return false,
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// A stanza of `room` did not reach the seat `occupant` holds under
    /// `session`: give the seat up now, so that nothing more is queued for
    /// it, and have the room remove the occupant behind what it already
    /// decides.
    pub(super) fn seat_undeliverable(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        session: OccupancySessionGeneration,
    ) {
        let Some(seat) = self.seats.release(room, occupant, session) else {
            return;
        };
        telemetry::seat_ended_undeliverable();
        self.local_rooms.push_exempt(
            room.clone(),
            RoomWork::Unseat(Box::new((seat, SeatEnd::Undeliverable))),
        );
    }

    // --- ends ---------------------------------------------------------------

    /// A stream between a local domain and a remote one failed: every seat
    /// of an occupant at that remote domain in a room at that local domain
    /// ends, since stanzas either way may be lost (XEP-0045 §11.1). Seats
    /// this replica holds end here; those another replica holds are marked
    /// for it.
    pub(super) fn lose_seats(self: &Arc<Self>, stream: &VerifiedPeerStream) {
        if self.hosting.get().is_none() {
            return;
        }
        let room_domain = stream.local().as_domain_ref().as_str().to_owned();
        let occupant_domain = stream.remote().as_domain_ref().as_str().to_owned();
        for seat in self.seats.of_pair(&room_domain, &occupant_domain) {
            self.seats.release(&seat.room, &seat.occupant, seat.session);
            telemetry::seat_ended_stream_lost();
            self.local_rooms.push_exempt(
                seat.room.clone(),
                RoomWork::Unseat(Box::new((seat, SeatEnd::StreamLost))),
            );
        }
        let router = Arc::clone(self);
        self.tasks.spawn(async move {
            let Some(hosting) = router.hosting.get() else {
                return;
            };
            let marked = hosting.store.mark_lost(
                &room_domain,
                &occupant_domain,
                router.owner.as_str(),
                now_ms(),
            );
            if marked.await.is_err() {
                tracing::warn!("marking the seats behind a lost stream failed");
            }
        });
    }

    /// The room stopped being open to accounts at other servers: every
    /// seat held here in it ends, behind what the room already decides.
    pub(super) fn close_room_seats(&self, room: &BareJid) {
        if self.hosting.get().is_none() {
            return;
        }
        for seat in self.seats.of_room(room) {
            self.seats.release(&seat.room, &seat.occupant, seat.session);
            telemetry::seat_ended_room_closed();
            self.local_rooms.push_exempt(
                seat.room.clone(),
                RoomWork::Unseat(Box::new((seat, SeatEnd::RoomClosed))),
            );
        }
    }

    /// End the seats of this replica whose stream another replica lost.
    /// Each occupancy is ended once: a seat is given up here before its
    /// end is queued, and a row that is read again before that end has
    /// removed it names a seat no longer held, for which nothing more is
    /// queued.
    pub(super) async fn sweep_lost_seats(&self) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let Ok(lost) = hosting.store.lost(self.owner.as_str(), SWEEP_BATCH).await else {
            tracing::warn!("reading the seats whose stream another replica lost failed");
            return;
        };
        let settled = now_ms().saturating_sub(duration_ms(self.limits.seat_owner_grace));
        for row in lost {
            match self.seats.release(&row.room, &row.occupant, row.session) {
                Some(seat) => {
                    telemetry::seat_ended_stream_lost();
                    self.local_rooms.push_exempt(
                        seat.room.clone(),
                        RoomWork::Unseat(Box::new((seat, SeatEnd::StreamLost))),
                    );
                }
                // Its end is queued, or the row is one a seat left behind.
                // A row younger than the grace may be that of a join being
                // opened, which holds its seat a moment after its row.
                None => {
                    if row.updated_at_ms <= settled && hosting.store.remove(&row).await.is_err() {
                        tracing::warn!(
                            "removing the row of an ended remote occupant's seat failed"
                        );
                    }
                }
            }
        }
    }

    /// Remove the seats a stopped replica left: its rooms ended with it.
    pub(super) async fn sweep_orphaned_seats(&self, before_ms: i64) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let Ok(orphaned) = hosting.store.orphaned(before_ms, SWEEP_BATCH).await else {
            tracing::warn!("reading the seats stopped replicas left failed");
            return;
        };
        for seat in orphaned {
            if hosting.store.remove(&seat).await.is_err() {
                tracing::warn!("removing a seat a stopped replica left failed");
            }
        }
    }

    /// Start a pass that checks the seats held here against their rooms
    /// and this replica's rows against the seats it holds, on a task of its
    /// own, unless one is still running: the sweep that calls this records
    /// that the replica is alive and must not wait for a room.
    pub(super) fn start_reconcile(self: &Arc<Self>) {
        if self.reconciling.swap(true, Ordering::AcqRel) {
            return;
        }
        let router = Arc::clone(self);
        self.tasks.spawn(async move {
            tokio::select! {
                biased;
                () = router.shutdown.cancelled() => {}
                () = router.reconcile_seats() => {}
            }
            router.reconciling.store(false, Ordering::Release);
        });
    }

    /// One pass: at most [`RECONCILE_ROOMS`] rooms, each asked once for
    /// every occupant it seats, and one page of this replica's rows, within
    /// the time between two passes. The next pass goes on where this one
    /// stopped.
    async fn reconcile_seats(self: &Arc<Self>) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let started = Instant::now();
        let budget = self.limits.seat_reconcile_interval;
        let mut rooms = self.seats.rooms();
        rooms.sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
        let after = lock(&self.reconcile_after).take();
        let from = after.map_or(0, |after| {
            rooms.partition_point(|room| room.as_str() <= after.as_str())
        });
        let mut reached = None;
        for room in rooms.iter().skip(from).take(RECONCILE_ROOMS) {
            if started.elapsed() >= budget {
                break;
            }
            self.reconcile_room(hosting, room).await;
            reached = Some(room.clone());
        }
        // A pass that reached the last room starts over at the first.
        if reached.as_ref() != rooms.last() {
            *lock(&self.reconcile_after) = reached;
        }
        self.reconcile_rows(hosting).await;
    }

    /// Give up every seat held here in `room` whose occupant the room no
    /// longer seats under the seat's generation: the room removed it and
    /// its own presence did not pass through this router, or the room's
    /// actor ended. The occupant is told, as far as a place is free, so it
    /// is not left believing it is seated.
    async fn reconcile_room(self: &Arc<Self>, hosting: &Hosting, room: &BareJid) {
        // Read before the room is asked: a seat settled after the room
        // answered is not in its answer.
        let seats = self.seats.settled_of_room(room);
        if seats.is_empty() {
            return;
        }
        let asked = hosting.host.occupancies(room);
        let Ok(Some(seated)) = tokio::time::timeout(self.limits.room_delivery_timeout, asked).await
        else {
            return;
        };
        for seat in seats {
            let still = seated
                .iter()
                .any(|(occupant, session)| *occupant == seat.occupant && *session == seat.session);
            if !still
                && self
                    .seats
                    .holds_under(&seat.room, &seat.occupant, seat.session)
            {
                telemetry::seat_ended_unseated();
                self.tell_removed(&seat, Status::ServiceErrorKick);
                self.close_seat(hosting, &seat).await;
            }
        }
    }

    /// Remove the rows of this replica that name no seat it holds: those
    /// whose removal failed when their seat ended. One page a pass, in the
    /// order of the table's key, so every row is reached in turn however
    /// many seats are held.
    async fn reconcile_rows(&self, hosting: &Hosting) {
        let after = lock(&self.rows_after).take();
        let page = hosting
            .store
            .owned(
                self.owner.as_str(),
                after.as_ref().map(|(room, occupant)| (room, occupant)),
                SWEEP_BATCH,
            )
            .await;
        let Ok(page) = page else {
            tracing::warn!("reading this replica's remote occupants' seats failed");
            return;
        };
        // The row of a join being opened is younger than the grace: its
        // seat is held a moment after the row is written.
        let settled = now_ms().saturating_sub(duration_ms(self.limits.seat_owner_grace));
        for row in &page {
            if row.updated_at_ms > settled
                || self
                    .seats
                    .holds_under(&row.room, &row.occupant, row.session)
            {
                continue;
            }
            match hosting.store.remove(row).await {
                Ok(_) => telemetry::seat_row_reclaimed(),
                Err(_) => tracing::warn!("removing a row no seat is held for failed"),
            }
        }
        if page.len() >= SWEEP_BATCH {
            *lock(&self.rows_after) = page
                .last()
                .map(|row| (row.room.clone(), row.occupant.clone()));
        }
    }

    /// Take over the seat a stopped replica left for `occupant` in `room`,
    /// whose claim moved here, and hold it: the room can then seat the
    /// occupant as recorded and remove it in its own order
    /// ([`Self::end_adopted_seats`]).
    pub(super) async fn adopt_seat(&self, room: &BareJid, occupant: &FullJid) -> bool {
        if self.seats.holds(room, occupant) {
            return true;
        }
        let Some(hosting) = self.hosting.get() else {
            return false;
        };
        let now = now_ms();
        let before = now.saturating_sub(duration_ms(self.limits.seat_owner_grace));
        match hosting
            .store
            .adopt(room, occupant, self.owner.as_str(), before, now)
            .await
        {
            Ok(Some(seat)) => {
                self.seats.hold(seat);
                true
            }
            Ok(None) => false,
            Err(_) => {
                tracing::warn!("taking over a remote occupant's seat failed");
                false
            }
        }
    }

    /// End the seats of `occupants` in `room` that this replica took over
    /// from a replica that stopped, once the room has seated its recorded
    /// occupants again. What the stopped replica had queued for them is
    /// lost and cannot be told from what they received, so none of those
    /// occupancies continues: each occupant is told that the service
    /// removed it (statuses 110 and 333) and enters again, and those who
    /// stay see it leave with status 333 (XEP-0045 §11.1).
    pub(super) fn end_adopted_seats(&self, room: &BareJid, occupants: &[FullJid]) {
        if self.hosting.get().is_none() {
            return;
        }
        for occupant in occupants {
            let Some(seat) = self.seats.get(room, occupant) else {
                continue;
            };
            if self
                .seats
                .release(&seat.room, &seat.occupant, seat.session)
                .is_none()
            {
                continue;
            }
            telemetry::seat_ended_holder_stopped();
            self.local_rooms.push_exempt(
                room.clone(),
                RoomWork::Unseat(Box::new((seat, SeatEnd::HolderStopped))),
            );
        }
    }

    /// This replica is stopping, and its rooms with it: tell every remote
    /// occupant it seats that the service is shutting down (XEP-0045
    /// §11.2, status 332, with status 110) and give its seat up, then wait
    /// for the pairs' workers to take those stanzas, at most the notice
    /// wait. No seat is opened from here on. Nothing is promised: a
    /// stanza that no stream takes in time is lost with the replica.
    pub(super) async fn farewell(self: &Arc<Self>) {
        if self.stopping.swap(true, Ordering::AcqRel) || self.hosting.get().is_none() {
            return;
        }
        let seats = self.seats.all();
        if seats.is_empty() {
            return;
        }
        for seat in &seats {
            if self
                .seats
                .release(&seat.room, &seat.occupant, seat.session)
                .is_some()
            {
                self.tell_removed(seat, Status::ServiceShutdown);
            }
        }
        let deadline = tokio::time::Instant::now() + self.limits.shutdown_notice_wait;
        while tokio::time::Instant::now() < deadline {
            let waiting = lock(&self.queues)
                .values()
                .any(|queue| queue.seats.capacity() < queue.seats.max_capacity());
            if !waiting {
                break;
            }
            tokio::time::sleep(NOTICE_POLL).await;
        }
    }
}

/// The text of the error for a room whose holder did not answer.
const HOLDER_SILENT: &str =
    "The room is held by another server node that did not answer; try again later.";
