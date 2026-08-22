//! Typed deferred projections (#1647).
//!
//! A projection commits a claim-fenced lifecycle revision but carries no
//! authoritative room state: occupancy and pins stay in actor memory, and the
//! committed revision is what authorizes the one-use in-memory projection
//! (`EphemeralProjectionAuthorization`). The vocabulary is closed so the store
//! can fingerprint each kind for acknowledgement-loss reconciliation.

use jid::FullJid;
use waddle_xmpp_core::xep0359::StanzaId;

use super::MucOccupantNick;

/// Closed vocabulary of deferred projections that commit a lifecycle revision
/// without writing authoritative room state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomProjection {
    /// One occupant session joined the room under a nick.
    OccupancyJoin {
        occupant: FullJid,
        nick: MucOccupantNick,
    },
    /// One occupant session left the room.
    OccupancyLeave {
        occupant: FullJid,
        nick: MucOccupantNick,
        cause: OccupancyLeaveCause,
    },
    /// A pin-list change.
    Pin(RoomPinProjection),
}

/// Why an occupancy leave projection was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OccupancyLeaveCause {
    /// The occupant sent unavailable presence to the room.
    Explicit,
    /// The occupant's session ended (disconnect, stream-management expiry,
    /// janitor re-drive).
    Disconnect,
    /// A channel or group-DM administrator removed the occupant.
    Administrative,
}

/// A pin-list projection keyed by the archived target stanza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomPinProjection {
    Pin { target: StanzaId },
    Unpin { target: StanzaId },
}

impl RoomProjection {
    /// Stable kind label for fingerprints, metrics, and logs.
    pub const fn kind(&self) -> RoomProjectionKind {
        match self {
            Self::OccupancyJoin { .. } => RoomProjectionKind::OccupancyJoin,
            Self::OccupancyLeave { .. } => RoomProjectionKind::OccupancyLeave,
            Self::Pin(RoomPinProjection::Pin { .. }) => RoomProjectionKind::Pin,
            Self::Pin(RoomPinProjection::Unpin { .. }) => RoomProjectionKind::Unpin,
        }
    }
}

/// Closed kind vocabulary for a [`RoomProjection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoomProjectionKind {
    OccupancyJoin,
    OccupancyLeave,
    Pin,
    Unpin,
}

impl RoomProjectionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OccupancyJoin => "occupancy_join",
            Self::OccupancyLeave => "occupancy_leave",
            Self::Pin => "pin",
            Self::Unpin => "unpin",
        }
    }
}
