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

    /// The seat ended without the room deciding it. The room removes the
    /// occupant, if it still seats it under the seat's generation, and
    /// tells those who stay with status 333 (XEP-0045 §11.1); nothing is
    /// sent to the removed occupant.
    async fn unseat(&self, seat: &Seat, end: SeatEnd);

    /// Whether the room on this replica seats the occupant under the
    /// seat's generation; `None` when that cannot be told now.
    async fn seats(&self, seat: &Seat) -> Option<bool>;
}
