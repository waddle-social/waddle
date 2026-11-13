//! Identity of one client connection's MUC occupancy (#1703).
//!
//! A MUC occupant session is keyed by full JID, but a full JID can be
//! re-bound by a replacement connection while the previous connection's
//! disconnect cleanup is still in flight. The temporal fences (occupancy
//! order, watermarks) only say which event came first; they cannot say
//! *which connection* an occupancy belongs to once a replacement has
//! joined. This value is minted once per connection, survives XEP-0198
//! resumption, is recorded on the occupant session at join, and is
//! presented by every connection-scoped leave and SFU teardown so a stale
//! cleanup can never evict the replacement's occupancy or media.
//!
//! XMPP clients cannot choose this authority. The server may carry it inside
//! signed SFU token attributes to identify observed media after a restart;
//! provider observations never grant or replace MUC occupancy authority.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `Ord` is byte order of the UUID: it exists only so inventories keyed by
/// generation can order entries deterministically; it carries no meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OccupancySessionGeneration(Uuid);

impl OccupancySessionGeneration {
    /// Mint a globally unique generation for a new connection.
    #[must_use]
    pub fn mint() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wrap an existing UUID as an occupancy-session generation.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Return the UUID identifying this occupancy generation.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl std::fmt::Display for OccupancySessionGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for OccupancySessionGeneration {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

/// Identity of one occupancy of a room by one full JID.
///
/// A connection keeps its [`OccupancySessionGeneration`] for its whole life,
/// so a leave followed by a join on the same connection holds the seat under
/// the same generation both times. The room mints this value when it admits
/// a join that starts a new occupancy, keeps it while that occupancy lasts
/// (through presence updates, role changes, a resumed session refreshing its
/// seat, and a change of the room's owner) and never reuses it. Two seats of
/// one full JID are the same occupancy exactly when their identities are
/// equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OccupancySeatId(Uuid);

impl OccupancySeatId {
    /// Mint a globally unique identity for a new occupancy.
    #[must_use]
    pub fn mint() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wrap an existing UUID as a seat identity.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Return the UUID identifying this occupancy.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl std::fmt::Display for OccupancySeatId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for OccupancySeatId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::{OccupancySeatId, OccupancySessionGeneration};

    #[test]
    fn minted_seat_identities_are_distinct_and_round_trip_through_text() {
        let seat = OccupancySeatId::mint();
        assert_ne!(seat, OccupancySeatId::mint());
        let parsed: OccupancySeatId = seat.to_string().parse().unwrap();
        assert_eq!(parsed, seat);
        assert!("not-a-uuid".parse::<OccupancySeatId>().is_err());
    }

    #[test]
    fn minted_generations_are_distinct() {
        assert_ne!(
            OccupancySessionGeneration::mint(),
            OccupancySessionGeneration::mint()
        );
    }

    #[test]
    fn round_trips_through_text() {
        let generation = OccupancySessionGeneration::mint();
        let parsed: OccupancySessionGeneration = generation.to_string().parse().unwrap();
        assert_eq!(parsed, generation);
        assert!("not-a-uuid".parse::<OccupancySessionGeneration>().is_err());
    }
}
