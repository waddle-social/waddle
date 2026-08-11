/// Forward-only protocol epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProtocolEpoch(u32);

impl ProtocolEpoch {
    /// The current initial epoch.
    pub const ZERO: Self = Self(0);

    pub fn from_storage(value: u32) -> Self {
        Self(value)
    }

    pub fn to_storage(&self) -> u32 {
        self.0
    }

    pub fn next(&self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}
