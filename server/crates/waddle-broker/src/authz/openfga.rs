//! OpenFGA `PermissionBackend` implementation with per-tenant stores,
//! pinned immutable `authorization_model_id`s, Modular Models (`schema 1.2`),
//! mandatory `HIGHER_CONSISTENCY`, and rejection of caller-supplied `contextual_tuples`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
    PermissionBackendError, TenantAuthzStoreRef,
};

use crate::authz::openfga_model::{
    decode_openfga_object, decode_openfga_subject, encode_openfga_object, encode_openfga_subject,
    object_type_wire_name, openfga_authorization_model_json, openfga_module_grant_object,
    openfga_store_grant_object, parse_object_type_wire_name, parse_subject_type_wire_name,
    subject_type_wire_name, OPENFGA_SCHEMA_VERSION,
};
use crate::authz::{
    enforce_module_write, enforce_object_module_write, evaluate_permission_in_memory,
    list_objects_in_memory, list_users_in_memory, object_type_module, AuthzCallerRole,
};

/// OpenFGA wire consistency enum. Only `HIGHER_CONSISTENCY` is representable;
/// `MINIMIZE_LATENCY` and `UNSPECIFIED` are prohibited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpenFgaConsistency {
    #[serde(rename = "HIGHER_CONSISTENCY")]
    HigherConsistency,
}

/// Validates a raw OpenFGA consistency string, rejecting `MINIMIZE_LATENCY`,
/// `UNSPECIFIED`, or any value other than `HIGHER_CONSISTENCY`.
pub fn parse_openfga_consistency(raw: &str) -> Result<OpenFgaConsistency, PermissionBackendError> {
    match raw {
        "HIGHER_CONSISTENCY" => Ok(OpenFgaConsistency::HigherConsistency),
        other => Err(PermissionBackendError::Backend(format!(
            "prohibited OpenFGA consistency value: {other} (only HIGHER_CONSISTENCY is allowed)"
        ))),
    }
}

/// Configuration for [`OpenFgaPermissionBackend`].
#[derive(Debug, Clone)]
pub struct OpenFgaConfig {
    pub endpoint: String,
    pub bearer_token: Option<String>,
    pub caller_role: AuthzCallerRole,
    pub pinned_model_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFgaTupleKey {
    pub user: String,
    pub relation: String,
    pub object: String,
}

impl OpenFgaTupleKey {
    pub fn from_tuple(tuple: &AuthzTuple) -> Self {
        Self {
            user: encode_openfga_subject(&tuple.subject),
            relation: tuple.relation.as_str().to_owned(),
            object: encode_openfga_object(&tuple.object),
        }
    }

    pub fn to_tuple(&self) -> Result<AuthzTuple, PermissionBackendError> {
        Ok(AuthzTuple {
            object: decode_openfga_object(&self.object)?,
            relation: AuthzRelation::new(self.relation.clone()),
            subject: decode_openfga_subject(&self.user)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFgaCheckWireRequest {
    pub authorization_model_id: String,
    pub tuple_key: OpenFgaTupleKey,
    pub consistency: OpenFgaConsistency,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaCheckWireResponse {
    allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaCreateStoreResponse {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaWriteModelResponse {
    authorization_model_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaReadTupleEntry {
    key: OpenFgaTupleKey,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaReadWireResponse {
    tuples: Vec<OpenFgaReadTupleEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaStoreListItem {
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaListStoresResponse {
    #[serde(default)]
    stores: Vec<OpenFgaStoreListItem>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaAuthorizationModelItem {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaListModelsResponse {
    #[serde(default)]
    authorization_models: Vec<OpenFgaAuthorizationModelItem>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaListObjectsWireResponse {
    #[serde(default)]
    objects: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenFgaObjectRef {
    #[serde(rename = "type")]
    object_type: String,
    id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenFgaUsersetRef {
    #[serde(rename = "type")]
    object_type: String,
    id: String,
    relation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenFgaUserEntry {
    #[serde(default)]
    object: Option<OpenFgaObjectRef>,
    #[serde(default)]
    userset: Option<OpenFgaUsersetRef>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenFgaListUsersWireResponse {
    #[serde(default)]
    users: Vec<OpenFgaUserEntry>,
}

/// OpenFGA-backed `PermissionBackend` using one `store_id` per tenant,
/// immutable pinned `authorization_model_id`, Modular Models (`schema 1.2`),
/// and mandatory `HIGHER_CONSISTENCY`.
#[derive(Debug, Clone)]
pub struct OpenFgaPermissionBackend {
    endpoint: String,
    bearer_token: Option<String>,
    caller_role: AuthzCallerRole,
    pinned_model_version: Option<String>,
    http: reqwest::Client,
    stores: Arc<RwLock<HashMap<TenantId, TenantAuthzStoreRef>>>,
}

impl OpenFgaPermissionBackend {
    pub fn new(config: OpenFgaConfig) -> Result<Self, PermissionBackendError> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| {
                PermissionBackendError::Backend(format!(
                    "failed to build OpenFGA HTTP client: {err}"
                ))
            })?;
        Ok(Self {
            endpoint: config.endpoint.trim_end_matches('/').to_owned(),
            bearer_token: config.bearer_token,
            caller_role: config.caller_role,
            pinned_model_version: config.pinned_model_version,
            http,
            stores: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Returns a handle sharing the same provisioned store map under a
    /// different [`AuthzCallerRole`].
    pub fn with_caller_role(&self, caller_role: AuthzCallerRole) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            bearer_token: self.bearer_token.clone(),
            caller_role,
            pinned_model_version: self.pinned_model_version.clone(),
            http: self.http.clone(),
            stores: Arc::clone(&self.stores),
        }
    }

    pub fn caller_role(&self) -> AuthzCallerRole {
        self.caller_role
    }

    pub fn store_ref(&self, tenant: &TenantId) -> Option<TenantAuthzStoreRef> {
        self.stores
            .read()
            .ok()
            .and_then(|map| map.get(tenant).cloned())
    }

    pub fn register_store_ref(
        &self,
        store_ref: TenantAuthzStoreRef,
    ) -> Result<(), PermissionBackendError> {
        self.write_stores()?
            .insert(store_ref.tenant_id.clone(), store_ref);
        Ok(())
    }

    /// Returns the OpenFGA Access Control module grant object (`module:<store_id>|chat`
    /// or `module:<store_id>|inference`) for a provisioned tenant.
    pub fn module_grant_object(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
    ) -> Result<String, PermissionBackendError> {
        let store_ref = self.require_store_ref(tenant)?;
        Ok(openfga_module_grant_object(&store_ref.store_id, module))
    }

    /// Returns the OpenFGA Access Control store reader object (`store:<store_id>`).
    pub fn store_grant_object(&self, tenant: &TenantId) -> Result<String, PermissionBackendError> {
        let store_ref = self.require_store_ref(tenant)?;
        Ok(openfga_store_grant_object(&store_ref.store_id))
    }

    /// Guarded check that rejects any non-empty caller-supplied `contextual_tuples`
    /// BEFORE any HTTP RPC is issued.
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

    fn read_stores(
        &self,
    ) -> Result<RwLockReadGuard<'_, HashMap<TenantId, TenantAuthzStoreRef>>, PermissionBackendError>
    {
        self.stores
            .read()
            .map_err(|_| PermissionBackendError::Backend("OpenFGA store map poisoned".to_owned()))
    }

    fn write_stores(
        &self,
    ) -> Result<RwLockWriteGuard<'_, HashMap<TenantId, TenantAuthzStoreRef>>, PermissionBackendError>
    {
        self.stores
            .write()
            .map_err(|_| PermissionBackendError::Backend("OpenFGA store map poisoned".to_owned()))
    }

    fn require_store_ref(
        &self,
        tenant: &TenantId,
    ) -> Result<TenantAuthzStoreRef, PermissionBackendError> {
        self.read_stores()?
            .get(tenant)
            .cloned()
            .ok_or_else(|| PermissionBackendError::StoreNotProvisioned(tenant.clone()))
    }

    fn apply_headers(&self, mut builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let role_header = match self.caller_role {
            AuthzCallerRole::Cell => "cell",
            AuthzCallerRole::Broker => "broker",
        };
        builder = builder.header("X-Waddle-Caller-Role", role_header);
        if let Some(token) = &self.bearer_token {
            builder = builder.bearer_auth(token);
        }
        builder
    }

    async fn send_request<Req>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Req>,
    ) -> Result<reqwest::Response, PermissionBackendError>
    where
        Req: Serialize + ?Sized,
    {
        let url = format!("{}{}", self.endpoint, path);
        let mut req = self.apply_headers(self.http.request(method, &url));
        if let Some(payload) = body {
            req = req.json(payload);
        }
        req.send()
            .await
            .map_err(|err| PermissionBackendError::Backend(format!("OpenFGA HTTP error: {err}")))
    }

    async fn send_json<Req, Resp>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Req>,
    ) -> Result<Resp, PermissionBackendError>
    where
        Req: Serialize + ?Sized,
        Resp: for<'de> Deserialize<'de>,
    {
        let resp = self.send_request(method, path, body).await?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|err| {
            PermissionBackendError::Backend(format!("OpenFGA response read error: {err}"))
        })?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            return Err(PermissionBackendError::Backend(format!(
                "OpenFGA HTTP {status}: {text}"
            )));
        }
        serde_json::from_slice(&bytes).map_err(|err| {
            PermissionBackendError::Backend(format!("OpenFGA JSON decode error: {err}"))
        })
    }

    async fn send_empty<Req>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Req>,
    ) -> Result<(), PermissionBackendError>
    where
        Req: Serialize + ?Sized,
    {
        let resp = self.send_request(method, path, body).await?;
        let status = resp.status();
        if !status.is_success() {
            let bytes = resp.bytes().await.unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            if status == reqwest::StatusCode::FORBIDDEN {
                return Err(PermissionBackendError::ModuleWriteForbidden(
                    AuthzModule::Inference,
                ));
            }
            return Err(PermissionBackendError::Backend(format!(
                "OpenFGA HTTP {status}: {text}"
            )));
        }
        Ok(())
    }

    async fn mutate_tuples(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        tuples: &[AuthzTuple],
        section: &str,
    ) -> Result<(), PermissionBackendError> {
        enforce_module_write(self.caller_role, module, tuples)?;
        if tuples.is_empty() {
            return Ok(());
        }
        let store_ref = self.require_store_ref(tenant)?;
        let keys: Vec<OpenFgaTupleKey> = tuples.iter().map(OpenFgaTupleKey::from_tuple).collect();
        let payload = json!({
            "authorization_model_id": store_ref.model_version,
            section: { "tuple_keys": keys }
        });
        self.send_empty(
            reqwest::Method::POST,
            &format!("/stores/{}/write", store_ref.store_id),
            Some(&payload),
        )
        .await
    }
}

#[async_trait]
impl PermissionBackend for OpenFgaPermissionBackend {
    async fn check(&self, req: &CheckRequest) -> Result<CheckDecision, PermissionBackendError> {
        let store_ref = self.require_store_ref(&req.tenant_id)?;
        let consistency = match req.consistency {
            AuthzConsistency::Strong => OpenFgaConsistency::HigherConsistency,
        };
        let wire_req = OpenFgaCheckWireRequest {
            authorization_model_id: store_ref.model_version.clone(),
            tuple_key: OpenFgaTupleKey {
                user: encode_openfga_subject(&req.subject),
                relation: req.permission.as_str().to_owned(),
                object: encode_openfga_object(&req.object),
            },
            consistency,
        };
        let resp: OpenFgaCheckWireResponse = self
            .send_json(
                reqwest::Method::POST,
                &format!("/stores/{}/check", store_ref.store_id),
                Some(&wire_req),
            )
            .await?;
        Ok(if resp.allowed {
            CheckDecision::Allowed
        } else {
            CheckDecision::Denied
        })
    }

    async fn write_tuples(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError> {
        self.mutate_tuples(tenant, module, tuples, "writes").await
    }

    async fn delete_tuples(
        &self,
        tenant: &TenantId,
        module: AuthzModule,
        tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError> {
        self.mutate_tuples(tenant, module, tuples, "deletes").await
    }

    async fn read_tuples(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        relation: Option<&AuthzRelation>,
    ) -> Result<Vec<AuthzTuple>, PermissionBackendError> {
        let store_ref = self.require_store_ref(tenant)?;
        let mut tuple_key = serde_json::Map::new();
        tuple_key.insert(
            "object".to_owned(),
            serde_json::Value::String(encode_openfga_object(object)),
        );
        if let Some(rel) = relation {
            tuple_key.insert(
                "relation".to_owned(),
                serde_json::Value::String(rel.as_str().to_owned()),
            );
        }
        let payload = json!({
            "tuple_key": tuple_key,
            "consistency": OpenFgaConsistency::HigherConsistency
        });
        let resp: OpenFgaReadWireResponse = self
            .send_json(
                reqwest::Method::POST,
                &format!("/stores/{}/read", store_ref.store_id),
                Some(&payload),
            )
            .await?;
        resp.tuples
            .iter()
            .map(|entry| entry.key.to_tuple())
            .collect()
    }

    async fn list_objects(
        &self,
        tenant: &TenantId,
        subject: &AuthzSubject,
        relation: &AuthzRelation,
        object_type: AuthzObjectType,
    ) -> Result<Vec<AuthzObject>, PermissionBackendError> {
        let store_ref = self.require_store_ref(tenant)?;
        let payload = json!({
            "authorization_model_id": store_ref.model_version,
            "type": object_type_wire_name(object_type),
            "relation": relation.as_str(),
            "user": encode_openfga_subject(subject),
            "consistency": OpenFgaConsistency::HigherConsistency
        });
        let resp: OpenFgaListObjectsWireResponse = self
            .send_json(
                reqwest::Method::POST,
                &format!("/stores/{}/list-objects", store_ref.store_id),
                Some(&payload),
            )
            .await?;
        resp.objects
            .iter()
            .map(|raw| decode_openfga_object(raw))
            .collect()
    }

    async fn list_users(
        &self,
        tenant: &TenantId,
        object: &AuthzObject,
        relation: &AuthzRelation,
        subject_type: AuthzSubjectType,
    ) -> Result<Vec<AuthzSubject>, PermissionBackendError> {
        let store_ref = self.require_store_ref(tenant)?;
        let payload = json!({
            "authorization_model_id": store_ref.model_version,
            "object": {
                "type": object_type_wire_name(object.object_type),
                "id": object.id,
            },
            "relation": relation.as_str(),
            "user_filters": [
                { "type": subject_type_wire_name(subject_type) }
            ],
            "consistency": OpenFgaConsistency::HigherConsistency
        });
        let resp: OpenFgaListUsersWireResponse = self
            .send_json(
                reqwest::Method::POST,
                &format!("/stores/{}/list-users", store_ref.store_id),
                Some(&payload),
            )
            .await?;
        let mut subjects = Vec::with_capacity(resp.users.len());
        for entry in resp.users {
            if let Some(obj) = entry.object {
                subjects.push(AuthzSubject {
                    subject_type: parse_subject_type_wire_name(&obj.object_type)?,
                    id: obj.id,
                    relation: None,
                });
            } else if let Some(us) = entry.userset {
                subjects.push(AuthzSubject {
                    subject_type: parse_subject_type_wire_name(&us.object_type)?,
                    id: us.id,
                    relation: Some(us.relation),
                });
            }
        }
        Ok(subjects)
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
        let store_ref = self.require_store_ref(tenant)?;
        let payload = json!({
            "authorization_model_id": store_ref.model_version,
            "writes": { "tuple_keys": [OpenFgaTupleKey::from_tuple(&new_tuple)] },
            "deletes": { "tuple_keys": [OpenFgaTupleKey::from_tuple(&old_tuple)] }
        });
        self.send_empty(
            reqwest::Method::POST,
            &format!("/stores/{}/write", store_ref.store_id),
            Some(&payload),
        )
        .await
    }

    async fn provision_tenant_store(
        &self,
        tenant: &TenantId,
    ) -> Result<TenantAuthzStoreRef, PermissionBackendError> {
        if let Some(existing) = self.store_ref(tenant) {
            return Ok(existing);
        }
        let store_name = format!("waddle-tenant-{}", tenant.as_str());
        let existing_store_id = match self
            .send_json::<serde_json::Value, OpenFgaListStoresResponse>(
                reqwest::Method::GET,
                "/stores",
                None,
            )
            .await
        {
            Ok(listed) => listed
                .stores
                .into_iter()
                .find(|s| s.name == store_name)
                .map(|s| s.id),
            Err(_) => None,
        };

        let store_id = match existing_store_id {
            Some(id) => id,
            None => {
                let create_payload = json!({ "name": store_name });
                let created: OpenFgaCreateStoreResponse = self
                    .send_json(reqwest::Method::POST, "/stores", Some(&create_payload))
                    .await?;
                created.id
            }
        };

        let model_version = if let Some(pinned) = &self.pinned_model_version {
            pinned.clone()
        } else {
            let existing_model = match self
                .send_json::<serde_json::Value, OpenFgaListModelsResponse>(
                    reqwest::Method::GET,
                    &format!("/stores/{store_id}/authorization-models"),
                    None,
                )
                .await
            {
                Ok(models) => models.authorization_models.into_iter().next().map(|m| m.id),
                Err(_) => None,
            };
            match existing_model {
                Some(id) => id,
                None => {
                    let model_payload = openfga_authorization_model_json();
                    let model_resp: OpenFgaWriteModelResponse = self
                        .send_json(
                            reqwest::Method::POST,
                            &format!("/stores/{store_id}/authorization-models"),
                            Some(&model_payload),
                        )
                        .await?;
                    model_resp.authorization_model_id
                }
            }
        };

        let store_ref = TenantAuthzStoreRef {
            tenant_id: tenant.clone(),
            store_id,
            model_version,
        };
        self.register_store_ref(store_ref.clone())?;
        Ok(store_ref)
    }

    async fn delete_tenant_store(&self, tenant: &TenantId) -> Result<(), PermissionBackendError> {
        if let Some(store_ref) = self.store_ref(tenant) {
            self.send_empty::<serde_json::Value>(
                reqwest::Method::DELETE,
                &format!("/stores/{}", store_ref.store_id),
                None,
            )
            .await?;
            let mut stores = self.write_stores()?;
            if stores
                .get(tenant)
                .is_some_and(|current| current.store_id == store_ref.store_id)
            {
                stores.remove(tenant);
            }
        }
        Ok(())
    }
}

/// Recorded HTTP/JSON request observed by [`InProcessOpenFgaServer`].
#[derive(Debug, Clone)]
pub struct RecordedOpenFgaRequest {
    pub method: String,
    pub path: String,
    pub caller_role: Option<String>,
    pub body: Option<serde_json::Value>,
}

#[derive(Debug, Default)]
struct MockOpenFgaStore {
    name: String,
    model_ids: Vec<String>,
    tuples: HashSet<AuthzTuple>,
}

#[derive(Debug, Default)]
struct MockOpenFgaState {
    stores: HashMap<String, MockOpenFgaStore>,
    requests: Vec<RecordedOpenFgaRequest>,
    fail_next_deletes: usize,
}

type MockHttpResponse = (u16, &'static str, Option<serde_json::Value>);

/// Real HTTP/1.1 in-process OpenFGA server for deterministic integration,
/// wire-contract, and parity tests without containers.
pub struct InProcessOpenFgaServer {
    endpoint: String,
    state: Arc<RwLock<MockOpenFgaState>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl InProcessOpenFgaServer {
    pub async fn start() -> Result<Self, PermissionBackendError> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|err| {
            PermissionBackendError::Backend(format!("failed to bind mock OpenFGA server: {err}"))
        })?;
        let addr = listener.local_addr().map_err(|err| {
            PermissionBackendError::Backend(format!("failed to read local addr: {err}"))
        })?;
        let state = Arc::new(RwLock::new(MockOpenFgaState::default()));
        let state_clone = Arc::clone(&state);
        let (tx, mut rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else {
                            break;
                        };
                        let st = Arc::clone(&state_clone);
                        tokio::spawn(async move {
                            let _ = handle_http_connection(&mut stream, st).await;
                        });
                    }
                }
            }
        });

        Ok(Self {
            endpoint: format!("http://{addr}"),
            state,
            shutdown: Some(tx),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn recorded_requests(&self) -> Vec<RecordedOpenFgaRequest> {
        self.state
            .read()
            .map(|s| s.requests.clone())
            .unwrap_or_default()
    }

    pub fn store_exists(&self, store_id: &str) -> bool {
        self.state
            .read()
            .map(|s| s.stores.contains_key(store_id))
            .unwrap_or(false)
    }

    /// Causes the next `count` `DELETE /stores/{store_id}` requests to fail
    /// with HTTP 500 before modifying server state (for retry tests).
    pub fn fail_next_deletes(&self, count: usize) {
        if let Ok(mut guard) = self.state.write() {
            guard.fail_next_deletes = count;
        }
    }
}

impl Drop for InProcessOpenFgaServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

async fn handle_http_connection(
    stream: &mut tokio::net::TcpStream,
    state: Arc<RwLock<MockOpenFgaState>>,
) -> Result<(), std::io::Error> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > 1024 * 1024 {
            return Ok(());
        }
    };

    let header_str = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = header_str.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    let mut content_length: usize = 0;
    let mut caller_role: Option<String> = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            } else if k.eq_ignore_ascii_case("x-waddle-caller-role") {
                caller_role = Some(v.trim().to_owned());
            }
        }
    }

    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body_bytes = if content_length > 0 && buf.len() >= body_start + content_length {
        &buf[body_start..body_start + content_length]
    } else {
        &[]
    };
    let body_json: Option<serde_json::Value> = if body_bytes.is_empty() {
        None
    } else {
        serde_json::from_slice(body_bytes).ok()
    };

    let (status_code, status_text, resp_body) =
        route_openfga_request(&state, &method, &path, caller_role, body_json);

    let resp_bytes = resp_body.map(|v| v.to_string()).unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status_code} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        resp_bytes.len(),
        resp_bytes
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn json_err(status: u16, reason: &'static str, message: &str) -> MockHttpResponse {
    (status, reason, Some(json!({ "message": message })))
}

fn bad_request(message: &str) -> MockHttpResponse {
    json_err(400, "Bad Request", message)
}

fn handle_create_store(
    guard: &mut MockOpenFgaState,
    body: Option<serde_json::Value>,
) -> MockHttpResponse {
    let store_id = format!("01FGA{}", Uuid::new_v4().simple());
    let name = body
        .as_ref()
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    guard.stores.insert(
        store_id.clone(),
        MockOpenFgaStore {
            name,
            ..MockOpenFgaStore::default()
        },
    );
    (201, "Created", Some(json!({ "id": store_id })))
}

fn handle_list_stores(guard: &MockOpenFgaState) -> MockHttpResponse {
    let stores: Vec<serde_json::Value> = guard
        .stores
        .iter()
        .map(|(id, store)| json!({ "id": id, "name": store.name }))
        .collect();
    (200, "OK", Some(json!({ "stores": stores })))
}

fn handle_list_models(store: &MockOpenFgaStore) -> MockHttpResponse {
    let models: Vec<serde_json::Value> = store
        .model_ids
        .iter()
        .rev()
        .map(|id| json!({ "id": id }))
        .collect();
    (200, "OK", Some(json!({ "authorization_models": models })))
}

fn handle_delete_store(guard: &mut MockOpenFgaState, store_id: &str) -> MockHttpResponse {
    if guard.fail_next_deletes > 0 {
        guard.fail_next_deletes -= 1;
        return json_err(
            500,
            "Internal Server Error",
            "injected transient delete failure",
        );
    }
    guard.stores.remove(store_id);
    (204, "No Content", None)
}

fn handle_write_model(
    store: &mut MockOpenFgaStore,
    body: Option<serde_json::Value>,
) -> MockHttpResponse {
    let schema_ver = body
        .as_ref()
        .and_then(|v| v.get("schema_version"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if schema_ver != OPENFGA_SCHEMA_VERSION {
        return bad_request("schema_version must be 1.2");
    }
    let model_id = format!("01MOD{}", Uuid::new_v4().simple());
    store.model_ids.push(model_id.clone());
    (
        201,
        "Created",
        Some(json!({ "authorization_model_id": model_id })),
    )
}

fn validate_consistency_and_model(
    store: &MockOpenFgaStore,
    val: &serde_json::Value,
) -> Result<(), MockHttpResponse> {
    if val.get("contextual_tuples").is_some() {
        return Err(bad_request("contextual_tuples prohibited"));
    }
    let consistency = val
        .get("consistency")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if consistency != "HIGHER_CONSISTENCY" {
        return Err(bad_request("HIGHER_CONSISTENCY required"));
    }
    let model_id = val
        .get("authorization_model_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !store.model_ids.is_empty() && !store.model_ids.iter().any(|id| id == model_id) {
        return Err(bad_request("unknown authorization_model_id"));
    }
    Ok(())
}

fn handle_check(store: &MockOpenFgaStore, body: Option<serde_json::Value>) -> MockHttpResponse {
    let Some(val) = body else {
        return bad_request("missing body");
    };
    if let Err(resp) = validate_consistency_and_model(store, &val) {
        return resp;
    }
    let Ok(tk) = serde_json::from_value::<OpenFgaTupleKey>(
        val.get("tuple_key")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    ) else {
        return bad_request("invalid tuple_key");
    };
    let Ok(object) = decode_openfga_object(&tk.object) else {
        return bad_request("invalid object");
    };
    let Ok(subject) = decode_openfga_subject(&tk.user) else {
        return bad_request("invalid user");
    };
    let Ok(dummy_tenant) = TenantId::parse("mock.openfga") else {
        return (500, "Internal Server Error", None);
    };
    let req = CheckRequest {
        tenant_id: dummy_tenant,
        object,
        permission: AuthzRelation::new(tk.relation),
        subject,
        consistency: AuthzConsistency::Strong,
    };
    let allowed = evaluate_permission_in_memory(&store.tuples, &req) == CheckDecision::Allowed;
    (200, "OK", Some(json!({ "allowed": allowed })))
}

fn parse_mock_tuple_batch(
    val: &serde_json::Value,
    section: &str,
    role: AuthzCallerRole,
    bad_tuple_msg: &str,
    forbidden_msg: &str,
) -> Result<Vec<AuthzTuple>, MockHttpResponse> {
    let mut parsed = Vec::new();
    let Some(arr) = val
        .get(section)
        .and_then(|s| s.get("tuple_keys"))
        .and_then(|k| k.as_array())
    else {
        return Ok(parsed);
    };
    for item in arr {
        let Ok(tk) = serde_json::from_value::<OpenFgaTupleKey>(item.clone()) else {
            return Err(bad_request(bad_tuple_msg));
        };
        let Ok(tuple) = tk.to_tuple() else {
            return Err(bad_request("bad tuple format"));
        };
        if role == AuthzCallerRole::Cell
            && object_type_module(tuple.object.object_type) == AuthzModule::Inference
        {
            return Err(json_err(403, "Forbidden", forbidden_msg));
        }
        parsed.push(tuple);
    }
    Ok(parsed)
}

fn handle_write(
    store: &mut MockOpenFgaStore,
    caller_role: Option<&str>,
    body: Option<serde_json::Value>,
) -> MockHttpResponse {
    let Some(val) = body else {
        return bad_request("missing body");
    };
    let role = if caller_role == Some("cell") {
        AuthzCallerRole::Cell
    } else {
        AuthzCallerRole::Broker
    };
    let to_write = match parse_mock_tuple_batch(
        &val,
        "writes",
        role,
        "bad write tuple",
        "cell cannot write inference module",
    ) {
        Ok(tuples) => tuples,
        Err(resp) => return resp,
    };
    let to_delete = match parse_mock_tuple_batch(
        &val,
        "deletes",
        role,
        "bad delete tuple",
        "cell cannot delete inference module",
    ) {
        Ok(tuples) => tuples,
        Err(resp) => return resp,
    };
    for t in to_delete {
        store.tuples.remove(&t);
    }
    for t in to_write {
        store.tuples.insert(t);
    }
    (200, "OK", Some(json!({})))
}

fn handle_read(store: &MockOpenFgaStore, body: Option<serde_json::Value>) -> MockHttpResponse {
    let Some(val) = body else {
        return bad_request("missing body");
    };
    let filter_obj = val
        .get("tuple_key")
        .and_then(|k| k.get("object"))
        .and_then(|v| v.as_str());
    let filter_rel = val
        .get("tuple_key")
        .and_then(|k| k.get("relation"))
        .and_then(|v| v.as_str());
    let matched: Vec<serde_json::Value> = store
        .tuples
        .iter()
        .filter(|t| {
            let obj_ok = filter_obj.is_none_or(|o| encode_openfga_object(&t.object) == o);
            let rel_ok = filter_rel.is_none_or(|r| t.relation.as_str() == r);
            obj_ok && rel_ok
        })
        .map(|t| json!({ "key": OpenFgaTupleKey::from_tuple(t) }))
        .collect();
    (200, "OK", Some(json!({ "tuples": matched })))
}

fn handle_list_objects(
    store: &MockOpenFgaStore,
    body: Option<serde_json::Value>,
) -> MockHttpResponse {
    let Some(val) = body else {
        return bad_request("missing body");
    };
    if let Err(resp) = validate_consistency_and_model(store, &val) {
        return resp;
    }
    let Some(type_raw) = val.get("type").and_then(|v| v.as_str()) else {
        return bad_request("missing type");
    };
    let Ok(object_type) = parse_object_type_wire_name(type_raw) else {
        return bad_request("invalid type");
    };
    let Some(relation_raw) = val.get("relation").and_then(|v| v.as_str()) else {
        return bad_request("missing relation");
    };
    let Some(user_raw) = val.get("user").and_then(|v| v.as_str()) else {
        return bad_request("missing user");
    };
    let Ok(subject) = decode_openfga_subject(user_raw) else {
        return bad_request("invalid user");
    };
    let Ok(dummy_tenant) = TenantId::parse("mock.openfga") else {
        return (500, "Internal Server Error", None);
    };
    let objects: Vec<String> = list_objects_in_memory(
        &store.tuples,
        &dummy_tenant,
        &subject,
        &AuthzRelation::new(relation_raw),
        object_type,
    )
    .iter()
    .map(encode_openfga_object)
    .collect();
    (200, "OK", Some(json!({ "objects": objects })))
}

fn handle_list_users(
    store: &MockOpenFgaStore,
    body: Option<serde_json::Value>,
) -> MockHttpResponse {
    let Some(val) = body else {
        return bad_request("missing body");
    };
    if let Err(resp) = validate_consistency_and_model(store, &val) {
        return resp;
    }
    let Some(obj_val) = val.get("object") else {
        return bad_request("missing object");
    };
    let Some(obj_type_raw) = obj_val.get("type").and_then(|v| v.as_str()) else {
        return bad_request("missing object.type");
    };
    let Ok(object_type) = parse_object_type_wire_name(obj_type_raw) else {
        return bad_request("invalid object.type");
    };
    let Some(obj_id) = obj_val.get("id").and_then(|v| v.as_str()) else {
        return bad_request("missing object.id");
    };
    let Some(relation_raw) = val.get("relation").and_then(|v| v.as_str()) else {
        return bad_request("missing relation");
    };
    let Some(filter_type_raw) = val
        .get("user_filters")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|f| f.get("type"))
        .and_then(|v| v.as_str())
    else {
        return bad_request("missing user_filters[0].type");
    };
    let Ok(subject_type) = parse_subject_type_wire_name(filter_type_raw) else {
        return bad_request("invalid user_filters[0].type");
    };
    let Ok(dummy_tenant) = TenantId::parse("mock.openfga") else {
        return (500, "Internal Server Error", None);
    };
    let object = AuthzObject {
        object_type,
        id: obj_id.to_owned(),
    };
    let users: Vec<serde_json::Value> = list_users_in_memory(
        &store.tuples,
        &dummy_tenant,
        &object,
        &AuthzRelation::new(relation_raw),
        subject_type,
    )
    .into_iter()
    .map(|subj| {
        let ty = subject_type_wire_name(subj.subject_type);
        match subj.relation {
            Some(rel) => json!({
                "userset": { "type": ty, "id": subj.id, "relation": rel }
            }),
            None => json!({
                "object": { "type": ty, "id": subj.id }
            }),
        }
    })
    .collect();
    (200, "OK", Some(json!({ "users": users })))
}

fn route_openfga_request(
    state: &Arc<RwLock<MockOpenFgaState>>,
    method: &str,
    path: &str,
    caller_role: Option<String>,
    body: Option<serde_json::Value>,
) -> MockHttpResponse {
    let Ok(mut guard) = state.write() else {
        return json_err(500, "Internal Server Error", "lock poisoned");
    };
    guard.requests.push(RecordedOpenFgaRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        caller_role: caller_role.clone(),
        body: body.clone(),
    });

    if method == "GET" && path == "/stores" {
        return handle_list_stores(&guard);
    }

    if method == "POST" && path == "/stores" {
        return handle_create_store(&mut guard, body);
    }

    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if segments.len() == 2 && segments[0] == "stores" && method == "DELETE" {
        return handle_delete_store(&mut guard, segments[1]);
    }

    if segments.len() == 3 && segments[0] == "stores" && method == "GET" {
        let Some(store) = guard.stores.get(segments[1]) else {
            return json_err(404, "Not Found", "store not found");
        };
        return match segments[2] {
            "authorization-models" => handle_list_models(store),
            _ => json_err(404, "Not Found", "unknown route"),
        };
    }

    if segments.len() == 3 && segments[0] == "stores" && method == "POST" {
        let Some(store) = guard.stores.get_mut(segments[1]) else {
            return json_err(404, "Not Found", "store not found");
        };
        return match segments[2] {
            "authorization-models" => handle_write_model(store, body),
            "check" => handle_check(store, body),
            "write" => handle_write(store, caller_role.as_deref(), body),
            "read" => handle_read(store, body),
            "list-objects" => handle_list_objects(store, body),
            "list-users" => handle_list_users(store, body),
            _ => json_err(404, "Not Found", "unknown route"),
        };
    }

    json_err(404, "Not Found", "unknown route")
}
