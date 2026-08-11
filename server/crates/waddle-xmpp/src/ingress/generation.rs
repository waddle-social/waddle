/// Changes only on entity authority transfer or reacquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityGeneration(u64);

impl EntityGeneration {
    pub const INITIAL: Self = Self(0);

    pub fn from_storage(value: u64) -> Self {
        Self(value)
    }

    pub fn to_storage(&self) -> u64 {
        self.0
    }

    pub fn next(&self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Changes only on full-JID connection install, replacement, or successful socket resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionGeneration(u64);

impl ConnectionGeneration {
    pub const INITIAL: Self = Self(0);

    pub fn from_storage(value: u64) -> Self {
        Self(value)
    }

    pub fn to_storage(&self) -> u64 {
        self.0
    }

    pub fn next(&self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Bumps for ordinary optimistic-concurrency changes to a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowRevision(u64);

impl RowRevision {
    pub const INITIAL: Self = Self(0);

    pub fn from_storage(value: u64) -> Self {
        Self(value)
    }

    pub fn to_storage(&self) -> u64 {
        self.0
    }

    pub fn next(&self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}
