use std::fmt;

use uuid::Uuid;

/// Canonical per-message identity resolved by the alias substrate.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageKey(Uuid);

impl MessageKey {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_storage(value: Uuid) -> Self {
        Self(value)
    }

    pub fn to_storage(&self) -> Uuid {
        self.0
    }
}

impl Default for MessageKey {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MessageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MessageKey(..)")
    }
}

/// Fully opaque per-delivery identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeliveryKey(Uuid);

impl DeliveryKey {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_storage(value: Uuid) -> Self {
        Self(value)
    }

    pub fn to_storage(&self) -> Uuid {
        self.0
    }
}

impl Default for DeliveryKey {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for DeliveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeliveryKey(..)")
    }
}
