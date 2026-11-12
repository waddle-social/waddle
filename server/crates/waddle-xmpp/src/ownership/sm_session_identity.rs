//! Durable identity of a stream-management session and its owner generation.
//!
//! These values are equality evidence. The repository that stores them
//! validates the claim, binding and principal in the transaction that mutates
//! the session.

use uuid::Uuid;

/// Internal session key, distinct from the client-visible SM resume secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmSessionKey(Uuid);

impl SmSessionKey {
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

/// Checked durable SM generation, distinct from an install generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionGeneration(u64);

impl SessionGeneration {
    pub const fn new(value: u64) -> Option<Self> {
        if value > 0 && value <= i64::MAX as u64 {
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
