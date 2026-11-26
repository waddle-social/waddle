use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::identity::{CellRole, HolderInstance, TenantId};

/// Strictly monotonic fencing generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FenceGeneration(u64);

impl FenceGeneration {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl fmt::Display for FenceGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlacementMode {
    SharedPool,
    DedicatedCell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferState {
    Active,
    Transferring { target: HolderInstance },
    Draining,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementLease {
    pub tenant_id: TenantId,
    pub cell_role: CellRole,
    pub mode: PlacementMode,
    pub holder: HolderInstance,
    pub generation: FenceGeneration,
    /// Authoritative DB-clock expiry in Unix milliseconds (never client clock).
    pub expires_at_ms: i64,
    pub transfer_state: TransferState,
}

#[derive(Debug, Error)]
pub enum LeaseError {
    #[error("lease already held for tenant {tenant_id} at generation {current_generation}")]
    AlreadyHeld {
        tenant_id: TenantId,
        current_generation: FenceGeneration,
    },
    #[error("stale lease generation for tenant {tenant_id}: expected {expected}, actual {actual}")]
    StaleGeneration {
        tenant_id: TenantId,
        expected: FenceGeneration,
        actual: FenceGeneration,
    },
    #[error("holder mismatch for tenant {0}")]
    HolderMismatch(TenantId),
    #[error("lease expired for tenant {0}")]
    LeaseExpired(TenantId),
    #[error("lease not found for tenant {0}")]
    NotFound(TenantId),
    #[error("invalid lease ttl_ms: {0}")]
    InvalidTtl(u64),
    #[error("placement store backend error: {0}")]
    Backend(String),
}

#[async_trait]
pub trait PlacementLeaseStore: Send + Sync {
    async fn claim(
        &self,
        tenant: &TenantId,
        role: &CellRole,
        mode: PlacementMode,
        holder: &HolderInstance,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError>;

    async fn renew(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        generation: FenceGeneration,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError>;

    async fn transfer(
        &self,
        tenant: &TenantId,
        from_holder: &HolderInstance,
        to_holder: &HolderInstance,
        expected_generation: FenceGeneration,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError>;

    async fn release(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> Result<(), LeaseError>;

    async fn current(&self, tenant: &TenantId) -> Result<Option<PlacementLease>, LeaseError>;
}
