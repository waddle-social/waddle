/// Durable non-wrapping shadow ingress counter carried by XEP-0198 session
/// state. Zero means no shadow ordinal has been allocated yet for the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ShadowOrdinal(u64);

impl ShadowOrdinal {
    pub const ZERO: Self = Self(0);

    pub fn from_storage(value: u64) -> Self {
        Self(value)
    }

    pub fn to_storage(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}
