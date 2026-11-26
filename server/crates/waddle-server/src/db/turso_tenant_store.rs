//! Server-facing Turso per-tenant `<tenant_id>.db` WAL store wrapper.
//!
//! Delegates to [`waddle_tenancy::turso_store::TursoTenantStore`] behind the
//! workspace [`TenantStore`] trait and Verus-verified LRU FD pool bounds.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::placement::PlacementLeaseStore;
use waddle_broker_core::store::{
    StorageBackendKind, TenantDbHandle, TenantQueryScope, TenantStore, TenantStoreError,
};
use waddle_tenancy::turso_store::{PinnedTursoConn, TursoPoolConfig, TursoTenantStore};

/// Bounded Turso WAL-mode [`TenantStore`] exposed by `waddle-server::db`.
#[derive(Clone)]
pub struct ServerTursoTenantStore {
    inner: Arc<TursoTenantStore>,
}

impl ServerTursoTenantStore {
    /// Construct a new bounded Turso per-tenant store under `data_dir`.
    pub fn new(
        data_dir: impl Into<PathBuf>,
        pool_config: TursoPoolConfig,
        lease_store: Option<Arc<dyn PlacementLeaseStore>>,
    ) -> Result<Self, TenantStoreError> {
        let store = TursoTenantStore::new(data_dir, pool_config, lease_store)?;
        Ok(Self {
            inner: Arc::new(store),
        })
    }

    /// Borrow the underlying [`TursoTenantStore`].
    pub fn inner(&self) -> &Arc<TursoTenantStore> {
        &self.inner
    }

    /// Convert into an `Arc<dyn TenantStore>` trait object.
    pub fn into_store_arc(self) -> Arc<dyn TenantStore> {
        self.inner
    }

    /// Acquire a pinned Turso connection from the bounded LRU FD pool.
    pub async fn acquire_pinned_conn(
        &self,
        tenant_id: &TenantId,
    ) -> Result<PinnedTursoConn, TenantStoreError> {
        self.inner.acquire_pinned_conn(tenant_id).await
    }

    /// Return `(open_count, pinned_count, eviction_count)` pool metrics.
    pub async fn pool_stats(&self) -> (usize, usize, usize) {
        self.inner.pool_stats().await
    }
}

#[async_trait]
impl TenantStore for ServerTursoTenantStore {
    fn backend_kind(&self) -> StorageBackendKind {
        self.inner.backend_kind()
    }

    async fn provision_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
        self.inner.provision_tenant(tenant_id).await
    }

    async fn open_tenant(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<TenantDbHandle, TenantStoreError> {
        self.inner.open_tenant(scope).await
    }

    async fn verify_write_fence(&self, scope: &TenantQueryScope) -> Result<(), TenantStoreError> {
        self.inner.verify_write_fence(scope).await
    }

    async fn checkpoint_and_release(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
        self.inner.checkpoint_and_release(tenant_id).await
    }

    async fn delete_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
        self.inner.delete_tenant(tenant_id).await
    }
}
