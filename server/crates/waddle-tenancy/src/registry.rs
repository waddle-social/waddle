//! `TenantRegistry`: sole public constructor of fenced tenant-scoped handles.
//!
//! `TenantHandleGuard` has strictly private fields and `pub(crate)` constructors
//! so production code can only obtain a tenant handle by going through
//! [`TenantRegistry::acquire_tenant`] with a verified [`TenantQueryScope`].

use std::sync::Arc;

use dashmap::DashMap;
use thiserror::Error;
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::permission::{
    PermissionBackend, PermissionBackendError, TenantAuthzStoreRef,
};
use waddle_broker_core::quotas::{QuotaError, TenantResourceController, TenantResourceQuotas};
use waddle_broker_core::store::{TenantDbHandle, TenantQueryScope, TenantStore, TenantStoreError};
use waddle_broker_core::tenant_cache::{TenantKey, TenantScopedCache};

#[derive(Debug, Error)]
pub enum TenantRegistryError {
    #[error("tenant storage error: {0}")]
    Store(#[from] TenantStoreError),
    #[error("tenant authorization store error: {0}")]
    Permission(#[from] PermissionBackendError),
    #[error("tenant quota configuration error: {0}")]
    Quota(#[from] QuotaError),
}

/// Fenced tenant handle token constructed exclusively by [`TenantRegistry`].
///
/// All fields are private so callers cannot forge a handle outside the registry.
#[derive(Debug, Clone)]
pub struct TenantHandleGuard {
    tenant_id: TenantId,
    scope: TenantQueryScope,
    db_handle: TenantDbHandle,
    authz_ref: Option<TenantAuthzStoreRef>,
    resources: Arc<TenantResourceController>,
}

impl TenantHandleGuard {
    pub(crate) fn new(
        scope: TenantQueryScope,
        db_handle: TenantDbHandle,
        authz_ref: Option<TenantAuthzStoreRef>,
        resources: Arc<TenantResourceController>,
    ) -> Self {
        Self {
            tenant_id: scope.tenant_id.clone(),
            scope,
            db_handle,
            authz_ref,
            resources,
        }
    }

    pub fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    pub fn scope(&self) -> &TenantQueryScope {
        &self.scope
    }

    pub fn db_handle(&self) -> &TenantDbHandle {
        &self.db_handle
    }

    pub fn authz_ref(&self) -> Option<&TenantAuthzStoreRef> {
        self.authz_ref.as_ref()
    }

    pub fn resources(&self) -> &Arc<TenantResourceController> {
        &self.resources
    }

    /// Build a tenant-scoped cache key bound to this handle's validated [`TenantId`].
    pub fn scoped_key<K>(&self, key: K) -> TenantKey<K> {
        TenantKey::new(self.tenant_id.clone(), key)
    }
}

struct TenantRegistryEntry {
    authz_ref: Option<TenantAuthzStoreRef>,
}

/// Sole public constructor of [`TenantHandleGuard`] and owner of per-tenant
/// resource quotas and tenant-scoped caches.
pub struct TenantRegistry {
    store: Arc<dyn TenantStore>,
    permissions: Option<Arc<dyn PermissionBackend>>,
    resources: Arc<TenantResourceController>,
    entries: DashMap<TenantId, Arc<TenantRegistryEntry>>,
    metadata_cache: TenantScopedCache<String, String>,
}

impl TenantRegistry {
    pub fn new(
        store: Arc<dyn TenantStore>,
        permissions: Option<Arc<dyn PermissionBackend>>,
        default_quotas: TenantResourceQuotas,
    ) -> Result<Self, TenantRegistryError> {
        let resources = Arc::new(TenantResourceController::new(default_quotas)?);
        Ok(Self {
            store,
            permissions,
            resources,
            entries: DashMap::new(),
            metadata_cache: TenantScopedCache::new(),
        })
    }

    pub fn store(&self) -> &Arc<dyn TenantStore> {
        &self.store
    }

    pub fn resources(&self) -> &Arc<TenantResourceController> {
        &self.resources
    }

    pub fn metadata_cache(&self) -> &TenantScopedCache<String, String> {
        &self.metadata_cache
    }

    /// Provision isolated storage (and optional authz store) for a new tenant.
    pub async fn provision_tenant(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Option<TenantAuthzStoreRef>, TenantRegistryError> {
        self.store.provision_tenant(tenant_id).await?;
        let authz_ref = match &self.permissions {
            Some(backend) => Some(backend.provision_tenant_store(tenant_id).await?),
            None => None,
        };
        self.entries.insert(
            tenant_id.clone(),
            Arc::new(TenantRegistryEntry {
                authz_ref: authz_ref.clone(),
            }),
        );
        Ok(authz_ref)
    }

    /// Acquire a fenced [`TenantHandleGuard`] for `scope`.
    ///
    /// Verifies provisioning, holder identity, and `placement_generation` via
    /// the backing [`TenantStore::open_tenant`].
    pub async fn acquire_tenant(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<TenantHandleGuard, TenantRegistryError> {
        let db_handle = self.store.open_tenant(scope).await?;
        let authz_ref = self
            .entries
            .get(&scope.tenant_id)
            .and_then(|entry| entry.authz_ref.clone());
        Ok(TenantHandleGuard::new(
            scope.clone(),
            db_handle,
            authz_ref,
            Arc::clone(&self.resources),
        ))
    }

    /// Evict cached tenant state and checkpoint/release the tenant's storage
    /// handle (e.g., during quiesce, idle eviction, or lease handoff).
    pub async fn evict_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantRegistryError> {
        self.metadata_cache.clear_tenant(tenant_id);
        self.store.checkpoint_and_release(tenant_id).await?;
        Ok(())
    }

    /// Permanently purge a tenant's cached state, quotas, storage (including
    /// Turso `.db`/`-wal`/`-shm`), and optional authz store.
    pub async fn purge_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantRegistryError> {
        self.metadata_cache.clear_tenant(tenant_id);
        self.entries.remove(tenant_id);
        self.resources.remove_tenant(tenant_id);
        self.store.delete_tenant(tenant_id).await?;
        if let Some(backend) = &self.permissions {
            backend.delete_tenant_store(tenant_id).await?;
        }
        Ok(())
    }
}
