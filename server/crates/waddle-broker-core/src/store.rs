use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::identity::{HolderInstance, TenantId};
use crate::placement::FenceGeneration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StorageBackendKind {
    TursoWal,
    Postgres,
}

/// Fenced execution context required for every tenant-scoped storage operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantQueryScope {
    pub tenant_id: TenantId,
    pub holder: HolderInstance,
    pub placement_generation: FenceGeneration,
}

/// Opaque tenant-scoped handle metadata returned by `TenantStore::open_tenant`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantDbHandle {
    pub tenant_id: TenantId,
    pub backend: StorageBackendKind,
    pub placement_generation: FenceGeneration,
}

#[derive(Debug, Error)]
pub enum TenantStoreError {
    #[error("tenant not provisioned: {0}")]
    TenantNotFound(TenantId),
    #[error("tenant already provisioned: {0}")]
    TenantAlreadyExists(TenantId),
    #[error(
        "stale placement generation for tenant {tenant_id}: expected {expected}, got {actual}"
    )]
    StalePlacementGeneration {
        tenant_id: TenantId,
        expected: FenceGeneration,
        actual: FenceGeneration,
    },
    #[error("unauthorized holder for tenant {0}")]
    UnauthorizedHolder(TenantId),
    #[error("file descriptor pool capacity exhausted (limit {limit})")]
    FdPoolExhausted { limit: usize },
    #[error("storage backend error: {0}")]
    Backend(String),
}

#[async_trait]
pub trait TenantStore: Send + Sync {
    fn backend_kind(&self) -> StorageBackendKind;

    /// Provision isolated storage for `tenant_id` (e.g., initialize `<tenant>.db` in WAL mode
    /// or register Postgres tenant scope).
    async fn provision_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError>;

    /// Acquire a fenced tenant handle after verifying `(tenant_id, holder, placement_generation)`.
    async fn open_tenant(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<TenantDbHandle, TenantStoreError>;

    /// Verify that `scope` still holds the authoritative write fence on the backing store.
    async fn verify_write_fence(&self, scope: &TenantQueryScope) -> Result<(), TenantStoreError>;

    /// Checkpoint/flush and evict an idle tenant handle (no-op on Postgres, WAL checkpoint + close on Turso).
    async fn checkpoint_and_release(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError>;

    /// Permanently purge a tenant's storage (including Turso `.db`/`-wal`/`-shm` and archived WAL segments).
    async fn delete_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError>;
}
