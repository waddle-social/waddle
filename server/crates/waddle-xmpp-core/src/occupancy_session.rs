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
//! It is internal state: never serialized onto an XMPP stanza.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `Ord` is byte order of the UUID: it exists only so inventories keyed by
/// generation can order entries deterministically; it carries no meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OccupancySessionGeneration(Uuid);

impl OccupancySessionGeneration {
    /// Mint a globally unique generation for a new connection.
    pub fn mint() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

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

#[cfg(test)]
mod tests {
    use super::OccupancySessionGeneration;

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
