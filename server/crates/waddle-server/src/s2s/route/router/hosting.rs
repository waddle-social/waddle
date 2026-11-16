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

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use tokio::sync::Notify;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::muc::presence::{NS_MUC, NS_MUC_USER};
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
use crate::destination::{SeatAdoption, ServedService};
use crate::permissions::host_grant::StreamEvidence;
use crate::s2s::conn::{SinkVerdict, SinkWait, VerifiedPeerStream, Waited};
use crate::s2s::route::local_rooms::{
    CarriedRoomOutcome, CarriedRoomRequest, RoomPlacement, RoomRequest, SeatChange, SeatEnd,
};
use crate::s2s::route::placement::{Laned, Offered, Waiting};
use crate::s2s::route::principal::RemotePrincipal;
use crate::s2s::route::reach::{reach, InboundReach, LocalTarget};
use crate::s2s::route::reach::{Reachable, RoomOperation};
use crate::s2s::route::replies;
use crate::s2s::route::room_delivery::{Admitted, Full, LocalRoomRequest, RoomWork};
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

/// Until shutdown: hand the work of one shard of local rooms to the room
/// host, one item at a time, so a room decides its requests in the order
/// they arrived.
///
/// One item takes at most the per-item budget
/// ([`super::RouterLimits::room_request_timeout`]): deciding a request,
/// opening or changing its seat and queueing its answer all fit in it, and
/// a seat's end is handed to the workers that end seats
/// ([`serve_seat_ends`]) instead of being waited for here. So a slow room
/// actor holds back its shard for at most that budget per item, well under
/// the time a stream waits for a place.
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
                router.end_seat(seat, end);
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

/// Until shutdown: end the seats handed to these workers, a few at a time,
/// each within the per-item budget for the room and the seat table.
pub(super) async fn serve_seat_ends(router: Arc<Inner>) {
    loop {
        let next = tokio::select! {
            biased;
            () = router.shutdown.cancelled() => return,
            next = router.seat_ends.next() => next,
        };
        router.finish_end(&next).await;
        router.seat_ends.done(&next);
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

/// What a stanza for the MUC service is to the bounds on what remote
/// senders may address to it: which shares of its domain's rate it counts
/// against, and how it is answered when one of them, or its room's share of
/// the queues, refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An IQ, or a message that carries something: it counts against its
    /// account's share, and a refusal is answered with an error.
    Request,
    /// Available presence that enters a room (XEP-0045 §7.2): it carries
    /// the bare `muc` element, or comes from a sender that holds no seat
    /// in a room this replica decides. It counts against its account's
    /// share of presence and not against its other share. A refusal is
    /// answered with error presence; a join is never dropped.
    Join,
    /// Any other available presence to an occupant JID, a change of
    /// availability or nickname: it counts against both of its account's
    /// shares, and a refusal is dropped.
    PresenceChange,
    /// A chat state or a marker (XEP-0085, XEP-0333): it counts against
    /// its account's share, and a refusal is dropped.
    Ephemeral,
    /// A leave, or an error the occupant's server sends: it ends a seat,
    /// counts against no account's share and not against its room's share
    /// of the queues, so it is never refused for either.
    Ending,
    /// Presence of any other type, which the host ignores: it counts
    /// against no account's share, and a refusal is dropped.
    Ignored,
}

impl Kind {
    /// Whether it counts against its account's share of its domain's rate.
    fn counts_against_account(self) -> bool {
        matches!(self, Self::Request | Self::PresenceChange | Self::Ephemeral)
    }

    /// Whether it counts against its account's share of presence.
    fn counts_against_presence(self) -> bool {
        matches!(self, Self::Join | Self::PresenceChange)
    }

    /// Whether a refusal of it is answered.
    fn answered(self) -> bool {
        matches!(self, Self::Request | Self::Join)
    }

    /// Whether it counts against its room's share of the queues.
    fn in_room(self) -> bool {
        self != Self::Ending
    }
}

/// Whether `presence` carries the bare `muc` element a client sends when it
/// enters a room (XEP-0045 §7.2.1).
fn enters(presence: &Presence) -> bool {
    presence
        .payloads
        .iter()
        .any(|payload| payload.is("x", NS_MUC))
}

/// The kind of `stanza`. `unseated` says whether its sender is known to
/// hold no seat in the room it addresses, which only the replica that
/// decides the room can tell; presence that does not carry the `muc`
/// element enters the room only from such a sender.
fn kind_of(stanza: &Stanza, unseated: impl FnOnce() -> bool) -> Kind {
    match stanza {
        Stanza::Iq(_) => Kind::Request,
        Stanza::Message(message) if message.type_ == MessageType::Error => Kind::Ending,
        Stanza::Message(_) if is_ephemeral(stanza) => Kind::Ephemeral,
        Stanza::Message(_) => Kind::Request,
        Stanza::Presence(presence) => match presence.type_ {
            PresenceType::None if enters(presence) || unseated() => Kind::Join,
            PresenceType::None => Kind::PresenceChange,
            PresenceType::Unavailable | PresenceType::Error => Kind::Ending,
            _ => Kind::Ignored,
        },
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
    /// The account has spent a share the stanza counts against: the
    /// stanza is not taken, and is answered with an error only when
    /// `answered`.
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
    /// room host's queue. A stanza that finds its domain's rate spent, or
    /// the queues or its domain's share of them full, holds its stream
    /// until it fits. A stanza beyond a share of its account, or beyond
    /// its room's share of the queues, is not taken at all and is answered
    /// as its kind is ([`Kind`]): the stream goes on, and the other
    /// accounts and rooms of the domain with it.
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
        let kind = self.kind_here(&stanza, &room, &principal, hosting);
        let account = principal.account().as_bare_jid().clone();
        let mut request = LocalRoomRequest {
            principal,
            operation,
            stanza,
            local: pair.local.clone(),
            remote: pair.remote.clone(),
        };
        let mut pause = match self.take_allowance(&peer, &account, kind) {
            Spend::Taken => None,
            Spend::Wait(wait) => Some(wait),
            Spend::AccountSpent { answered } => {
                self.refuse_spent(&pair, &request.stanza, answered);
                return SinkVerdict::Accepted;
            }
        };
        if pause.is_none() {
            match self.admit(&room, request, &peer, staged, kind) {
                Ok(()) => return SinkVerdict::Accepted,
                Err((refused, Full::Room)) => {
                    self.refuse_room_full(&pair, &refused.stanza, kind);
                    return SinkVerdict::Accepted;
                }
                Err((refused, Full::Shared)) => request = *refused,
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
                pause = match router.take_allowance(&peer, &account, kind) {
                    Spend::Taken => None,
                    Spend::Wait(wait) => Some(wait),
                    Spend::AccountSpent { answered } => {
                        router.refuse_spent(&pair, &request.stanza, answered);
                        return Waited::Taken;
                    }
                };
            }
            if staged {
                let waiting = Waiting {
                    request,
                    peer: peer.clone(),
                    arrived: tokio::time::Instant::now(),
                    in_room: kind.in_room(),
                };
                let laned = router
                    .placements
                    .offer_or_wait(&room, waiting, deadline, &router.shutdown)
                    .await;
                match laned {
                    Laned::Queued { started } => {
                        if started {
                            router
                                .tasks
                                .spawn(place_room(Arc::clone(&router), room.clone()));
                        }
                        Waited::Taken
                    }
                    Laned::RoomFull(refused) => {
                        router.refuse_room_full(&pair, &refused.request.stanza, kind);
                        Waited::Taken
                    }
                    Laned::TimedOut => {
                        telemetry::room_request_overloaded();
                        Waited::Overloaded
                    }
                    Laned::Stopped => Waited::Stopping,
                }
            } else {
                let work = RoomWork::Request(Box::new(request));
                let admitted = router
                    .local_rooms
                    .offer_or_wait(
                        room,
                        work,
                        &peer,
                        kind.in_room(),
                        deadline,
                        &router.shutdown,
                    )
                    .await;
                match admitted {
                    Admitted::Queued => Waited::Taken,
                    Admitted::RoomFull(work) => {
                        if let RoomWork::Request(refused) = work {
                            router.refuse_room_full(&pair, &refused.stanza, kind);
                        }
                        Waited::Taken
                    }
                    Admitted::TimedOut => {
                        telemetry::room_request_overloaded();
                        Waited::Overloaded
                    }
                    Admitted::Stopped => Waited::Stopping,
                }
            }
        }))
    }

    /// The kind of `stanza` from `principal` for `room`, as this replica
    /// can tell it: whether presence without the `muc` element enters the
    /// room is known only where this replica decides every room itself.
    fn kind_here(
        &self,
        stanza: &Stanza,
        room: &BareJid,
        principal: &RemotePrincipal,
        hosting: &Hosting,
    ) -> Kind {
        kind_of(stanza, || {
            !hosting.host.carries()
                && principal
                    .full()
                    .is_some_and(|occupant| !self.seats.holds(room, &occupant))
        })
    }

    /// Queue `request` without waiting: for placement when the host keeps
    /// rooms on other replicas (`staged`), or else for the room host. The
    /// request is handed back with the bound that refused it.
    fn admit(
        self: &Arc<Self>,
        room: &BareJid,
        request: LocalRoomRequest,
        peer: &str,
        staged: bool,
        kind: Kind,
    ) -> Result<(), (Box<LocalRoomRequest>, Full)> {
        if !staged {
            let work = RoomWork::Request(Box::new(request));
            return match self
                .local_rooms
                .offer(room.clone(), work, peer, kind.in_room())
            {
                Ok(()) => Ok(()),
                Err((RoomWork::Request(refused), full)) => Err((refused, full)),
                // Only the work offered is handed back.
                Err(_) => Ok(()),
            };
        }
        let waiting = Waiting {
            request,
            peer: peer.to_owned(),
            arrived: tokio::time::Instant::now(),
            in_room: kind.in_room(),
        };
        match self.placements.offer(room, waiting) {
            Offered::Queued => Ok(()),
            Offered::Started => {
                self.tasks.spawn(place_room(Arc::clone(self), room.clone()));
                Ok(())
            }
            Offered::Refused(refused, full) => Err((Box::new(refused.request), full)),
        }
    }

    /// Find where one request for `room` is decided and hand it there: to
    /// this replica's room host, or to the replica that holds the room,
    /// which then took it.
    ///
    /// Waiting for a place among the rooms being placed and asking the
    /// host are bounded apart, each by the placement timeout. Only the ask
    /// says anything of the room's holder: a request that finds no place
    /// in time is answered with `resource-constraint` alone, and its room
    /// is not taken for one whose holder is silent. A holder the host
    /// reports as silent ([`RoomPlacement::Silent`]), or that the ask
    /// outlasts, is: the request and everything that waits for the room
    /// then are answered with `resource-constraint`, and so is every
    /// request for the room until the backoff has passed.
    async fn place_request(self: &Arc<Self>, room: &BareJid, waiting: Waiting) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let Waiting {
            request,
            peer,
            arrived,
            ..
        } = waiting;
        if self.placements.silent(room) {
            telemetry::room_request_not_carried();
            self.refuse_unplaced(&request, self.busy(HOLDER_SILENT));
            return;
        }
        let place = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return,
            place = tokio::time::timeout(
                self.limits.room_placement_timeout,
                self.placements.place(&peer),
            ) => place,
        };
        let place = match place {
            Ok(Some(place)) => place,
            // The stage is closed: the replica is stopping.
            Ok(None) => return,
            Err(_) => {
                telemetry::room_placement_crowded();
                self.refuse_unplaced(&request, self.busy(PLACES_TAKEN));
                return;
            }
        };
        // Another request may have found the holder silent meanwhile.
        if self.placements.silent(room) {
            telemetry::room_request_not_carried();
            self.refuse_unplaced(&request, self.busy(HOLDER_SILENT));
            return;
        }
        let placing = hosting.host.place(&request.principal, &request.stanza);
        let placed = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return,
            placed = tokio::time::timeout(self.limits.room_placement_timeout, placing) => placed,
        };
        // The places are held for the ask only, not while the request
        // waits for the room host's queue.
        drop(place);
        match placed {
            Ok(RoomPlacement::Here) => {
                let deadline = arrived + self.limits.room_request_wait;
                self.queue_placed(room, request, &peer, deadline).await;
            }
            Ok(RoomPlacement::Carried) => telemetry::room_request_carried(),
            Ok(RoomPlacement::Refused(error)) => {
                telemetry::room_request_not_carried();
                self.refuse_unplaced(&request, *error);
            }
            Ok(RoomPlacement::Silent) => {
                telemetry::room_placement_silent();
                self.holder_silent(room, &request);
            }
            Err(_) => {
                telemetry::room_placement_timed_out();
                self.holder_silent(room, &request);
            }
        }
    }

    /// Queue a request placed on this replica for the room host, waiting
    /// for a place while the queues or the domain's share are full, until
    /// `deadline`. The stream that carried the stanza is not waiting for
    /// it any more, so a stanza that finds no place in time is answered
    /// as its kind is rather than its stream closed; one whose room's
    /// share is full is answered so at once.
    async fn queue_placed(
        self: &Arc<Self>,
        room: &BareJid,
        request: LocalRoomRequest,
        peer: &str,
        deadline: tokio::time::Instant,
    ) {
        // This replica decides the room, so whether its sender holds a
        // seat here says whether presence enters it.
        let kind = kind_of(&request.stanza, || {
            request
                .principal
                .full()
                .is_some_and(|occupant| !self.seats.holds(room, &occupant))
        });
        let pair = Pair {
            local: request.local.clone(),
            remote: request.remote.clone(),
        };
        let stanza = request.stanza.clone();
        let work = RoomWork::Request(Box::new(request));
        let admitted = self
            .local_rooms
            .offer_or_wait(
                room.clone(),
                work,
                peer,
                kind.in_room(),
                deadline,
                &self.shutdown,
            )
            .await;
        match admitted {
            Admitted::Queued | Admitted::Stopped => {}
            Admitted::RoomFull(_) => self.refuse_room_full(&pair, &stanza, kind),
            Admitted::TimedOut => {
                telemetry::room_request_overloaded();
                self.refuse_kind(
                    &pair,
                    &stanza,
                    kind,
                    self.busy("The room is busy; try again later."),
                );
            }
        }
    }

    /// `room`'s holder did not answer `request`: answer it and everything
    /// that waits for the room now, which asked the same holder, and answer
    /// the room's requests without asking it until the backoff has passed.
    fn holder_silent(self: &Arc<Self>, room: &BareJid, request: &LocalRoomRequest) {
        self.placements.mark_silent(
            room,
            tokio::time::Instant::now() + self.limits.room_placement_backoff,
        );
        self.refuse_unplaced(request, self.busy(HOLDER_SILENT));
        for behind in self.placements.drain(room) {
            telemetry::room_request_not_carried();
            self.refuse_unplaced(&behind.request, self.busy(HOLDER_SILENT));
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
    /// a share of its domain's request rate, when it is one that is
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

    /// Answer a stanza that was not taken because what already waits for
    /// its room fills the room's share of the queues, as its kind is.
    fn refuse_room_full(self: &Arc<Self>, pair: &Pair, stanza: &Stanza, kind: Kind) {
        telemetry::room_request_room_full();
        self.refuse_kind(
            pair,
            stanza,
            kind,
            self.busy("The room is busy; try again later."),
        );
    }

    /// Answer a stanza that was not taken with `error` when its kind is
    /// answered, if a place for the answer is free; drop it otherwise.
    fn refuse_kind(self: &Arc<Self>, pair: &Pair, stanza: &Stanza, kind: Kind, error: StanzaError) {
        if !kind.answered() {
            return;
        }
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
        // This replica decides the room, so whether its sender holds a
        // seat here says whether presence enters it.
        let kind = kind_of(&stanza, || {
            principal
                .full()
                .is_some_and(|occupant| !self.seats.holds(&room, &occupant))
        });
        let work = RoomWork::Request(Box::new(LocalRoomRequest {
            principal,
            operation,
            stanza,
            local,
            remote,
        }));
        match self.local_rooms.offer(room, work, &peer, kind.in_room()) {
            Ok(()) => CarriedRoomOutcome::Queued,
            Err(_) => CarriedRoomOutcome::Busy,
        }
    }

    /// Take one stanza of `peer`'s allowance for the MUC service and of
    /// the shares of `account` that a stanza of `kind` counts against; or
    /// say how long until the domain's rate allows the next, or that the
    /// account has spent a share.
    ///
    /// A join is never dropped: when its account's share of presence is
    /// spent it is refused with an answer while the domain's allowance of
    /// answered refusals lasts, and otherwise waits, holding its stream,
    /// until the share allows it.
    fn take_allowance(&self, peer: &str, account: &BareJid, kind: Kind) -> Spend {
        let rate = f64::from(self.limits.room_request_rate.max(1));
        let burst = f64::from(self.limits.room_request_burst.max(1));
        let account_rate = f64::from(self.limits.room_request_rate_per_account.max(1)).min(rate);
        let account_burst = f64::from(self.limits.room_request_burst_per_account.max(1)).min(burst);
        let presence_rate = f64::from(self.limits.room_presence_rate_per_account.max(1)).min(rate);
        let presence_burst =
            f64::from(self.limits.room_presence_burst_per_account.max(1)).min(burst);
        let now = Instant::now();
        let mut allowances = lock(&self.allowances);
        let mut accounts = lock(&self.account_allowances);
        let mut presences = lock(&self.presence_allowances);
        let peer = peer.to_owned();
        // A domain table full of spending domains cannot be: an admitted
        // domain has an entry only while it spends, and far fewer are
        // admitted than the table holds.
        let Some(of_peer) = bucket(&mut allowances, &peer, rate, burst, now, ALLOWANCES) else {
            return Spend::Wait(Duration::from_secs_f64(1.0 / rate));
        };
        let of_account = if kind.counts_against_account() {
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
                    let answered = kind.answered() && of_peer.refusals >= 1.0;
                    if answered {
                        of_peer.refusals -= 1.0;
                    }
                    return Spend::AccountSpent { answered };
                }
            }
        } else {
            None
        };
        let of_presence = if kind.counts_against_presence() {
            let share = bucket(
                &mut presences,
                account,
                presence_rate,
                presence_burst,
                now,
                ACCOUNT_ALLOWANCES,
            );
            match share {
                Some(share) if share.tokens >= 1.0 => Some(share),
                spent => {
                    if kind != Kind::Join {
                        return Spend::AccountSpent { answered: false };
                    }
                    if of_peer.refusals >= 1.0 {
                        of_peer.refusals -= 1.0;
                        return Spend::AccountSpent { answered: true };
                    }
                    let missing = spent.map_or(1.0, |share| 1.0 - share.tokens);
                    return Spend::Wait(Duration::from_secs_f64(
                        (missing / presence_rate).max(0.001),
                    ));
                }
            }
        } else {
            None
        };
        if of_peer.tokens < 1.0 {
            return Spend::Wait(Duration::from_secs_f64((1.0 - of_peer.tokens) / rate));
        }
        of_peer.tokens -= 1.0;
        if let Some(share) = of_account {
            share.tokens -= 1.0;
        }
        if let Some(share) = of_presence {
            share.tokens -= 1.0;
        }
        Spend::Taken
    }

    /// Have the room host decide one stanza and queue its answer for the
    /// sender, within the per-item budget: opening a seat, every ask of
    /// the host and queueing the answer share one deadline.
    async fn decide_room_request(self: &Arc<Self>, request: LocalRoomRequest) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let deadline = tokio::time::Instant::now() + self.limits.room_request_timeout;
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
                self.end_seat(held.clone(), SeatEnd::OccupantError);
                return;
            }
            // A seat stands for what the stream that carried its join
            // proved. A stanza a stream of other strength or trust profile
            // carries is not the seat's: the seat ends, and the stanza is
            // decided as one from an entity without a seat, so presence
            // enters again on the evidence of the stream that carries it
            // now.
            if held.evidence != StreamEvidence::of(&principal) {
                self.end_seat(held.clone(), SeatEnd::EvidenceChanged);
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
                    let opened = tokio::time::timeout_at(
                        deadline,
                        self.open_seat(hosting, &principal, &pair, &room, occupant, nick),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        // The seat table did not answer in time. A row it
                        // may still have written names no seat held here
                        // and is removed when this replica's rows are next
                        // checked.
                        tracing::warn!("opening a remote occupant's seat timed out");
                        Err(Box::new(self.join_failed()))
                    });
                    match opened {
                        Ok(opened) => {
                            seat = Some(opened);
                            joining = true;
                            copies = 0;
                        }
                        Err(error) => {
                            if let Some(reply) = replies::error_reply(&stanza, *error) {
                                self.queue_room_reply(&pair, reply, self.reply_deadline(deadline))
                                    .await;
                            }
                            if let Some(left) = given_up {
                                self.end_seat(left, SeatEnd::Refused);
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
            let Ok(answer) = tokio::time::timeout_at(deadline, handled).await else {
                telemetry::room_request_timed_out();
                // The room may have seated the occupant before the host was
                // abandoned.
                if let Some(opened) = seat.filter(|_| joining) {
                    self.end_seat(opened, SeatEnd::Undeliverable);
                }
                if let Some(left) = given_up {
                    self.end_seat(left, SeatEnd::Refused);
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
        // A replica that began to stop while the room decided this join
        // gives up the seat opened for it without a notice, as it gave up
        // the other seats whose join was being answered: the occupant is
        // not told that it entered a room that ends with the replica.
        if joining && self.stopping.load(Ordering::Acquire) {
            if let Some(opened) = seat.take() {
                self.seats
                    .release(&opened.room, &opened.occupant, opened.session);
            }
            if let Some(reply) = replies::error_reply(
                &stanza,
                self.busy("This server is shutting down; try again later."),
            ) {
                self.offer_room_reply(&pair, reply);
            }
            return;
        }
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
                    self.close_seat(&opened);
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
        let replied_by = self.reply_deadline(deadline);
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
                self.rename_seat(held, &nick);
            }
            if !self.queue_room_reply(&pair, reply, replied_by).await {
                // The sender missed part of its answer: a seated occupant
                // cannot be kept in step and joins again.
                answered = false;
                break;
            }
            if last {
                if let Some(held) = seat.take() {
                    self.close_seat(&held);
                }
            }
        }
        if let Some(left) = refused {
            self.end_seat(left, SeatEnd::Refused);
        }
        if !answered {
            if let Some(held) = seat.take() {
                self.end_seat(held, SeatEnd::Undeliverable);
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

    /// Until when the answer to a request decided by `deadline` may wait
    /// for a place among the pair's stanzas: the reply wait, and never past
    /// the item's budget.
    fn reply_deadline(&self, deadline: tokio::time::Instant) -> tokio::time::Instant {
        deadline.min(tokio::time::Instant::now() + self.limits.room_reply_wait)
    }

    /// The answer to a join whose seat could not be opened now.
    fn join_failed(&self) -> StanzaError {
        replies::error(
            self.home(),
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "The room could not be joined now; try again later.",
        )
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
        // seat would end at the room's first stanza for it. The place is
        // held until the seat is, so that the queue is not retired as idle
        // in between.
        let reachable =
            self.reserve_on(pair.clone(), self.limits.reply_queues, |queue| &queue.seats);
        let Some(_reachable) = reachable else {
            telemetry::seat_refused_unreachable();
            return Err(Box::new(self.busy(
                "This server holds no place for more traffic to your server now; try again later.",
            )));
        };
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
        Err(Box::new(self.join_failed()))
    }

    /// Stop holding `seat` and remove its row off the shard: the room does
    /// not seat the occupant, or has just told it that its occupancy
    /// ended. A row that could not be removed is removed when this
    /// replica's rows are next checked against the seats it holds.
    fn close_seat(self: &Arc<Self>, seat: &Seat) {
        self.seats.release(&seat.room, &seat.occupant, seat.session);
        self.remove_row(seat);
    }

    /// Remove `seat`'s row on a task of its own, within the per-item
    /// budget.
    fn remove_row(self: &Arc<Self>, seat: &Seat) {
        let router = Arc::clone(self);
        let seat = seat.clone();
        self.tasks.spawn(async move {
            let Some(hosting) = router.hosting.get() else {
                return;
            };
            let removal = hosting.store.remove(&seat);
            if !matches!(
                tokio::time::timeout(router.limits.room_request_timeout, removal).await,
                Ok(Ok(_))
            ) {
                tracing::warn!("removing a remote occupant's seat failed");
            }
        });
    }

    /// Record the nickname the room now seats `seat`'s occupant under: at
    /// once in the index, and in the row on a task of its own.
    fn rename_seat(self: &Arc<Self>, seat: &Seat, nick: &str) {
        self.seats.rename(&seat.room, &seat.occupant, nick);
        let router = Arc::clone(self);
        let seat = seat.clone();
        let nick = nick.to_owned();
        self.tasks.spawn(async move {
            let Some(hosting) = router.hosting.get() else {
                return;
            };
            let renamed = hosting.store.rename(&seat, &nick, now_ms());
            if !matches!(
                tokio::time::timeout(router.limits.room_request_timeout, renamed).await,
                Ok(Ok(_))
            ) {
                tracing::debug!("recording a remote occupant's nickname failed");
            }
        });
    }

    /// End `seat` without the room deciding it: the seat is given up now,
    /// the occupant told where it may still be reached, and the room's
    /// removal of the occupant and the row's removal are handed to the
    /// workers that end seats, so no shard waits for them. Those who stay
    /// are told by the room (XEP-0045 §11.1).
    pub(super) fn end_seat(self: &Arc<Self>, seat: Seat, end: SeatEnd) {
        if self.hosting.get().is_none() {
            return;
        }
        self.seats.release(&seat.room, &seat.occupant, seat.session);
        // Where the occupant's server may still be reached and was not told
        // by a failed stream or by its own error, the occupant is told that
        // the service removed it (XEP-0045 §11.1), ahead of anything a
        // later join is answered with. It is not waited for: nothing
        // depends on its arrival. A seat taken over from a replica that
        // stopped is told only once the room has removed that occupancy.
        match end {
            SeatEnd::Undeliverable
            | SeatEnd::EvidenceChanged
            | SeatEnd::RoomClosed
            | SeatEnd::Refused => self.tell_removed(&seat, Status::ServiceErrorKick),
            SeatEnd::HolderStopped | SeatEnd::StreamLost | SeatEnd::OccupantError => {}
        }
        self.seat_ends.push(EndWork::Seat(seat, end));
    }

    /// Run one end handed to the seat-end workers: remove the row and have
    /// the room remove the occupancy, each within the per-item budget. A
    /// room that does not answer in time is checked again at the next pass
    /// over the seats, which removes an occupant of another server that
    /// holds no seat.
    async fn finish_end(self: &Arc<Self>, work: &EndWork) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let budget = self.limits.room_request_timeout;
        match work {
            EndWork::Seat(seat, end) => {
                if !matches!(
                    tokio::time::timeout(budget, hosting.store.remove(seat)).await,
                    Ok(Ok(_))
                ) {
                    tracing::warn!("removing an ended remote occupant's seat failed");
                }
                match tokio::time::timeout(budget, hosting.host.unseat(seat, *end)).await {
                    // The occupant of a seat taken over from a replica that
                    // stopped is told once the room removed the occupancy
                    // recorded for it, unless it has entered again since.
                    Ok(true) if *end == SeatEnd::HolderStopped => {
                        if !self.seats.holds(&seat.room, &seat.occupant) {
                            self.tell_removed(seat, Status::ServiceErrorKick);
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        tracing::warn!("removing an ended remote occupant from its room timed out");
                        self.recheck(&seat.room);
                    }
                }
            }
            EndWork::Ghost {
                room,
                occupant,
                session,
            } => {
                let removal = hosting.host.remove_unseated(room, occupant, *session);
                if tokio::time::timeout(budget, removal).await.is_err() {
                    tracing::warn!("removing an occupant that holds no seat timed out");
                    self.recheck(room);
                }
            }
        }
    }

    /// Have the next pass over the seats ask `room` for its occupants,
    /// whether or not this replica still holds a seat in it.
    fn recheck(&self, room: &BareJid) {
        let mut recheck = lock(&self.recheck);
        if recheck.len() < RECONCILE_ROOMS {
            recheck.insert(room.clone());
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
        // A full JID at a domain this deployment serves never holds a seat:
        // a local sender's own copy of its message, and any other frame
        // for a resource of this deployment, is told so without the seat
        // index.
        if self.served.serves(occupant.domain()) {
            return None;
        }
        match stanza {
            Stanza::Presence(_) => {}
            Stanza::Message(message) if message.type_ == MessageType::Groupchat => {}
            Stanza::Message(_) | Stanza::Iq(_) => return None,
        }
        let room = stanza_from(stanza)?.to_bare();
        // Nor does any JID in a room without a seat held here.
        if !self.seats.seats_room(&room) {
            return None;
        }
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
            self.rename_seat(seat, &nick);
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
        let budget = self
            .limits
            .room_delivery_timeout
            .min(self.limits.room_request_timeout);
        match tokio::time::timeout(budget, asked).await {
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
                self.close_seat(&seat);
            }
            Ok(Some(true)) => telemetry::seat_departure_stale(),
            // Whether the room seats the occupant cannot be told: the seat
            // ends and the room removes the occupant if it still seats it,
            // so neither is left without the other.
            Ok(None) | Err(_) => self.end_seat(seat, SeatEnd::Undeliverable),
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
    /// entity's domain, waiting for a place until `deadline`: `false` when
    /// it found none and was dropped.
    async fn queue_room_reply(
        self: &Arc<Self>,
        pair: &Pair,
        stanza: Stanza,
        deadline: tokio::time::Instant,
    ) -> bool {
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
    /// stopped. Rooms where a seat's end could not be confirmed are asked
    /// first, whether or not a seat is still held in them.
    async fn reconcile_seats(self: &Arc<Self>) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let started = Instant::now();
        let budget = self.limits.seat_reconcile_interval;
        let rechecked: Vec<BareJid> = std::mem::take(&mut *lock(&self.recheck))
            .into_iter()
            .collect();
        for room in &rechecked {
            if started.elapsed() >= budget {
                self.recheck(room);
                continue;
            }
            self.reconcile_room(hosting, room).await;
        }
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
    ///
    /// The other way round, an occupant of another server that the room
    /// seats under a generation for which no seat is held here, a pending
    /// seat included, is one nothing can reach: its seat ended and the
    /// room's removal did not complete, or the room was restored with it.
    /// The room removes it, telling those who stay (XEP-0045 §11.1).
    async fn reconcile_room(self: &Arc<Self>, hosting: &Hosting, room: &BareJid) {
        // Read before the room is asked: a seat settled after the room
        // answered is not in its answer.
        let seats = self.seats.settled_of_room(room);
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
                self.close_seat(&seat);
            }
        }
        for (occupant, session) in seated {
            if self.served.serves(occupant.domain())
                || self.seats.holds_under(room, &occupant, session)
                || self.seat_ends.ending(room, &occupant, session)
            {
                continue;
            }
            telemetry::seat_ghost_removed();
            self.seat_ends.push(EndWork::Ghost {
                room: room.clone(),
                occupant,
                session,
            });
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

    /// The seat a room restored on this replica may seat `occupant` on,
    /// recorded under `session`: one this replica already holds under that
    /// generation, which goes on ([`SeatAdoption::Held`]: the room's actor
    /// was restored on the replica that seats the occupant), or the seat a
    /// stopped replica left, taken over and held so that the room can seat
    /// the occupant as recorded and remove it in its own order
    /// ([`SeatAdoption::Adopted`], [`Self::end_adopted_seats`]). A seat of
    /// another generation stands for another occupancy than the one the
    /// room recorded: the occupant is not seated again, and a row taken
    /// over for it is removed.
    pub(super) async fn adopt_seat(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        session: OccupancySessionGeneration,
    ) -> SeatAdoption {
        if let Some(held) = self.seats.get(room, occupant) {
            return if held.session == session {
                SeatAdoption::Held
            } else {
                SeatAdoption::NotHeld
            };
        }
        let Some(hosting) = self.hosting.get() else {
            return SeatAdoption::NotHeld;
        };
        let now = now_ms();
        let before = now.saturating_sub(duration_ms(self.limits.seat_owner_grace));
        let adopted = hosting
            .store
            .adopt(room, occupant, self.owner.as_str(), before, now);
        match tokio::time::timeout(self.limits.room_request_timeout, adopted).await {
            Ok(Ok(Some(seat))) if seat.session == session => {
                self.seats.hold(seat);
                SeatAdoption::Adopted
            }
            Ok(Ok(Some(seat))) => {
                let removal = hosting.store.remove(&seat);
                if !matches!(
                    tokio::time::timeout(self.limits.room_request_timeout, removal).await,
                    Ok(Ok(_))
                ) {
                    tracing::warn!("removing a seat taken over for another occupancy failed");
                }
                SeatAdoption::NotHeld
            }
            Ok(Ok(None)) => SeatAdoption::NotHeld,
            Ok(Err(_)) | Err(_) => {
                tracing::warn!("taking over a remote occupant's seat failed");
                SeatAdoption::NotHeld
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
    /// until the pairs' workers have handed those notices to their streams
    /// or given them up, at most the notice wait. A seat whose join is
    /// still being answered is given up without a notice: its occupant was
    /// never told that it entered, and is not told so afterwards. No seat
    /// is opened from here on. Nothing is promised: a notice that no
    /// stream takes in time is lost with the replica.
    pub(super) async fn farewell(self: &Arc<Self>) {
        if self.stopping.swap(true, Ordering::AcqRel) || self.hosting.get().is_none() {
            return;
        }
        let (settled, _pending) = self.seats.release_all();
        if settled.is_empty() {
            return;
        }
        for seat in &settled {
            if let Some((pair, stanza)) = Self::own_unavailable(seat, Status::ServiceShutdown) {
                self.offer_notice(&pair, stanza);
            }
        }
        let deadline = tokio::time::Instant::now() + self.limits.shutdown_notice_wait;
        self.notices.written_by(deadline).await;
    }

    /// Queue a stopping replica's notice for the occupant's domain without
    /// waiting, if a place is free for it. It holds one of the notices
    /// [`Self::farewell`] waits for until the pair's worker is done with
    /// it.
    fn offer_notice(self: &Arc<Self>, pair: &Pair, stanza: Stanza) {
        let notice = self.notices.notice();
        let placed = self.reserve_on(pair.clone(), self.limits.reply_queues, |queue| &queue.seats);
        if let Some(permit) = placed {
            permit.send(Queued {
                stanza,
                purpose: Purpose::Notice { _held: notice },
            });
        }
    }
}

/// A seat's end, or the removal of an occupant that holds none, for the
/// workers that end seats.
#[derive(Debug, Clone)]
pub(super) enum EndWork {
    Seat(Seat, SeatEnd),
    /// The room seats an occupant of another server under `session`, for
    /// which this replica holds no seat: nothing reaches it.
    Ghost {
        room: BareJid,
        occupant: FullJid,
        session: OccupancySessionGeneration,
    },
}

type EndKey = (BareJid, FullJid, OccupancySessionGeneration);

impl EndWork {
    fn key(&self) -> EndKey {
        match self {
            Self::Seat(seat, _) => (seat.room.clone(), seat.occupant.clone(), seat.session),
            Self::Ghost {
                room,
                occupant,
                session,
            } => (room.clone(), occupant.clone(), *session),
        }
    }
}

#[derive(Debug, Default)]
struct EndQueue {
    items: VecDeque<EndWork>,
    /// The occupancies whose end is queued or running.
    keys: HashSet<EndKey>,
}

/// The ends waiting for the workers that end seats, in the order they were
/// handed over. Each occupancy is ended once: an end of one whose end is
/// queued or running is not queued again. There is at most one end for each
/// seat and for each occupant a room seats, so what waits here is bounded
/// by what this replica seats.
#[derive(Debug, Default)]
pub(super) struct SeatEnds {
    queue: Mutex<EndQueue>,
    work: Notify,
}

impl SeatEnds {
    /// Hand `work` to the workers, unless an end of the same occupancy is
    /// queued or running.
    pub(super) fn push(&self, work: EndWork) {
        let mut queue = lock(&self.queue);
        if queue.keys.insert(work.key()) {
            queue.items.push_back(work);
            drop(queue);
            self.work.notify_one();
        }
    }

    /// The next end, waiting for one.
    pub(super) async fn next(&self) -> EndWork {
        loop {
            let work = self.work.notified();
            tokio::pin!(work);
            work.as_mut().enable();
            if let Some(next) = lock(&self.queue).items.pop_front() {
                return next;
            }
            work.await;
        }
    }

    /// `work` is done: an end of the same occupancy may be queued again.
    pub(super) fn done(&self, work: &EndWork) {
        lock(&self.queue).keys.remove(&work.key());
    }

    /// Whether an end of the occupancy of `occupant` under `session` in
    /// `room` is queued or running.
    pub(super) fn ending(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        session: OccupancySessionGeneration,
    ) -> bool {
        lock(&self.queue)
            .keys
            .contains(&(room.clone(), occupant.clone(), session))
    }
}

/// The notices a stopping replica waits for.
#[derive(Debug, Default)]
pub(super) struct Notices {
    outstanding: AtomicUsize,
    written: Notify,
}

impl Notices {
    fn notice(self: &Arc<Self>) -> Notice {
        self.outstanding.fetch_add(1, Ordering::AcqRel);
        Notice(Arc::clone(self))
    }

    /// Wait until every notice is done with, or until `deadline`.
    async fn written_by(&self, deadline: tokio::time::Instant) {
        loop {
            let written = self.written.notified();
            tokio::pin!(written);
            written.as_mut().enable();
            if self.outstanding.load(Ordering::Acquire) == 0 {
                return;
            }
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => return,
                () = &mut written => {}
            }
        }
    }
}

/// One of a stopping replica's notices, held by its stanza while it is
/// queued and written: dropped once the pair's worker has handed the
/// stanza to its stream, or given it up.
#[derive(Debug)]
pub(super) struct Notice(Arc<Notices>);

impl Drop for Notice {
    fn drop(&mut self) {
        if self.0.outstanding.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.written.notify_waiters();
        }
    }
}

/// The text of the error for a room whose holder did not answer.
const HOLDER_SILENT: &str =
    "The room is held by another server node that did not answer; try again later.";

/// The text of the error for a request that found no place among the rooms
/// being placed.
const PLACES_TAKEN: &str = "Too many rooms are being looked up now; try again later.";
