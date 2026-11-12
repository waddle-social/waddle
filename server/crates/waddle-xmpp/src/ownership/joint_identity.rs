//! Shared identity evidence for the P3 resource and SM models.
//!
//! None of these values grants authority. A repository validates the exact
//! claim, lease and principal in the transaction that mutates data.

use waddle_xmpp_core::OccupancySessionGeneration;

use super::endpoint_transition::LogicalResourceGeneration;
pub use super::sm_session_identity::{SessionGeneration, SmSessionKey};
use super::{ClaimEpoch, NodeLeaseIdentity};

/// Identities of one logical resource lifetime, retained across SM resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceLifetime {
    pub logical: LogicalResourceGeneration,
    pub occupancy: OccupancySessionGeneration,
}

impl ResourceLifetime {
    pub fn fresh() -> Self {
        Self {
            logical: LogicalResourceGeneration::fresh(),
            occupancy: OccupancySessionGeneration::mint(),
        }
    }
}

/// Equality evidence for the SM claim, distinct from the account claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmClaimIdentity {
    pub session: SmSessionKey,
    pub owner: NodeLeaseIdentity,
    pub epoch: ClaimEpoch,
}

/// Exact SM identity carried by the account resource snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmBinding {
    pub session: SmSessionKey,
    pub generation: SessionGeneration,
}

/// Non-wrapping durable resource-slot installation generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstallGeneration(u64);

impl InstallGeneration {
    pub const MAX: u64 = i64::MAX as u64;
    pub const INITIAL: Self = Self(0);

    pub const fn new(value: u64) -> Option<Self> {
        if value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).and_then(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::{InstallGeneration, SessionGeneration};

    #[test]
    fn durable_generations_reject_invalid_values_and_exhaustion() {
        assert_eq!(SessionGeneration::new(0), None);
        assert_eq!(SessionGeneration::new(i64::MAX as u64 + 1), None);
        assert!(SessionGeneration::new(i64::MAX as u64)
            .unwrap()
            .checked_next()
            .is_none());
        assert!(InstallGeneration::new(i64::MAX as u64)
            .unwrap()
            .checked_next()
            .is_none());
        assert_eq!(InstallGeneration::new(i64::MAX as u64 + 1), None);
    }
}
