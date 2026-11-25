//! Pure-Rust tenant-isolated local `PermissionBackend` for deterministic tests
//! and single-node local mode.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use uuid::Uuid;
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzRelation, AuthzSubject, AuthzTuple, CheckDecision, CheckRequest,
    PermissionBackend, PermissionBackendError, TenantAuthzStoreRef,
};

use crate::authz::{
    enforce_module_write, enforce_object_module_write, evaluate_permission_in_memory,
    AuthzCallerRole,
};

#[derive(Debug, Clone)]
struct LocalTenantStore {
    store_ref: TenantAuthzStoreRef,
    tuples: HashSet<AuthzTuple>,
}

/// Thread-safe, per-tenant isolated `PermissionBackend` implementing the full
/// chat + inference schema v3 evaluation rules and caller-role module write guards.
#[derive(Debug, Clone)]
pub struct LocalPermissionBackend {
    caller_role: AuthzCallerRole,
    stores: Arc<RwLock<HashMap<TenantId, LocalTenantStore>>>,
}

impl Default for LocalPermissionBackend {
    fn default() -> Self {
        Self::new(AuthzCallerRole::Broker)
    }
}

impl LocalPermissionBackend {
    pub fn new(caller_role: AuthzCallerRole) -> Self {
        Self {
            caller_role,
            stores: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Returns a view over the exact same underlying tenant stores acting under
    /// a different `AuthzCallerRole` (e.g. testing `Cell` vs `Broker` authority).
    pub fn with_caller_role(&self, caller_role: AuthzCallerRole) -> Self {
        Self {
            caller_role,
            stores: Arc::clone(&self.stores),
        }
    }

    /// Guarded check entry point rejecting any non-empty caller-supplied `contextual_tuples`.
    pub async fn check_with_contextual_tuples(
        &self,
        req: &CheckRequest,
        contextual_tuples: &[AuthzTuple],
    ) -> Result<CheckDecision, PermissionBackendError> {
        if !contextual_tuples.is_empty() {
            return Err(PermissionBackendError::ContextualTuplesForbidden);
        }
        self.check(req).await
    }

    pub fn store_ref(&self, tenant: &TenantId) -> Option<TenantAuthzStoreRef> {
        self.stores
            .read()
            .ok()
            .and_then(|map| map.get(tenant).map(|s| s.store_ref.clone()))
    }
}

#[async_trait]
impl PermissionBackend for LocalPermissionBackend {
    async fn check(&self, req: &CheckRequest) -> Result<CheckDecision, PermissionBackendError> {
        let stores = self
            .stores
            .read()
            .map_err(|_| PermissionBackendError::Backend("local authz lock poisoned".to_owned()))?;
        let store = stores
            .get(&req.tenant_id)
            .ok_or_else(|| PermissionBackendError::StoreNotProvisioned(req.tenant_id.clone()))?;
        Ok(evaluate_permission_in_memory(&store.tuples, req))
    }

    async fn write_tuples(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError> {
        enforce_module_write(self.caller_role, module, tuples)?;
        let mut stores = self
            .stores
            .write()
            .map_err(|_| PermissionBackendError::Backend("local authz lock poisoned".to_owned()))?;
        let store = stores
            .get_mut(tenant)
            .ok_or_else(|| PermissionBackendError::StoreNotProvisioned(tenant.clone()))?;
        for tuple in tuples {
            store.tuples.insert(tuple.clone());
        }
        Ok(())
    }

    async fn delete_tuples(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError> {
        enforce_module_write(self.caller_role, module, tuples)?;
        let mut stores = self
            .stores
            .write()
            .map_err(|_| PermissionBackendError::Backend("local authz lock poisoned".to_owned()))?;
        let store = stores
            .get_mut(tenant)
            .ok_or_else(|| PermissionBackendError::StoreNotProvisioned(tenant.clone()))?;
        for tuple in tuples {
            store.tuples.remove(tuple);
        }
        Ok(())
    }

    async fn read_tuples(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        relation: Option<&AuthzRelation>,
    ) -> Result<Vec<AuthzTuple>, PermissionBackendError> {
        let stores = self
            .stores
            .read()
            .map_err(|_| PermissionBackendError::Backend("local authz lock poisoned".to_owned()))?;
        let store = stores
            .get(tenant)
            .ok_or_else(|| PermissionBackendError::StoreNotProvisioned(tenant.clone()))?;
        Ok(store
            .tuples
            .iter()
            .filter(|t| {
                t.object == *object
                    && relation.is_none_or(|rel| t.relation.as_str() == rel.as_str())
            })
            .cloned()
            .collect())
    }

    async fn replace_exclusive_relation(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        object: &AuthzObject,
        relation: &AuthzRelation,
        new_subject: &AuthzSubject,
    ) -> Result<(), PermissionBackendError> {
        enforce_object_module_write(self.caller_role, module, object)?;
        let mut stores = self
            .stores
            .write()
            .map_err(|_| PermissionBackendError::Backend("local authz lock poisoned".to_owned()))?;
        let store = stores
            .get_mut(tenant)
            .ok_or_else(|| PermissionBackendError::StoreNotProvisioned(tenant.clone()))?;
        store
            .tuples
            .retain(|t| !(t.object == *object && t.relation == *relation));
        store.tuples.insert(AuthzTuple {
            object: object.clone(),
            relation: relation.clone(),
            subject: new_subject.clone(),
        });
        Ok(())
    }

    async fn swap_exclusive_relation(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        object: &AuthzObject,
        relation: &AuthzRelation,
        expected_subject: &AuthzSubject,
        new_subject: &AuthzSubject,
    ) -> Result<(), PermissionBackendError> {
        enforce_object_module_write(self.caller_role, module, object)?;
        let mut stores = self
            .stores
            .write()
            .map_err(|_| PermissionBackendError::Backend("local authz lock poisoned".to_owned()))?;
        let store = stores
            .get_mut(tenant)
            .ok_or_else(|| PermissionBackendError::StoreNotProvisioned(tenant.clone()))?;
        let current: Vec<AuthzTuple> = store
            .tuples
            .iter()
            .filter(|t| t.object == *object && t.relation == *relation)
            .cloned()
            .collect();
        if current.len() != 1 || current[0].subject != *expected_subject {
            return Err(PermissionBackendError::SwapPreconditionFailed);
        }
        store.tuples.remove(&current[0]);
        store.tuples.insert(AuthzTuple {
            object: object.clone(),
            relation: relation.clone(),
            subject: new_subject.clone(),
        });
        Ok(())
    }

    async fn provision_tenant_store(
        &self,
        tenant: &TenantId,
    ) -> Result<TenantAuthzStoreRef, PermissionBackendError> {
        let mut stores = self
            .stores
            .write()
            .map_err(|_| PermissionBackendError::Backend("local authz lock poisoned".to_owned()))?;
        if let Some(existing) = stores.get(tenant) {
            return Ok(existing.store_ref.clone());
        }
        let store_ref = TenantAuthzStoreRef {
            tenant_id: tenant.clone(),
            store_id: format!("local_store_{}", Uuid::new_v4().simple()),
            model_version: "local-schema-v3".to_owned(),
        };
        stores.insert(
            tenant.clone(),
            LocalTenantStore {
                store_ref: store_ref.clone(),
                tuples: HashSet::new(),
            },
        );
        Ok(store_ref)
    }

    async fn delete_tenant_store(&self, tenant: &TenantId) -> Result<(), PermissionBackendError> {
        let mut stores = self
            .stores
            .write()
            .map_err(|_| PermissionBackendError::Backend("local authz lock poisoned".to_owned()))?;
        stores.remove(tenant);
        Ok(())
    }
}
