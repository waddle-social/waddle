//! Sole server-side entrypoint for fenced tenant handles.
//!
//! Wraps [`waddle_tenancy::registry::TenantRegistry`] so callers in `waddle-server`
//! obtain a [`TenantHandleGuard`] only by presenting a valid [`TenantQueryScope`]
//! against a configured [`TenantStore`].

use std::sync::Arc;

use waddle_broker_core::identity::TenantId;
use waddle_broker_core::permission::{PermissionBackend, TenantAuthzStoreRef};
use waddle_broker_core::quotas::{TenantResourceController, TenantResourceQuotas};
use waddle_broker_core::store::{TenantQueryScope, TenantStore};
use waddle_tenancy::registry::{TenantHandleGuard, TenantRegistry, TenantRegistryError};

/// Server registry coordinating [`TenantStore`] handles, authz stores, and per-tenant quotas.
#[derive(Clone)]
pub struct ServerTenantRegistry {
    inner: Arc<TenantRegistry>,
}

impl ServerTenantRegistry {
    /// Create a tenant registry backed by `store`, optional `permissions`, and `default_quotas`.
    pub fn new(
        store: Arc<dyn TenantStore>,
        permissions: Option<Arc<dyn PermissionBackend>>,
        default_quotas: TenantResourceQuotas,
    ) -> Result<Self, TenantRegistryError> {
        let inner = TenantRegistry::new(store, permissions, default_quotas)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Borrow the underlying [`TenantRegistry`].
    pub fn inner(&self) -> &Arc<TenantRegistry> {
        &self.inner
    }

    /// Borrow the backing [`TenantStore`].
    pub fn store(&self) -> &Arc<dyn TenantStore> {
        self.inner.store()
    }

    /// Borrow the per-tenant [`TenantResourceController`].
    pub fn resources(&self) -> &Arc<TenantResourceController> {
        self.inner.resources()
    }

    /// Provision isolated storage (and optional authz store) for `tenant_id`.
    pub async fn provision_tenant(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Option<TenantAuthzStoreRef>, TenantRegistryError> {
        self.inner.provision_tenant(tenant_id).await
    }

    /// Idempotently ensure isolated storage (and optional authz store) is
    /// provisioned and registered for `tenant_id` across restarts.
    pub async fn ensure_tenant(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Option<TenantAuthzStoreRef>, TenantRegistryError> {
        self.inner.ensure_tenant(tenant_id).await
    }

    /// Acquire a fenced [`TenantHandleGuard`] for a verified [`TenantQueryScope`].
    pub async fn acquire_tenant(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<TenantHandleGuard, TenantRegistryError> {
        self.inner.acquire_tenant(scope).await
    }

    /// Evict cached tenant state and checkpoint/release its storage handle.
    pub async fn evict_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantRegistryError> {
        self.inner.evict_tenant(tenant_id).await
    }

    /// Permanently purge tenant storage, cached metadata, quotas, and optional authz store.
    pub async fn purge_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantRegistryError> {
        self.inner.purge_tenant(tenant_id).await
    }
}
