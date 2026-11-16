//! The local rooms remote occupants use: what the router hands to the
//! room host and what it expects back (ADR-0020 D10; XEP-0045).
//!
//! The router decides transport questions: which stream carried a stanza,
//! whether its sender holds a seat, the bounds on seats and on requests,
//! and what leaves for a remote JID. Everything about the room itself is
//! the host's: whether a principal may enter, read, speak or administer,
//! decided for that principal on the evidence of its stream at each
//! request ([`RoomRequest::principal`]). Reaching the host admits nothing.

use async_trait::async_trait;
use waddle_xmpp::Stanza;

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
    /// The room refused the join: the seat opened for it is dropped.
    Refused,
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

/// Where a request for a room is decided.
#[derive(Debug)]
pub enum RoomPlacement {
    /// On this replica: it holds the room, or no replica does.
    Here,
    /// On the replica that holds the room, which took the request with its
    /// sender's stream evidence. That replica seats the occupant and
    /// answers through its own router.
    Carried,
    /// Nowhere now: the room's holder could not take the request. The
    /// sender is answered with this error.
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
}

/// The rooms of this deployment, as the S2S router reaches them.
///
/// Calls are made off the stream's task, one at a time for each room, in
/// the order the room's requests arrived.
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
    async fn place(&self, _principal: &RemotePrincipal, _stanza: &Stanza) -> RoomPlacement {
        RoomPlacement::Here
    }

    /// The seat ended without the room deciding it. The room removes the
    /// occupant, if it still seats it under the seat's generation, and
    /// tells those who stay with status 333 (XEP-0045 §11.1); nothing is
    /// sent to the removed occupant.
    async fn unseat(&self, seat: &Seat, end: SeatEnd);

    /// Whether the room on this replica seats the occupant under the
    /// seat's generation; `None` when that cannot be told now.
    async fn seats(&self, seat: &Seat) -> Option<bool>;
}
