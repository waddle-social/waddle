//! The local rooms remote occupants use: what the router hands to the
//! room host and what it expects back (ADR-0020 D10; XEP-0045).
//!
//! The router decides transport questions: which stream carried a stanza,
//! whether its sender holds a seat, the bounds on seats and on requests,
//! and what leaves for a remote JID. Everything about the room itself is
//! the host's: whether a principal may enter, read, speak or administer,
//! decided for that principal on the evidence of its stream at each
//! request ([`RoomRequest::principal`]). Reaching the host admits nothing.

use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;

use super::principal::RemotePrincipal;
use super::reach::RoomOperation;
use super::seats::Seat;

/// One stanza a remote principal addressed to the MUC service, a room or
/// an occupant JID.
#[derive(Debug, Clone)]
pub struct RoomRequest {
    /// The sender, as the inbound stream that carried the stanza proved
    /// it. Every decision the host takes for this request names the
    /// evidence of that stream.
    pub principal: RemotePrincipal,
    /// What the stanza is, as [`super::reach`] classified it.
    pub operation: RoomOperation,
    pub stanza: Stanza,
    /// The seat the sender holds in the addressed room on this replica, or
    /// the one opened for this request when the stanza is a join.
    pub seat: Option<Seat>,
    /// Whether `seat` was opened for this request. The host seats the
    /// occupant under the seat's occupancy generation, and says in its
    /// answer whether it did ([`SeatChange`]).
    pub joining: bool,
}

/// What a request did to the seat opened for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatChange {
    /// The request was not a join, or changed nothing about the seat.
    Unchanged,
    /// The room seated the occupant: the seat stays.
    Seated,
    /// The room refused the join: the seat opened for it is dropped. For
    /// a request of an occupant that held its seat before, the room no
    /// longer admits the occupant: the seat ends and the room removes it.
    Refused,
    /// The request is presence that enters the room although its sender
    /// holds a seat: the room no longer seats the occupant under the
    /// seat's generation, or the occupant asks to enter again. Nothing was
    /// decided. The router opens a seat that replaces the one the sender
    /// held and asks again with [`RoomRequest::joining`] set, so the
    /// room's answer precedes its traffic for the new seat (XEP-0045
    /// §7.1).
    Rejoin,
}

/// The host's answer to one request.
#[derive(Debug, Clone)]
pub struct RoomReplies {
    /// Stanzas for the sender, in the order it must receive them, each
    /// from the MUC service, the room or one of its occupant JIDs.
    pub stanzas: Vec<Stanza>,
    pub seat: SeatChange,
}

impl RoomReplies {
    /// An answer that leaves the sender's seat as it is.
    pub fn of(stanzas: Vec<Stanza>) -> Self {
        Self {
            stanzas,
            seat: SeatChange::Unchanged,
        }
    }

    /// No answer: the stanza is ignored.
    pub fn none() -> Self {
        Self::of(Vec::new())
    }
}

/// How long a host's [`LocalRooms::place`] takes at most for one request,
/// its lookups and its asks of the replica that holds the room together,
/// before it answers. It is below the router's own bound on placing one
/// request ([`super::RouterLimits::room_placement_timeout`], 5 s by
/// default), so a holder that does not answer is reported by the host and
/// not only by the router's timer, and a slow lookup is not taken for a
/// silent holder.
pub const HOLDER_ASK_LIMIT: Duration = Duration::from_millis(4_500);

/// Where a request for a room is decided.
#[derive(Debug)]
pub enum RoomPlacement {
    /// On this replica: it holds the room, or no replica does.
    Here,
    /// On the replica that holds the room, which took the request with its
    /// sender's stream evidence. That replica seats the occupant and
    /// answers through its own router.
    Carried,
    /// Nowhere now: the replica that holds the room did not answer within
    /// [`HOLDER_ASK_LIMIT`] or could not be reached, so nothing says that
    /// it took the request. The sender is answered with
    /// `resource-constraint`, and so is every request for the room for a
    /// while, without the holder being asked again.
    Silent,
    /// Nowhere now: the room's holder answered and did not take the
    /// request, or no holder could be named. The sender is answered with
    /// this error.
    Refused(Box<xmpp_parsers::stanza_error::StanzaError>),
}

/// A stanza a remote principal addressed to a room this replica does not
/// hold, with what the inbound stream that carried it proved of its
/// sender, as the replica that verified the stream hands it to the room's
/// holder.
#[derive(Debug, Clone)]
pub struct CarriedRoomRequest {
    pub principal: super::principal::CarriedPrincipal,
    pub stanza: Stanza,
}

/// What the room's holder did with a carried request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarriedRoomOutcome {
    /// Queued for the room host, behind the room's earlier requests.
    Queued,
    /// The queues for the room host are full; nothing was queued.
    Busy,
    /// This replica hosts no rooms for remote occupants, or the stanza is
    /// none a remote principal may address to a room; nothing was queued.
    Refused,
}

/// Why a seat ended without the room deciding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatEnd {
    /// A stream of the seat's domain pair failed: stanzas either way may
    /// be lost.
    StreamLost,
    /// A stanza of the room could not be handed to the pair's stream: the
    /// queue for the remote domain was full, the stream could not be
    /// established or did not take it.
    Undeliverable,
    /// The occupant's server answered the room's traffic with a
    /// delivery-related error (XEP-0045 §17.1.2, "ghost users").
    OccupantError,
    /// A stanza of the occupant arrived on a stream that proved something
    /// other than the seat's stream did: the seat no longer stands for the
    /// evidence it was admitted under.
    EvidenceChanged,
    /// The room stopped being open to accounts at other servers: its
    /// `federated` setting was turned off.
    RoomClosed,
    /// The host refused a request of the occupant in a way that ends its
    /// occupancy: its domain is no longer admitted, its room is not open
    /// to it any more, or its leave could not be answered.
    Refused,
    /// The replica that held the room stopped without ending the seat, and
    /// this replica took the seat over with the room. What that replica
    /// had queued for the occupant is lost, so the occupancy does not
    /// continue: the occupant enters again.
    HolderStopped,
}

/// The rooms of this deployment, as the S2S router reaches them.
///
/// Calls are made off the stream's task. [`LocalRooms::handle`] is called
/// one request at a time for each room, in the order the room's requests
/// arrived; [`LocalRooms::place`] is called before it, one request of a
/// room at a time, off the workers that call the others.
/// [`LocalRooms::unseat`] and [`LocalRooms::remove_unseated`] are called
/// off those workers too, so that no room's requests wait for a removal,
/// and may run beside a later request of the same room: that is safe
/// because every removal names the occupancy generation it removes, and a
/// later join seats the occupant under another.
#[async_trait]
pub trait LocalRooms: Send + Sync {
    /// Decide one request and answer it. A stanza that asks for nothing a
    /// remote principal may do is answered with its error or ignored, as
    /// its kind requires; no error answers an error.
    async fn handle(&self, request: RoomRequest) -> RoomReplies;

    /// Where `stanza`, which `principal` addressed to a room, is decided.
    /// A deployment of several replicas keeps a room's actor on one of
    /// them: a request that reaches another is carried there with the
    /// facts of its sender's stream, before any seat is opened for it, so
    /// the replica a stream lands on never seats an occupant of a room it
    /// does not hold. A host of a single replica decides everything here.
    /// Asked only of a host that [`Self::carries`], within a bound of the
    /// router's own: an answer that comes later is not waited for. A host
    /// answers within [`HOLDER_ASK_LIMIT`] in all, every lookup of where
    /// the room is held and every ask of its holder included: with
    /// [`RoomPlacement::Silent`] when that time ran out while it waited
    /// for the holder, or the holder could not be reached, and with
    /// [`RoomPlacement::Refused`] when it ran out while it looked the room
    /// up, which says nothing of the holder.
    async fn place(&self, _principal: &RemotePrincipal, _stanza: &Stanza) -> RoomPlacement {
        RoomPlacement::Here
    }

    /// The seat ended without the room deciding it. The room removes the
    /// occupant, if it still seats it under the seat's generation, and
    /// tells those who stay with status 333 (XEP-0045 §11.1); nothing is
    /// sent to the removed occupant. `true` when the room seated the
    /// occupant under that generation and this call removed it.
    async fn unseat(&self, seat: &Seat, end: SeatEnd) -> bool;

    /// The room seats `occupant`, an occupant of another server, under
    /// `session`, and this replica holds no seat for that occupancy:
    /// nothing reaches it. The room removes it, if it still seats it under
    /// `session`, and tells those who stay with status 333 (XEP-0045
    /// §11.1). By default nothing happens.
    async fn remove_unseated(
        &self,
        _room: &BareJid,
        _occupant: &FullJid,
        _session: OccupancySessionGeneration,
    ) {
    }

    /// Whether the room on this replica seats the occupant under the
    /// seat's generation; `None` when that cannot be told now.
    async fn seats(&self, seat: &Seat) -> Option<bool>;

    /// Whether [`Self::place`] may find a room held by another replica. A
    /// host that says no decides every request itself, and the router asks
    /// it nothing before queueing a request for it.
    fn carries(&self) -> bool {
        false
    }

    /// Every occupant `room` seats on this replica, with the occupancy
    /// generation it is seated under: one answer for all the seats of a
    /// room. Empty for a room this replica holds no actor for; `None` when
    /// that cannot be told now.
    async fn occupancies(
        &self,
        _room: &BareJid,
    ) -> Option<Vec<(FullJid, OccupancySessionGeneration)>> {
        None
    }
}
