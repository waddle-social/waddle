use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::OperatorError;

const MAX_STORE_LIST_PAGES: usize = 20;

/// OpenFGA Modular Models require schema version `1.2` (matches `waddle-broker`).
pub const OPENFGA_SCHEMA_VERSION: &str = "1.2";

/// HTTP client for idempotent OpenFGA tenant store and authorization-model provisioning.
#[derive(Clone)]
pub struct OpenFgaClient {
    base_url: String,
    bearer_token: Option<String>,
    http: HyperClient<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>,
}

#[derive(Debug, Deserialize)]
struct ListStoresResponse {
    #[serde(default)]
    stores: Vec<StoreSummary>,
    #[serde(default)]
    continuation_token: String,
}

#[derive(Debug, Deserialize)]
struct StoreSummary {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct CreateStoreResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ListModelsResponse {
    #[serde(default)]
    authorization_models: Vec<AuthorizationModelSummary>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationModelSummary {
    id: String,
}

#[derive(Debug, Deserialize)]
struct WriteModelResponse {
    authorization_model_id: String,
}

impl OpenFgaClient {
    /// Creates a new OpenFGA HTTP/HTTPS client targeting `base_url`.
    ///
    /// # Errors
    /// Returns [`OperatorError::OpenFga`] if `base_url` is empty or TLS roots cannot be loaded.
    pub fn new(
        base_url: impl Into<String>,
        bearer_token: Option<String>,
    ) -> Result<Self, OperatorError> {
        let trimmed = base_url.into().trim_end_matches('/').to_owned();
        if trimmed.is_empty() {
            return Err(OperatorError::OpenFga {
                context: "initialize OpenFGA client",
                detail: "OpenFGA base URL must not be empty".to_owned(),
            });
        }

        let https = HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|err| OperatorError::OpenFga {
                context: "load native TLS roots",
                detail: err.to_string(),
            })?
            .https_or_http()
            .enable_http1()
            .build();

        let http = HyperClient::builder(TokioExecutor::new()).build(https);
        Ok(Self {
            base_url: trimmed,
            bearer_token,
            http,
        })
    }

    /// Idempotently ensures an OpenFGA store named `store_name` exists, returning its `store_id`.
    ///
    /// Lists existing stores via `GET /stores` first; if a store with exact name `store_name`
    /// already exists, returns its ID without creating a duplicate. Otherwise creates it via
    /// `POST /stores`.
    ///
    /// # Errors
    /// Returns [`OperatorError::OpenFga`] on network, HTTP, or response decoding failures.
    pub async fn ensure_store(&self, store_name: &str) -> Result<String, OperatorError> {
        if let Some(existing_id) = self.find_store_by_name(store_name).await? {
            return Ok(existing_id);
        }

        let url = format!("{}/stores", self.base_url);
        let body = serde_json::to_vec(&json!({ "name": store_name }))
            .map_err(|err| OperatorError::Serialization(err.to_string()))?;
        let resp_bytes = self
            .send_json(Method::POST, &url, Some(body), "create OpenFGA store")
            .await?;
        let created: CreateStoreResponse =
            serde_json::from_slice(&resp_bytes).map_err(|err| OperatorError::OpenFga {
                context: "decode create store response",
                detail: err.to_string(),
            })?;
        if created.id.trim().is_empty() {
            return Err(OperatorError::OpenFga {
                context: "create OpenFGA store",
                detail: "OpenFGA returned empty store id".to_owned(),
            });
        }
        Ok(created.id)
    }

    /// Idempotently ensures an authorization model exists in `store_id`.
    ///
    /// Checks `GET /stores/{store_id}/authorization-models` first so operator and server reuse
    /// the same model ID when the server provisions first; otherwise writes the canonical
    /// Waddle `schema 1.2` model via `POST /stores/{store_id}/authorization-models`.
    ///
    /// # Errors
    /// Returns [`OperatorError::OpenFga`] on network, HTTP, or response decoding failures.
    pub async fn ensure_waddle_authorization_model(
        &self,
        store_id: &str,
    ) -> Result<String, OperatorError> {
        if let Some(existing_id) = self.find_latest_authorization_model(store_id).await? {
            return Ok(existing_id);
        }
        self.write_waddle_authorization_model(store_id).await
    }

    /// Writes the canonical Waddle `schema 1.2` authorization model into `store_id` via
    /// `POST /stores/{store_id}/authorization-models` and returns the immutable `authorization_model_id`.
    ///
    /// # Errors
    /// Returns [`OperatorError::OpenFga`] on network, HTTP, or response decoding failures.
    pub async fn write_waddle_authorization_model(
        &self,
        store_id: &str,
    ) -> Result<String, OperatorError> {
        let url = format!("{}/stores/{store_id}/authorization-models", self.base_url);
        let body = serde_json::to_vec(&waddle_openfga_authorization_model_json())
            .map_err(|err| OperatorError::Serialization(err.to_string()))?;
        let resp_bytes = self
            .send_json(
                Method::POST,
                &url,
                Some(body),
                "write OpenFGA authorization model",
            )
            .await?;
        let written: WriteModelResponse =
            serde_json::from_slice(&resp_bytes).map_err(|err| OperatorError::OpenFga {
                context: "decode write authorization model response",
                detail: err.to_string(),
            })?;
        if written.authorization_model_id.trim().is_empty() {
            return Err(OperatorError::OpenFga {
                context: "write OpenFGA authorization model",
                detail: "OpenFGA returned empty authorization_model_id".to_owned(),
            });
        }
        Ok(written.authorization_model_id)
    }

    async fn find_latest_authorization_model(
        &self,
        store_id: &str,
    ) -> Result<Option<String>, OperatorError> {
        let url = format!("{}/stores/{store_id}/authorization-models", self.base_url);
        let resp_bytes = self
            .send_json(Method::GET, &url, None, "list OpenFGA authorization models")
            .await?;
        let listed: ListModelsResponse =
            serde_json::from_slice(&resp_bytes).map_err(|err| OperatorError::OpenFga {
                context: "decode list authorization models response",
                detail: err.to_string(),
            })?;
        Ok(listed
            .authorization_models
            .into_iter()
            .map(|m| m.id)
            .find(|id| !id.trim().is_empty()))
    }

    async fn find_store_by_name(&self, store_name: &str) -> Result<Option<String>, OperatorError> {
        let mut token: Option<String> = None;
        for _ in 0..MAX_STORE_LIST_PAGES {
            let url = match &token {
                Some(tok) => format!("{}/stores?continuation_token={tok}", self.base_url),
                None => format!("{}/stores", self.base_url),
            };
            let resp_bytes = self
                .send_json(Method::GET, &url, None, "list OpenFGA stores")
                .await?;
            let page: ListStoresResponse =
                serde_json::from_slice(&resp_bytes).map_err(|err| OperatorError::OpenFga {
                    context: "decode list stores response",
                    detail: err.to_string(),
                })?;
            if let Some(found) = page.stores.into_iter().find(|s| s.name == store_name) {
                return Ok(Some(found.id));
            }
            if page.continuation_token.trim().is_empty() {
                break;
            }
            token = Some(page.continuation_token);
        }
        Ok(None)
    }

    async fn send_json(
        &self,
        method: Method,
        url: &str,
        body: Option<Vec<u8>>,
        context: &'static str,
    ) -> Result<Bytes, OperatorError> {
        let mut req_builder = Request::builder().method(method).uri(url);
        if body.is_some() {
            req_builder = req_builder.header(CONTENT_TYPE, "application/json");
        }
        if let Some(token) = &self.bearer_token {
            req_builder = req_builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let payload = body.map_or_else(Bytes::new, Bytes::from);
        let req = req_builder
            .body(Full::new(payload))
            .map_err(|err| OperatorError::OpenFga {
                context,
                detail: err.to_string(),
            })?;

        let resp = self
            .http
            .request(req)
            .await
            .map_err(|err| OperatorError::OpenFga {
                context,
                detail: err.to_string(),
            })?;
        let status = resp.status();
        let collected = resp
            .into_body()
            .collect()
            .await
            .map_err(|err| OperatorError::OpenFga {
                context,
                detail: err.to_string(),
            })?
            .to_bytes();

        if !matches!(status, StatusCode::OK | StatusCode::CREATED) {
            let preview = String::from_utf8_lossy(&collected);
            let truncated: String = preview.chars().take(256).collect();
            return Err(OperatorError::OpenFga {
                context,
                detail: format!("HTTP {}: {truncated}", status.as_u16()),
            });
        }
        Ok(collected)
    }
}

fn direct_meta(types: &[&str]) -> Value {
    let entries: Vec<Value> = types.iter().map(|t| json!({ "type": t })).collect();
    json!({ "directly_related_user_types": entries })
}

fn computed(rel: &str) -> Value {
    json!({ "computedUserset": { "relation": rel } })
}

fn ttu(tupleset: &str, computed_rel: &str) -> Value {
    json!({
        "tupleToUserset": {
            "tupleset": { "relation": tupleset },
            "computedUserset": { "relation": computed_rel }
        }
    })
}

fn union(children: Vec<Value>) -> Value {
    json!({ "union": { "child": children } })
}

fn diff(base: Value, subtract: Value) -> Value {
    json!({ "difference": { "base": base, "subtract": subtract } })
}

/// Canonical OpenFGA `schema 1.2` JSON authorization model submitted during
/// `POST /stores/{store_id}/authorization-models`.
///
/// Mirrors `waddle-broker::authz::openfga_model::openfga_authorization_model_json()` 1:1.
#[must_use]
pub fn waddle_openfga_authorization_model_json() -> Value {
    let this = || json!({ "this": {} });
    json!({
        "schema_version": OPENFGA_SCHEMA_VERSION,
        "type_definitions": [
            { "type": "user", "metadata": { "module": "chat" } },
            {
                "type": "server",
                "relations": {
                    "owner": this(),
                    "admin": union(vec![this(), computed("owner")]),
                    "member": union(vec![this(), computed("admin")]),
                    "create_space": computed("owner"),
                    "create_muc": computed("owner")
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "owner": direct_meta(&["user"]),
                        "admin": direct_meta(&["user"]),
                        "member": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "space",
                "relations": {
                    "owner": this(),
                    "admin": union(vec![this(), computed("owner")]),
                    "moderator": union(vec![this(), computed("admin")]),
                    "member": union(vec![this(), computed("moderator")]),
                    "read": computed("member"),
                    "view": computed("member"),
                    "manage": computed("admin"),
                    "delete": computed("owner"),
                    "manage_settings": computed("admin"),
                    "manage_roles": computed("admin"),
                    "manage_members": computed("moderator"),
                    "create_channel": computed("owner"),
                    "update": computed("admin")
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "owner": direct_meta(&["user"]),
                        "admin": direct_meta(&["user"]),
                        "moderator": direct_meta(&["user"]),
                        "member": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "channel",
                "relations": {
                    "space": this(),
                    "parent": this(),
                    "outcast": this(),
                    "manager": this(),
                    "moderator": this(),
                    "writer": this(),
                    "viewer": this(),
                    "owner": diff(
                        union(vec![this(), ttu("space", "owner"), ttu("parent", "owner")]),
                        computed("outcast")
                    ),
                    "admin": diff(
                        union(vec![this(), computed("owner"), ttu("space", "admin"), ttu("parent", "admin")]),
                        computed("outcast")
                    ),
                    "member": diff(
                        union(vec![this(), computed("admin"), ttu("space", "member"), ttu("parent", "member")]),
                        computed("outcast")
                    ),
                    "delete": diff(
                        union(vec![ttu("space", "admin"), ttu("parent", "admin")]),
                        computed("outcast")
                    ),
                    "manage": diff(
                        union(vec![computed("owner"), computed("admin"), computed("manager"), ttu("space", "admin"), ttu("parent", "admin")]),
                        computed("outcast")
                    ),
                    "moderate": diff(
                        union(vec![computed("owner"), computed("admin"), computed("moderator"), computed("manager"), ttu("space", "moderator"), ttu("parent", "moderator")]),
                        computed("outcast")
                    ),
                    "send_message": diff(
                        union(vec![computed("member"), computed("writer"), computed("moderator"), computed("manager")]),
                        computed("outcast")
                    ),
                    "read": diff(
                        union(vec![computed("member"), computed("viewer"), computed("writer"), computed("moderator"), computed("manager")]),
                        computed("outcast")
                    ),
                    "view": computed("read")
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "space": direct_meta(&["space"]),
                        "parent": direct_meta(&["space"]),
                        "outcast": direct_meta(&["user"]),
                        "manager": direct_meta(&["user"]),
                        "moderator": direct_meta(&["user"]),
                        "writer": direct_meta(&["user"]),
                        "viewer": direct_meta(&["user"]),
                        "owner": direct_meta(&["user"]),
                        "admin": direct_meta(&["user"]),
                        "member": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "message",
                "relations": {
                    "channel": this(),
                    "author": this(),
                    "read": ttu("channel", "read"),
                    "react": ttu("channel", "read"),
                    "edit": diff(computed("author"), ttu("channel", "outcast")),
                    "delete": union(vec![
                        diff(computed("author"), ttu("channel", "outcast")),
                        ttu("channel", "moderate")
                    ])
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "channel": direct_meta(&["channel"]),
                        "author": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "direct_message",
                "relations": {
                    "participant": this(),
                    "owner": this(),
                    "read": computed("participant"),
                    "send": computed("participant"),
                    "leave": computed("participant"),
                    "manage": computed("owner"),
                    "add_participant": union(vec![computed("owner"), computed("participant")])
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "participant": direct_meta(&["user"]),
                        "owner": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "role",
                "relations": {
                    "space": this(),
                    "parent": this(),
                    "assignee": this(),
                    "member": this(),
                    "manager": this(),
                    "assign": union(vec![computed("manager"), ttu("space", "admin"), ttu("parent", "admin")]),
                    "manage": union(vec![computed("manager"), ttu("space", "admin"), ttu("parent", "admin")]),
                    "delete": union(vec![computed("manager"), ttu("space", "admin"), ttu("parent", "admin")]),
                    "edit": union(vec![computed("manager"), ttu("space", "admin"), ttu("parent", "admin")])
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "space": direct_meta(&["space"]),
                        "parent": direct_meta(&["space"]),
                        "assignee": direct_meta(&["user"]),
                        "member": direct_meta(&["user"]),
                        "manager": direct_meta(&["user"])
                    }
                }
            },
            { "type": "extension", "metadata": { "module": "inference" } },
            {
                "type": "tenant_credential",
                "relations": {
                    "tenant_admin": this(),
                    "allowed_extension": this(),
                    "revoked": this(),
                    "use": diff(
                        union(vec![computed("allowed_extension"), computed("tenant_admin")]),
                        computed("revoked")
                    ),
                    "manage": computed("tenant_admin")
                },
                "metadata": {
                    "module": "inference",
                    "relations": {
                        "tenant_admin": direct_meta(&["user"]),
                        "allowed_extension": direct_meta(&["extension"]),
                        "revoked": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "user_credential",
                "relations": {
                    "owner": this(),
                    "consented_extension": this(),
                    "use": computed("owner"),
                    "invoke_with_consent": union(vec![computed("owner"), computed("consented_extension")]),
                    "manage": computed("owner")
                },
                "metadata": {
                    "module": "inference",
                    "relations": {
                        "owner": direct_meta(&["user"]),
                        "consented_extension": direct_meta(&["extension"])
                    }
                }
            },
            {
                "type": "installed_extension",
                "relations": {
                    "space": this(),
                    "installer": this(),
                    "invoke": union(vec![computed("installer"), ttu("space", "member")])
                },
                "metadata": {
                    "module": "inference",
                    "relations": {
                        "space": direct_meta(&["space"]),
                        "installer": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "room_inference_policy",
                "relations": {
                    "channel": this(),
                    "tenant_funded_allowed": this(),
                    "member_funded_allowed": this(),
                    "disabled": this(),
                    "allow_tenant_funded": diff(
                        union(vec![computed("tenant_funded_allowed"), ttu("channel", "read")]),
                        union(vec![computed("disabled"), ttu("channel", "outcast")])
                    ),
                    "allow_member_funded": diff(
                        union(vec![computed("member_funded_allowed"), ttu("channel", "send_message")]),
                        union(vec![computed("disabled"), ttu("channel", "outcast")])
                    )
                },
                "metadata": {
                    "module": "inference",
                    "relations": {
                        "channel": direct_meta(&["channel"]),
                        "tenant_funded_allowed": direct_meta(&["user"]),
                        "member_funded_allowed": direct_meta(&["user"]),
                        "disabled": direct_meta(&["user"])
                    }
                }
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, State};
    use axum::http::HeaderMap;
    use axum::routing::get;
    use axum::{Json, Router};
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Default)]
    struct MockOpenFgaState {
        expected_token: Option<String>,
        stores: Vec<(String, String)>,
        created_count: usize,
        models_written: Vec<(String, String, Value)>,
    }

    async fn handle_list_stores(
        State(state): State<Arc<Mutex<MockOpenFgaState>>>,
        headers: HeaderMap,
    ) -> (StatusCode, Json<Value>) {
        let g = state.lock().expect("lock");
        if let Some(expected) = &g.expected_token {
            let actual = headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if actual != format!("Bearer {expected}") {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "code": "unauthenticated" })),
                );
            }
        }
        let stores: Vec<Value> = g
            .stores
            .iter()
            .map(|(id, name)| json!({ "id": id, "name": name }))
            .collect();
        (
            StatusCode::OK,
            Json(json!({ "stores": stores, "continuation_token": "" })),
        )
    }

    async fn handle_create_store(
        State(state): State<Arc<Mutex<MockOpenFgaState>>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let mut g = state.lock().expect("lock");
        if let Some(expected) = &g.expected_token {
            let actual = headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if actual != format!("Bearer {expected}") {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "code": "unauthenticated" })),
                );
            }
        }
        g.created_count += 1;
        let id = format!("01STORE{:04}", g.created_count);
        let name = body["name"].as_str().unwrap_or("").to_owned();
        g.stores.push((id.clone(), name.clone()));
        (StatusCode::CREATED, Json(json!({ "id": id, "name": name })))
    }

    async fn handle_list_models(
        State(state): State<Arc<Mutex<MockOpenFgaState>>>,
        Path(store_id): Path<String>,
    ) -> (StatusCode, Json<Value>) {
        let g = state.lock().expect("lock");
        let models: Vec<Value> = g
            .models_written
            .iter()
            .filter(|(sid, _, _)| sid == &store_id)
            .rev()
            .map(|(_, mid, _)| json!({ "id": mid }))
            .collect();
        (
            StatusCode::OK,
            Json(json!({ "authorization_models": models })),
        )
    }

    async fn handle_write_model(
        State(state): State<Arc<Mutex<MockOpenFgaState>>>,
        Path(store_id): Path<String>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let mut g = state.lock().expect("lock");
        let model_id = format!("01MODEL{:04}", g.models_written.len() + 1);
        g.models_written.push((store_id, model_id.clone(), body));
        (
            StatusCode::CREATED,
            Json(json!({ "authorization_model_id": model_id })),
        )
    }

    #[tokio::test]
    async fn ensures_store_and_model_idempotently_with_full_schema_1_2() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let state = Arc::new(Mutex::new(MockOpenFgaState {
            expected_token: Some("test-preshared-key".to_owned()),
            ..MockOpenFgaState::default()
        }));

        let app = Router::new()
            .route("/stores", get(handle_list_stores).post(handle_create_store))
            .route(
                "/stores/{store_id}/authorization-models",
                get(handle_list_models).post(handle_write_model),
            )
            .with_state(Arc::clone(&state));
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = OpenFgaClient::new(
            format!("http://{addr}"),
            Some("test-preshared-key".to_owned()),
        )
        .expect("client");

        let first_id = client
            .ensure_store("waddle-tenant-waddle.social")
            .await
            .expect("create store");
        assert_eq!(first_id, "01STORE0001");

        let second_id = client
            .ensure_store("waddle-tenant-waddle.social")
            .await
            .expect("reuse store");
        assert_eq!(second_id, first_id);
        assert_eq!(state.lock().expect("lock").created_count, 1);

        let model_id = client
            .ensure_waddle_authorization_model(&first_id)
            .await
            .expect("write model");
        assert_eq!(model_id, "01MODEL0001");

        // Second call must reuse the existing model via GET without posting a duplicate.
        let reused_model_id = client
            .ensure_waddle_authorization_model(&first_id)
            .await
            .expect("reuse model");
        assert_eq!(reused_model_id, model_id);

        {
            let guard = state.lock().expect("lock");
            assert_eq!(guard.models_written.len(), 1);
            assert_eq!(guard.models_written[0].0, first_id);
            let model = &guard.models_written[0].2;
            assert_eq!(model["schema_version"].as_str(), Some("1.2"));
            let types = model["type_definitions"]
                .as_array()
                .expect("type_definitions");
            let find_type = |name: &str| {
                types
                    .iter()
                    .find(|t| t["type"].as_str() == Some(name))
                    .unwrap_or_else(|| panic!("missing OpenFGA type {name}"))
            };
            assert_eq!(
                find_type("server")["metadata"]["module"].as_str(),
                Some("chat")
            );
            assert!(find_type("server")["relations"]
                .get("create_space")
                .is_some());
            assert!(find_type("server")["relations"].get("create_muc").is_some());
            assert!(find_type("space")["relations"].get("view").is_some());
            assert!(find_type("space")["relations"]
                .get("manage_settings")
                .is_some());
            assert!(find_type("channel")["relations"].get("parent").is_some());
            assert!(find_type("channel")["relations"].get("view").is_some());
            assert!(find_type("channel")["relations"].get("moderate").is_some());
            assert!(find_type("message")["relations"].get("edit").is_some());
            assert!(find_type("message")["relations"].get("react").is_some());
            assert!(find_type("direct_message")["relations"]
                .get("send")
                .is_some());
            assert!(find_type("role")["relations"].get("assign").is_some());
            assert_eq!(
                find_type("tenant_credential")["metadata"]["module"].as_str(),
                Some("inference")
            );
        }

        let bad_client = OpenFgaClient::new(format!("http://{addr}"), Some("wrong-key".to_owned()))
            .expect("client");
        assert!(bad_client.ensure_store("waddle-tenant-x").await.is_err());
    }
}
