use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::identity::TenantId;

/// OpenFGA / SpiceDB domain object types (chat + inference modules).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthzObjectType {
    Server,
    Space,
    Channel,
    Message,
    DirectMessage,
    Role,
    TenantCredential,
    UserCredential,
    InstalledExtension,
    RoomInferencePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuthzObject {
    pub object_type: AuthzObjectType,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthzSubjectType {
    Account,
    Space,
    Channel,
    Role,
    Extension,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuthzSubject {
    pub subject_type: AuthzSubjectType,
    pub id: String,
    pub relation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthzRelation(String);

impl AuthzRelation {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuthzTuple {
    pub object: AuthzObject,
    pub relation: AuthzRelation,
    pub subject: AuthzSubject,
}

/// Separation of cell-writable chat tuples from broker-only inference tuples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthzModule {
    Chat,
    Inference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthzConsistency {
    /// OpenFGA `HIGHER_CONSISTENCY` / SpiceDB `FullyConsistent` (mandatory on broker admission path).
    Strong,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRequest {
    pub tenant_id: TenantId,
    pub object: AuthzObject,
    pub permission: AuthzRelation,
    pub subject: AuthzSubject,
    pub consistency: AuthzConsistency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CheckDecision {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantAuthzStoreRef {
    pub tenant_id: TenantId,
    /// OpenFGA `store_id` or SpiceDB tenant namespace prefix.
    pub store_id: String,
    /// Pinned immutable OpenFGA `authorization_model_id` or SpiceDB schema version.
    pub model_version: String,
}

#[derive(Debug, Error)]
pub enum PermissionBackendError {
    #[error("tenant authorization store not provisioned: {0}")]
    StoreNotProvisioned(TenantId),
    #[error("caller is not authorized to write module {0:?}")]
    ModuleWriteForbidden(AuthzModule),
    #[error("contextual tuples are forbidden on broker admission checks")]
    ContextualTuplesForbidden,
    #[error("authorization identifier exceeds backend byte limit ({max} bytes)")]
    IdentifierTooLong { max: usize },
    #[error("exclusive relation swap precondition failed")]
    SwapPreconditionFailed,
    #[error("permission backend error: {0}")]
    Backend(String),
}

/// Backend-agnostic authorization contract implemented for OpenFGA, SpiceDB, and Local test store.
#[async_trait]
pub trait PermissionBackend: Send + Sync {
    async fn check(&self, req: &CheckRequest) -> Result<CheckDecision, PermissionBackendError>;

    async fn write_tuples(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError>;

    async fn delete_tuples(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError>;

    async fn read_tuples(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        relation: Option<&AuthzRelation>,
    ) -> Result<Vec<AuthzTuple>, PermissionBackendError>;

    async fn replace_exclusive_relation(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        object: &AuthzObject,
        relation: &AuthzRelation,
        new_subject: &AuthzSubject,
    ) -> Result<(), PermissionBackendError>;

    async fn swap_exclusive_relation(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        object: &AuthzObject,
        relation: &AuthzRelation,
        expected_subject: &AuthzSubject,
        new_subject: &AuthzSubject,
    ) -> Result<(), PermissionBackendError>;

    async fn provision_tenant_store(
        &self,
        tenant: &TenantId,
    ) -> Result<TenantAuthzStoreRef, PermissionBackendError>;

    async fn delete_tenant_store(&self, tenant: &TenantId) -> Result<(), PermissionBackendError>;
}
