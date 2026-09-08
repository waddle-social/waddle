use crate::ingress::IngressTypeError;

/// Durable non-wrapping handled-stanza ordinal per SM ingress stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IngressOrdinal(u64);

impl IngressOrdinal {
    /// The first handled stanza's durable ordinal.
    pub const FIRST: Self = Self(1);

    pub fn from_storage(value: u64) -> Result<Self, IngressTypeError> {
        if value == 0 {
            return Err(IngressTypeError::ZeroIngressOrdinal);
        }

        Ok(Self(value))
    }

    pub fn to_storage(&self) -> u64 {
        self.0
    }

    /// Returns the next durable ordinal, or `None` after exhaustion.
    pub fn next(&self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    /// Low 32 bits for XEP-0198's wire `h` value.
    ///
    /// Wire `h` remains a wrapping `u32` and is never an idempotency key. The
    /// first handled stanza has ordinal one, so `Self::FIRST.wire_h() == 1`.
    pub fn wire_h(&self) -> u32 {
        self.0 as u32
    }
}

/// XEP-0198 wrapping wire handled count, distinct from a durable ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WireHandledCount(u32);

impl WireHandledCount {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    pub const fn from_storage(value: u32) -> Self {
        Self(value)
    }
    pub const fn to_storage(self) -> u32 {
        self.0
    }
}
