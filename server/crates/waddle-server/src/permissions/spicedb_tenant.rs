//! Tenant-scoped SpiceDB `PermissionBackend` implementation.
//!
//! Scopes every resource identifier by `tenant_id` (`"{tenant_id}|{object_id}"`),
//! validates raw UTF-8 length against [`MAX_AUTHZ_ID_BYTES`] (765 bytes -> 1023
//! wire bytes <= 1024) BEFORE any RPC, enforces `Consistency::FullyConsistent`,
//! rejects caller-supplied `contextual_tuples`, and isolates `AuthzModule::Inference`
//! writes behind [`AuthzCallerRole::Broker`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::StreamExt;
use prescience::{
    Client, Consistency, ObjectReference, PermissionResult, Relationship, RelationshipFilter,
    RelationshipUpdate, SubjectReference,
};
use waddle_broker::authz::{
    enforce_module_write, enforce_object_module_write, evaluate_permission_in_memory,
    list_objects_in_memory, list_users_in_memory, AuthzCallerRole,
};
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
    PermissionBackendError, TenantAuthzStoreRef,
};

use super::authz_id::{
    decode_tenant_scoped_id, encode_tenant_scoped_id, AuthzIdError, AuthzOpaqueId,
    MAX_AUTHZ_ID_BYTES,
};
use super::authz_schema_version::AUTHZ_SCHEMA_VERSION;

fn map_id_err(err: AuthzIdError) -> PermissionBackendError {
    match err {
        AuthzIdError::TooLong => PermissionBackendError::IdentifierTooLong {
            max: MAX_AUTHZ_ID_BYTES,
        },
        other => PermissionBackendError::Backend(format!("invalid SpiceDB identifier: {other}")),
    }
}

const fn object_type_name(object_type: AuthzObjectType) -> &'static str {
    match object_type {
        AuthzObjectType::Server => "server",
        AuthzObjectType::Space => "space",
        AuthzObjectType::Channel => "channel",
        AuthzObjectType::Message => "message",
        AuthzObjectType::DirectMessage => "direct_message",
        AuthzObjectType::Role => "role",
        AuthzObjectType::TenantCredential => "tenant_credential",
        AuthzObjectType::UserCredential => "user_credential",
        AuthzObjectType::InstalledExtension => "installed_extension",
        AuthzObjectType::RoomInferencePolicy => "room_inference_policy",
    }
}

const fn subject_type_name(subject_type: AuthzSubjectType) -> &'static str {
    match subject_type {
        AuthzSubjectType::Account => "user",
        AuthzSubjectType::Space => "space",
        AuthzSubjectType::Channel => "channel",
        AuthzSubjectType::Role => "role",
        AuthzSubjectType::Extension => "extension",
    }
}

fn parse_subject_type_name(raw: &str) -> Result<AuthzSubjectType, PermissionBackendError> {
    match raw {
        "user" => Ok(AuthzSubjectType::Account),
        "space" => Ok(AuthzSubjectType::Space),
        "channel" => Ok(AuthzSubjectType::Channel),
        "role" => Ok(AuthzSubjectType::Role),
        "extension" => Ok(AuthzSubjectType::Extension),
        other => Err(PermissionBackendError::Backend(format!(
            "unknown SpiceDB subject type: {other}"
        ))),
    }
}

fn encode_scoped_object(
    tenant: &TenantId,
    object: &AuthzObject,
) -> Result<ObjectReference, PermissionBackendError> {
    let encoded = encode_tenant_scoped_id(tenant, &object.id).map_err(map_id_err)?;
    ObjectReference::new(object_type_name(object.object_type), encoded.as_str()).map_err(|err| {
        PermissionBackendError::Backend(format!("invalid SpiceDB object ref: {err}"))
    })
}

fn encode_scoped_subject(
    tenant: &TenantId,
    subject: &AuthzSubject,
) -> Result<SubjectReference, PermissionBackendError> {
    let wire_id = match subject.subject_type {
        AuthzSubjectType::Space | AuthzSubjectType::Channel | AuthzSubjectType::Role => {
            encode_tenant_scoped_id(tenant, &subject.id)
                .map_err(map_id_err)?
                .as_str()
                .to_owned()
        }
        AuthzSubjectType::Account | AuthzSubjectType::Extension => AuthzOpaqueId::new(&subject.id)
            .map_err(map_id_err)?
            .to_subject_wire()
            .as_str()
            .to_owned(),
    };
    let obj =
        ObjectReference::new(subject_type_name(subject.subject_type), wire_id).map_err(|err| {
            PermissionBackendError::Backend(format!("invalid SpiceDB subject obj: {err}"))
        })?;
    SubjectReference::new(obj, subject.relation.clone()).map_err(|err| {
        PermissionBackendError::Backend(format!("invalid SpiceDB subject ref: {err}"))
    })
}

fn decode_scoped_subject(
    tenant: &TenantId,
    subject: &SubjectReference,
) -> Result<AuthzSubject, PermissionBackendError> {
    let obj = subject.object();
    let subject_type = parse_subject_type_name(obj.object_type())?;
    let id = match subject_type {
        AuthzSubjectType::Space | AuthzSubjectType::Channel | AuthzSubjectType::Role => {
            let (decoded_tenant, raw_id) =
                decode_tenant_scoped_id(obj.object_id()).map_err(map_id_err)?;
            if decoded_tenant != tenant.as_str() {
                return Err(PermissionBackendError::Backend(
                    "cross-tenant subject leaked from SpiceDB read".to_owned(),
                ));
            }
            raw_id
        }
        AuthzSubjectType::Account | AuthzSubjectType::Extension => {
            AuthzOpaqueId::from_wire(obj.object_id())
                .map_err(map_id_err)?
                .into_string()
        }
    };
    Ok(AuthzSubject {
        subject_type,
        id,
        relation: subject.optional_relation().map(str::to_owned),
    })
}

#[async_trait]
trait SpiceDbTransport: Send + Sync {
    async fn check(
        &self,
        tenant: &TenantId,
        req: &CheckRequest,
        resource: ObjectReference,
        subject: SubjectReference,
    ) -> Result<CheckDecision, PermissionBackendError>;

    async fn write_relationships(
        &self,
        tenant: &TenantId,
        tuples: &[AuthzTuple],
        relationships: Vec<Relationship>,
    ) -> Result<(), PermissionBackendError>;

    async fn delete_relationships(
        &self,
        tenant: &TenantId,
        tuples: &[AuthzTuple],
        relationships: Vec<Relationship>,
    ) -> Result<(), PermissionBackendError>;

    async fn read_relationships(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        relation: Option<&AuthzRelation>,
        resource: ObjectReference,
    ) -> Result<Vec<AuthzTuple>, PermissionBackendError>;

    async fn list_objects(
        &self,
        tenant: &TenantId,
        subject: &AuthzSubject,
        encoded_subject: SubjectReference,
        relation: &AuthzRelation,
        object_type: AuthzObjectType,
    ) -> Result<Vec<AuthzObject>, PermissionBackendError>;

    async fn list_users(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        encoded_resource: ObjectReference,
        relation: &AuthzRelation,
        subject_type: AuthzSubjectType,
    ) -> Result<Vec<AuthzSubject>, PermissionBackendError>;

    async fn delete_tenant_relationships(
        &self,
        tenant: &TenantId,
    ) -> Result<(), PermissionBackendError>;

    fn rpc_count(&self) -> u64;
}

struct LiveSpiceDbTransport {
    client: Client,
    rpcs: AtomicU64,
}

#[async_trait]
impl SpiceDbTransport for LiveSpiceDbTransport {
    async fn check(
        &self,
        _tenant: &TenantId,
        req: &CheckRequest,
        resource: ObjectReference,
        subject: SubjectReference,
    ) -> Result<CheckDecision, PermissionBackendError> {
        let _ = req.consistency;
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let response = self
            .client
            .check_permission(&resource, req.permission.as_str(), &subject)
            .consistency(Consistency::FullyConsistent)
            .await
            .map_err(|err| {
                PermissionBackendError::Backend(format!("SpiceDB check failed: {err}"))
            })?;
        Ok(match response {
            PermissionResult::Allowed => CheckDecision::Allowed,
            PermissionResult::Denied | PermissionResult::Conditional { .. } => {
                CheckDecision::Denied
            }
        })
    }

    async fn write_relationships(
        &self,
        _tenant: &TenantId,
        _tuples: &[AuthzTuple],
        relationships: Vec<Relationship>,
    ) -> Result<(), PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let updates: Vec<RelationshipUpdate> = relationships
            .into_iter()
            .map(RelationshipUpdate::touch)
            .collect();
        self.client
            .write_relationships(updates)
            .await
            .map_err(|err| {
                PermissionBackendError::Backend(format!("SpiceDB write failed: {err}"))
            })?;
        Ok(())
    }

    async fn delete_relationships(
        &self,
        _tenant: &TenantId,
        _tuples: &[AuthzTuple],
        relationships: Vec<Relationship>,
    ) -> Result<(), PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let updates: Vec<RelationshipUpdate> = relationships
            .into_iter()
            .map(RelationshipUpdate::delete)
            .collect();
        self.client
            .write_relationships(updates)
            .await
            .map_err(|err| {
                PermissionBackendError::Backend(format!("SpiceDB delete failed: {err}"))
            })?;
        Ok(())
    }

    async fn read_relationships(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        relation: Option<&AuthzRelation>,
        resource: ObjectReference,
    ) -> Result<Vec<AuthzTuple>, PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let mut filter =
            RelationshipFilter::new(resource.object_type()).resource_id(resource.object_id());
        if let Some(rel) = relation {
            filter = filter.relation(rel.as_str());
        }
        let mut stream = self
            .client
            .read_relationships(filter)
            .consistency(Consistency::FullyConsistent)
            .send()
            .await
            .map_err(|err| {
                PermissionBackendError::Backend(format!("SpiceDB read failed: {err}"))
            })?;
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            let resp = item.map_err(|err| {
                PermissionBackendError::Backend(format!("SpiceDB read stream error: {err}"))
            })?;
            let rel = resp.relationship;
            let subject = decode_scoped_subject(tenant, &rel.subject)?;
            out.push(AuthzTuple {
                object: object.clone(),
                relation: AuthzRelation::new(rel.relation),
                subject,
            });
        }
        Ok(out)
    }

    async fn list_objects(
        &self,
        tenant: &TenantId,
        _subject: &AuthzSubject,
        encoded_subject: SubjectReference,
        relation: &AuthzRelation,
        object_type: AuthzObjectType,
    ) -> Result<Vec<AuthzObject>, PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let mut stream = self
            .client
            .lookup_resources(
                object_type_name(object_type),
                relation.as_str(),
                &encoded_subject,
            )
            .consistency(Consistency::FullyConsistent)
            .send()
            .await
            .map_err(|err| {
                PermissionBackendError::Backend(format!("SpiceDB lookup_resources failed: {err}"))
            })?;
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            let resp = item.map_err(|err| {
                PermissionBackendError::Backend(format!(
                    "SpiceDB lookup_resources stream error: {err}"
                ))
            })?;
            if matches!(resp.permission, PermissionResult::Allowed) {
                let (decoded_tenant, raw_id) =
                    decode_tenant_scoped_id(&resp.resource_id).map_err(map_id_err)?;
                if decoded_tenant == tenant.as_str() {
                    out.push(AuthzObject {
                        object_type,
                        id: raw_id,
                    });
                }
            }
        }
        Ok(out)
    }

    async fn list_users(
        &self,
        tenant: &TenantId,
        _object: &AuthzObject,
        encoded_resource: ObjectReference,
        relation: &AuthzRelation,
        subject_type: AuthzSubjectType,
    ) -> Result<Vec<AuthzSubject>, PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let mut stream = self
            .client
            .lookup_subjects(
                &encoded_resource,
                relation.as_str(),
                subject_type_name(subject_type),
            )
            .consistency(Consistency::FullyConsistent)
            .send()
            .await
            .map_err(|err| {
                PermissionBackendError::Backend(format!("SpiceDB lookup_subjects failed: {err}"))
            })?;
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            let resp = item.map_err(|err| {
                PermissionBackendError::Backend(format!(
                    "SpiceDB lookup_subjects stream error: {err}"
                ))
            })?;
            if matches!(resp.permission, PermissionResult::Allowed) {
                let id = match subject_type {
                    AuthzSubjectType::Space
                    | AuthzSubjectType::Channel
                    | AuthzSubjectType::Role => {
                        let (decoded_tenant, raw_id) =
                            decode_tenant_scoped_id(&resp.subject_id).map_err(map_id_err)?;
                        if decoded_tenant != tenant.as_str() {
                            continue;
                        }
                        raw_id
                    }
                    AuthzSubjectType::Account | AuthzSubjectType::Extension => {
                        AuthzOpaqueId::from_wire(&resp.subject_id)
                            .map_err(map_id_err)?
                            .into_string()
                    }
                };
                out.push(AuthzSubject {
                    subject_type,
                    id,
                    relation: None,
                });
            }
        }
        Ok(out)
    }

    async fn delete_tenant_relationships(
        &self,
        _tenant: &TenantId,
    ) -> Result<(), PermissionBackendError> {
        Ok(())
    }

    fn rpc_count(&self) -> u64 {
        self.rpcs.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct InMemorySpiceDbState {
    tuples_by_tenant: HashMap<TenantId, HashSet<AuthzTuple>>,
    wire_ids: HashSet<String>,
}

struct InMemorySpiceDbTransport {
    state: RwLock<InMemorySpiceDbState>,
    rpcs: AtomicU64,
}

impl InMemorySpiceDbTransport {
    fn new() -> Self {
        Self {
            state: RwLock::new(InMemorySpiceDbState::default()),
            rpcs: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl SpiceDbTransport for InMemorySpiceDbTransport {
    async fn check(
        &self,
        tenant: &TenantId,
        req: &CheckRequest,
        _resource: ObjectReference,
        _subject: SubjectReference,
    ) -> Result<CheckDecision, PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let guard = self.state.read().map_err(|_| {
            PermissionBackendError::Backend("in-memory SpiceDB lock poisoned".to_owned())
        })?;
        let Some(tuples) = guard.tuples_by_tenant.get(tenant) else {
            return Ok(CheckDecision::Denied);
        };
        Ok(evaluate_permission_in_memory(tuples, req))
    }

    async fn write_relationships(
        &self,
        tenant: &TenantId,
        tuples: &[AuthzTuple],
        relationships: Vec<Relationship>,
    ) -> Result<(), PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.state.write().map_err(|_| {
            PermissionBackendError::Backend("in-memory SpiceDB lock poisoned".to_owned())
        })?;
        for rel in &relationships {
            guard.wire_ids.insert(rel.resource.object_id().to_owned());
        }
        let set = guard.tuples_by_tenant.entry(tenant.clone()).or_default();
        for tuple in tuples {
            set.insert(tuple.clone());
        }
        Ok(())
    }

    async fn delete_relationships(
        &self,
        tenant: &TenantId,
        tuples: &[AuthzTuple],
        _relationships: Vec<Relationship>,
    ) -> Result<(), PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.state.write().map_err(|_| {
            PermissionBackendError::Backend("in-memory SpiceDB lock poisoned".to_owned())
        })?;
        if let Some(set) = guard.tuples_by_tenant.get_mut(tenant) {
            for tuple in tuples {
                set.remove(tuple);
            }
        }
        Ok(())
    }

    async fn read_relationships(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        relation: Option<&AuthzRelation>,
        _resource: ObjectReference,
    ) -> Result<Vec<AuthzTuple>, PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let guard = self.state.read().map_err(|_| {
            PermissionBackendError::Backend("in-memory SpiceDB lock poisoned".to_owned())
        })?;
        let Some(set) = guard.tuples_by_tenant.get(tenant) else {
            return Ok(Vec::new());
        };
        Ok(set
            .iter()
            .filter(|t| {
                t.object == *object
                    && relation.is_none_or(|rel| t.relation.as_str() == rel.as_str())
            })
            .cloned()
            .collect())
    }

    async fn list_objects(
        &self,
        tenant: &TenantId,
        subject: &AuthzSubject,
        _encoded_subject: SubjectReference,
        relation: &AuthzRelation,
        object_type: AuthzObjectType,
    ) -> Result<Vec<AuthzObject>, PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let guard = self.state.read().map_err(|_| {
            PermissionBackendError::Backend("in-memory SpiceDB lock poisoned".to_owned())
        })?;
        let Some(set) = guard.tuples_by_tenant.get(tenant) else {
            return Ok(Vec::new());
        };
        Ok(list_objects_in_memory(
            set,
            tenant,
            subject,
            relation,
            object_type,
        ))
    }

    async fn list_users(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        _encoded_resource: ObjectReference,
        relation: &AuthzRelation,
        subject_type: AuthzSubjectType,
    ) -> Result<Vec<AuthzSubject>, PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let guard = self.state.read().map_err(|_| {
            PermissionBackendError::Backend("in-memory SpiceDB lock poisoned".to_owned())
        })?;
        let Some(set) = guard.tuples_by_tenant.get(tenant) else {
            return Ok(Vec::new());
        };
        Ok(list_users_in_memory(
            set,
            tenant,
            object,
            relation,
            subject_type,
        ))
    }

    async fn delete_tenant_relationships(
        &self,
        tenant: &TenantId,
    ) -> Result<(), PermissionBackendError> {
        self.rpcs.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.state.write().map_err(|_| {
            PermissionBackendError::Backend("in-memory SpiceDB lock poisoned".to_owned())
        })?;
        guard.tuples_by_tenant.remove(tenant);
        Ok(())
    }

    fn rpc_count(&self) -> u64 {
        self.rpcs.load(Ordering::Relaxed)
    }
}

/// Tenant-scoped SpiceDB `PermissionBackend`.
#[derive(Clone)]
pub struct TenantSpiceDbPermissionBackend {
    caller_role: AuthzCallerRole,
    transport: Arc<dyn SpiceDbTransport>,
    stores: Arc<RwLock<HashMap<TenantId, TenantAuthzStoreRef>>>,
}

impl TenantSpiceDbPermissionBackend {
    pub fn live(client: Client, caller_role: AuthzCallerRole) -> Self {
        Self {
            caller_role,
            transport: Arc::new(LiveSpiceDbTransport {
                client,
                rpcs: AtomicU64::new(0),
            }),
            stores: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn in_memory(caller_role: AuthzCallerRole) -> Self {
        Self {
            caller_role,
            transport: Arc::new(InMemorySpiceDbTransport::new()),
            stores: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn with_caller_role(&self, caller_role: AuthzCallerRole) -> Self {
        Self {
            caller_role,
            transport: Arc::clone(&self.transport),
            stores: Arc::clone(&self.stores),
        }
    }

    pub fn rpc_count(&self) -> u64 {
        self.transport.rpc_count()
    }

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

    fn ensure_provisioned(&self, tenant: &TenantId) -> Result<(), PermissionBackendError> {
        let guard = self.stores.read().map_err(|_| {
            PermissionBackendError::Backend("SpiceDB store registry poisoned".to_owned())
        })?;
        if guard.contains_key(tenant) {
            Ok(())
        } else {
            Err(PermissionBackendError::StoreNotProvisioned(tenant.clone()))
        }
    }

    fn encode_tuples(
        tenant: &TenantId,
        tuples: &[AuthzTuple],
    ) -> Result<Vec<Relationship>, PermissionBackendError> {
        tuples
            .iter()
            .map(|tuple| {
                let resource = encode_scoped_object(tenant, &tuple.object)?;
                let subject = encode_scoped_subject(tenant, &tuple.subject)?;
                Ok(Relationship::new(
                    resource,
                    tuple.relation.as_str(),
                    subject,
                ))
            })
            .collect()
    }
}

#[async_trait]
impl PermissionBackend for TenantSpiceDbPermissionBackend {
    async fn check(&self, req: &CheckRequest) -> Result<CheckDecision, PermissionBackendError> {
        let AuthzConsistency::Strong = req.consistency;
        self.ensure_provisioned(&req.tenant_id)?;
        let resource = encode_scoped_object(&req.tenant_id, &req.object)?;
        let subject = encode_scoped_subject(&req.tenant_id, &req.subject)?;
        self.transport
            .check(&req.tenant_id, req, resource, subject)
            .await
    }

    async fn write_tuples(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError> {
        enforce_module_write(self.caller_role, module, tuples)?;
        self.ensure_provisioned(tenant)?;
        if tuples.is_empty() {
            return Ok(());
        }
        let relationships = Self::encode_tuples(tenant, tuples)?;
        self.transport
            .write_relationships(tenant, tuples, relationships)
            .await
    }

    async fn delete_tuples(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError> {
        enforce_module_write(self.caller_role, module, tuples)?;
        self.ensure_provisioned(tenant)?;
        if tuples.is_empty() {
            return Ok(());
        }
        let relationships = Self::encode_tuples(tenant, tuples)?;
        self.transport
            .delete_relationships(tenant, tuples, relationships)
            .await
    }

    async fn read_tuples(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        relation: Option<&AuthzRelation>,
    ) -> Result<Vec<AuthzTuple>, PermissionBackendError> {
        self.ensure_provisioned(tenant)?;
        let resource = encode_scoped_object(tenant, object)?;
        self.transport
            .read_relationships(tenant, object, relation, resource)
            .await
    }

    async fn list_objects(
        &self,
        tenant: &TenantId,
        subject: &AuthzSubject,
        relation: &AuthzRelation,
        object_type: AuthzObjectType,
    ) -> Result<Vec<AuthzObject>, PermissionBackendError> {
        self.ensure_provisioned(tenant)?;
        let encoded_subject = encode_scoped_subject(tenant, subject)?;
        self.transport
            .list_objects(tenant, subject, encoded_subject, relation, object_type)
            .await
    }

    async fn list_users(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        relation: &AuthzRelation,
        subject_type: AuthzSubjectType,
    ) -> Result<Vec<AuthzSubject>, PermissionBackendError> {
        self.ensure_provisioned(tenant)?;
        let encoded_resource = encode_scoped_object(tenant, object)?;
        self.transport
            .list_users(tenant, object, encoded_resource, relation, subject_type)
            .await
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
        self.ensure_provisioned(tenant)?;
        let existing = self.read_tuples(tenant, object, Some(relation)).await?;
        let new_tuple = AuthzTuple {
            object: object.clone(),
            relation: relation.clone(),
            subject: new_subject.clone(),
        };
        let to_delete: Vec<AuthzTuple> = existing.into_iter().filter(|t| t != &new_tuple).collect();
        if !to_delete.is_empty() {
            self.delete_tuples(tenant, module, &to_delete).await?;
        }
        self.write_tuples(tenant, module, &[new_tuple]).await
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
        self.ensure_provisioned(tenant)?;
        let existing = self.read_tuples(tenant, object, Some(relation)).await?;
        if existing.len() != 1 || existing[0].subject != *expected_subject {
            return Err(PermissionBackendError::SwapPreconditionFailed);
        }
        let old_tuple = existing[0].clone();
        let new_tuple = AuthzTuple {
            object: object.clone(),
            relation: relation.clone(),
            subject: new_subject.clone(),
        };
        if old_tuple == new_tuple {
            return Ok(());
        }
        self.delete_tuples(tenant, module, &[old_tuple]).await?;
        self.write_tuples(tenant, module, &[new_tuple]).await
    }

    async fn provision_tenant_store(
        &self,
        tenant: &TenantId,
    ) -> Result<TenantAuthzStoreRef, PermissionBackendError> {
        let mut guard = self.stores.write().map_err(|_| {
            PermissionBackendError::Backend("SpiceDB store registry poisoned".to_owned())
        })?;
        if let Some(existing) = guard.get(tenant) {
            return Ok(existing.clone());
        }
        let store_ref = TenantAuthzStoreRef {
            tenant_id: tenant.clone(),
            store_id: format!("spicedb-tenant:{}", tenant.as_str()),
            model_version: format!("spicedb-schema-v{AUTHZ_SCHEMA_VERSION}"),
        };
        guard.insert(tenant.clone(), store_ref.clone());
        Ok(store_ref)
    }

    async fn delete_tenant_store(&self, tenant: &TenantId) -> Result<(), PermissionBackendError> {
        {
            let mut guard = self.stores.write().map_err(|_| {
                PermissionBackendError::Backend("SpiceDB store registry poisoned".to_owned())
            })?;
            guard.remove(tenant);
        }
        self.transport.delete_tenant_relationships(tenant).await
    }
}
