use std::fmt;

use uuid::Uuid;

/// Identity of one accepted inbound stream.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct IngressStreamId(Uuid);

impl IngressStreamId {
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

impl Default for IngressStreamId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for IngressStreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IngressStreamId(..)")
    }
}

/// Identity of one Stream Management-managed ingress stream.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmIngressId(Uuid);

impl SmIngressId {
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

impl Default for SmIngressId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SmIngressId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SmIngressId(..)")
    }
}
