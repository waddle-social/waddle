//! XEP-0493: OAuth Client Login routes backed by the auth broker.
//!
//! Implements the HTTP-side of XEP-0493 (OAuth Client Login):
//! - RFC 8414 authorization server metadata (`/.well-known/oauth-authorization-server`)
//! - Authorization endpoint with PKCE (RFC 7636)
//! - Token endpoint (Authorization Code grant)
//! - RFC 7591 Dynamic Client Registration (DCR)

use super::auth::{AuthState, ErrorResponse, PendingFlow};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Redirect},
    routing::{get, post},
    Form, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::{debug, instrument, warn};

pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_discovery_handler),
        )
        .route("/api/auth/xmpp/authorize", get(xmpp_authorize_handler))
        .route("/api/auth/xmpp/token", post(xmpp_token_handler))
        .route("/api/auth/xmpp/register", post(xmpp_register_handler))
        .with_state(auth_state)
}

#[derive(Debug, Serialize)]
struct OAuthServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
    response_types_supported: Vec<String>,
    grant_types_supported: Vec<String>,
    code_challenge_methods_supported: Vec<String>,
    scopes_supported: Vec<String>,
    token_endpoint_auth_methods_supported: Vec<String>,
}

/// XEP-0493 scopes per §3.
pub const SCOPE_CLIENT_NORMAL: &str = "xmpp:client:normal";
pub const SCOPE_ACCOUNT_READ: &str = "xmpp:account:read";
pub const SCOPE_ACCOUNT_WRITE: &str = "xmpp:account:write";

#[instrument(skip(state))]
pub async fn oauth_discovery_handler(State(state): State<Arc<AuthState>>) -> impl IntoResponse {
    let metadata = OAuthServerMetadata {
        issuer: state.base_url.clone(),
        authorization_endpoint: format!("{}/api/auth/xmpp/authorize", state.base_url),
        token_endpoint: format!("{}/api/auth/xmpp/token", state.base_url),
        registration_endpoint: format!("{}/api/auth/xmpp/register", state.base_url),
        response_types_supported: vec!["code".to_string()],
        grant_types_supported: vec!["authorization_code".to_string()],
        code_challenge_methods_supported: vec!["S256".to_string()],
        scopes_supported: vec![
            SCOPE_CLIENT_NORMAL.to_string(),
            SCOPE_ACCOUNT_READ.to_string(),
            SCOPE_ACCOUNT_WRITE.to_string(),
        ],
        token_endpoint_auth_methods_supported: vec!["none".to_string()],
    };

    (StatusCode::OK, Json(metadata))
}

#[derive(Debug, Deserialize)]
pub struct XmppAuthorizeQuery {
    #[serde(default)]
    pub provider: Option<String>,
    pub redirect_uri: String,
    #[serde(default = "default_response_type")]
    pub response_type: String,
    #[serde(default)]
    pub code_challenge: Option<String>,
    #[serde(default)]
    pub code_challenge_method: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

fn default_response_type() -> String {
    "code".to_string()
}

/// Recognised XEP-0493 scopes.
const VALID_SCOPES: &[&str] = &[SCOPE_CLIENT_NORMAL, SCOPE_ACCOUNT_READ, SCOPE_ACCOUNT_WRITE];

/// Validate a space-separated scope string. Returns the validated scope
/// or a default of `xmpp:client:normal` when none is provided.
fn validate_scope(scope: Option<&str>) -> Result<String, String> {
    let requested = scope.unwrap_or(SCOPE_CLIENT_NORMAL);
    for s in requested.split_whitespace() {
        if !VALID_SCOPES.contains(&s) {
            return Err(format!("Unknown scope: {}", s));
        }
    }
    Ok(requested.to_string())
}

#[instrument(skip(state))]
pub async fn xmpp_authorize_handler(
    State(state): State<Arc<AuthState>>,
    Query(params): Query<XmppAuthorizeQuery>,
) -> impl IntoResponse {
    if params.response_type != "code" {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "unsupported_response_type",
                "Only response_type=code is supported",
            )),
        )
            .into_response();
    }

    if let Some(method) = params.code_challenge_method.as_deref() {
        if method != "S256" {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_request",
                    "Only code_challenge_method=S256 is supported",
                )),
            )
                .into_response();
        }
    }

    // XEP-0493 §3: validate requested scope
    let granted_scope = match validate_scope(params.scope.as_deref()) {
        Ok(s) => s,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("invalid_scope", &msg)),
            )
                .into_response();
        }
    };

    let provider_id = match params.provider {
        Some(v) => v,
        None => {
            let list = state.providers.list();
            if let Some(default) = list.first() {
                default.id.clone()
            } else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse::new(
                        "invalid_provider",
                        "No auth providers configured",
                    )),
                )
                    .into_response();
            }
        }
    };

    let provider = match state.providers.get(&provider_id) {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("invalid_provider", "Unknown provider")),
            )
                .into_response();
        }
    };

    match state
        .start_authorization(
            provider,
            PendingFlow::Xmpp {
                client_redirect_uri: params.redirect_uri,
                client_state: params.state,
                client_code_challenge: params.code_challenge,
                granted_scope,
            },
        )
        .await
    {
        Ok(url) => Redirect::temporary(&url).into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse::new("authorization_failed", &err.to_string())),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct XmppTokenRequest {
    pub grant_type: String,
    pub code: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub code_verifier: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct XmppTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u32,
    pub scope: String,
}

fn pkce_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

#[instrument(skip(state))]
pub async fn xmpp_token_handler(
    State(state): State<Arc<AuthState>>,
    Form(request): Form<XmppTokenRequest>,
) -> impl IntoResponse {
    if request.grant_type != "authorization_code" {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "unsupported_grant_type",
                "Only authorization_code grant is supported",
            )),
        )
            .into_response();
    }

    // RFC 6749 §4.1.3: public clients MUST send client_id.
    // If a DCR-registered client_id is provided, validate it.
    if let Some(ref cid) = request.client_id {
        if cid.starts_with("dcr-") && !state.dynamic_clients.contains_key(cid) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("invalid_client", "Unknown client_id")),
            )
                .into_response();
        }
    }

    let code = match state.xmpp_auth_codes.remove(&request.code) {
        Some((_, code)) => code,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_grant",
                    "Invalid or expired code",
                )),
            )
                .into_response();
        }
    };

    if code.is_expired() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_grant",
                "Authorization code expired",
            )),
        )
            .into_response();
    }

    if code.redirect_uri != request.redirect_uri {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new("invalid_grant", "redirect_uri mismatch")),
        )
            .into_response();
    }

    if let Some(challenge) = code.code_challenge.as_deref() {
        let Some(verifier) = request.code_verifier.as_deref() else {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_request",
                    "code_verifier is required",
                )),
            )
                .into_response();
        };

        if pkce_s256(verifier) != challenge {
            warn!("XMPP token exchange failed PKCE verification");
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_grant",
                    "PKCE verification failed",
                )),
            )
                .into_response();
        }
    }

    let session = match state.session_manager.get_session(&code.session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("invalid_grant", "Session not found")),
            )
                .into_response();
        }
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new("server_error", &err.to_string())),
            )
                .into_response();
        }
    };

    debug!(
        user_id = %session.user_id,
        username = %session.username,
        scope = %code.granted_scope,
        "Issued XMPP OAuth bearer token"
    );

    (
        StatusCode::OK,
        Json(XmppTokenResponse {
            access_token: session.id,
            token_type: "Bearer".to_string(),
            expires_in: 3600,
            scope: code.granted_scope,
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// RFC 7591: OAuth 2.0 Dynamic Client Registration
// Required by XEP-0493 §2.3.2 so third-party clients can auto-register.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DcrRequest {
    /// Human-readable client name.
    #[serde(default)]
    pub client_name: Option<String>,
    /// Redirect URIs the client will use.
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    /// URI for the client's homepage/logo.
    #[serde(default)]
    pub client_uri: Option<String>,
    /// Grant types the client intends to use.
    #[serde(default)]
    pub grant_types: Vec<String>,
    /// Token endpoint auth method. Public XMPP clients use "none".
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
    /// Response types (should contain "code").
    #[serde(default)]
    pub response_types: Vec<String>,
    /// Software identifier.
    #[serde(default)]
    pub software_id: Option<String>,
    /// Software version.
    #[serde(default)]
    pub software_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DcrResponse {
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub client_id_issued_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub token_endpoint_auth_method: String,
    pub response_types: Vec<String>,
}

/// RFC 7591 Dynamic Client Registration endpoint.
///
/// XEP-0493 §2.3.2 requires this so third-party XMPP clients can register
/// themselves before initiating the authorization flow.
///
/// For now this is a permissive implementation that accepts any registration
/// and issues a client_id. A production deployment should add rate-limiting
/// and may persist registrations for audit/revocation.
#[instrument(skip(state))]
pub async fn xmpp_register_handler(
    State(state): State<Arc<AuthState>>,
    Json(request): Json<DcrRequest>,
) -> impl IntoResponse {
    // Validate redirect_uris — at least one is required
    if request.redirect_uris.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_client_metadata",
                "At least one redirect_uri is required",
            )),
        )
            .into_response();
    }

    // Validate redirect URIs use https (or localhost for dev)
    for uri in &request.redirect_uris {
        match url::Url::parse(uri) {
            Ok(parsed) => {
                let is_localhost = parsed.host_str().map_or(false, |h| {
                    h == "localhost" || h == "127.0.0.1" || h == "[::1]"
                });
                if parsed.scheme() != "https" && !is_localhost {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse::new(
                            "invalid_redirect_uri",
                            "redirect_uris must use https (except localhost)",
                        )),
                    )
                        .into_response();
                }
            }
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse::new(
                        "invalid_redirect_uri",
                        "Invalid redirect_uri format",
                    )),
                )
                    .into_response();
            }
        }
    }

    // Generate a unique client_id
    let client_id = format!("dcr-{}", uuid::Uuid::new_v4());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let auth_method = request
        .token_endpoint_auth_method
        .unwrap_or_else(|| "none".to_string());

    let grant_types = if request.grant_types.is_empty() {
        vec!["authorization_code".to_string()]
    } else {
        request.grant_types
    };

    let response_types = if request.response_types.is_empty() {
        vec!["code".to_string()]
    } else {
        request.response_types
    };

    // Store the registration for later validation
    state.register_dynamic_client(
        &client_id,
        &request.redirect_uris,
        request.client_name.as_deref(),
    );

    debug!(
        client_id = %client_id,
        client_name = ?request.client_name,
        redirect_uris = ?request.redirect_uris,
        "Registered dynamic OAuth client"
    );

    (
        StatusCode::CREATED,
        Json(DcrResponse {
            client_id,
            client_secret: None, // Public clients don't get secrets
            client_id_issued_at: now,
            client_name: request.client_name,
            redirect_uris: request.redirect_uris,
            grant_types,
            token_endpoint_auth_method: auth_method,
            response_types,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{
        AuthProviderConfig, AuthProviderKind, AuthProviderTokenEndpointAuthMethod, Session,
    };
    use crate::config::{AuthConfig, ServerConfig, ServerMode};
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::server::routes::auth::XmppAuthCode;
    use crate::server::AppState;
    use axum::{body::Body, http::Request};
    use chrono::{Duration, Utc};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_provider() -> AuthProviderConfig {
        AuthProviderConfig {
            id: "github".to_string(),
            display_name: "GitHub".to_string(),
            kind: AuthProviderKind::OAuth2,
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::ClientSecretPost,
            scopes: vec!["read:user".to_string()],
            issuer: Some("https://github.com".to_string()),
            authorization_endpoint: Some("https://github.com/login/oauth/authorize".to_string()),
            token_endpoint: Some("https://github.com/login/oauth/access_token".to_string()),
            userinfo_endpoint: Some("https://api.github.com/user".to_string()),
            jwks_uri: None,
            subject_claim: "id".to_string(),
            username_claim: Some("login".to_string()),
            email_claim: Some("email".to_string()),
        }
    }

    fn test_server_config() -> ServerConfig {
        ServerConfig {
            mode: ServerMode::HomeServer,
            base_url: "http://localhost:3000".to_string(),
            session_key: Some("test-key-32-bytes-long-for-aes!".to_string()),
            auth: AuthConfig {
                providers: vec![test_provider()],
            },
        }
    }

    async fn create_test_auth_state() -> Arc<AuthState> {
        let db_pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig::default())
            .await
            .expect("database pool should initialize");

        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .expect("migrations should run");

        let server_config = test_server_config();
        let app_state = Arc::new(AppState::new(Arc::new(db_pool)));

        Arc::new(AuthState::new(
            app_state,
            &server_config,
            server_config.session_key.as_ref().map(|s| s.as_bytes()),
        ))
    }

    async fn create_session(state: &Arc<AuthState>) -> Session {
        let session = Session::new("user-1", "alice", "alice");
        state
            .session_manager
            .create_session(&session)
            .await
            .expect("session should be created");
        session
    }

    fn insert_auth_code(
        state: &Arc<AuthState>,
        code: &str,
        session_id: &str,
        redirect_uri: &str,
        code_challenge: Option<String>,
        created_at: chrono::DateTime<Utc>,
    ) {
        state.xmpp_auth_codes.insert(
            code.to_string(),
            XmppAuthCode {
                session_id: session_id.to_string(),
                redirect_uri: redirect_uri.to_string(),
                code_challenge,
                granted_scope: SCOPE_CLIENT_NORMAL.to_string(),
                created_at,
            },
        );
    }

    fn encode_form(fields: &[(&str, &str)]) -> String {
        fields
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}={}",
                    urlencoding::encode(key),
                    urlencoding::encode(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&")
    }

    async fn response_json(response: axum::response::Response) -> Value {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body should be readable")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("response body should be valid json")
    }

    #[tokio::test]
    async fn xmpp_authorize_rejects_unsupported_response_type() {
        let state = create_test_auth_state().await;
        let app = router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/auth/xmpp/authorize?response_type=token&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await;
        assert_eq!(body["error"], "unsupported_response_type");
    }

    #[tokio::test]
    async fn xmpp_token_accepts_valid_pkce_verifier() {
        let state = create_test_auth_state().await;
        let session = create_session(&state).await;

        let code = "auth-code-valid-pkce";
        let redirect_uri = "https://client.example/callback";
        let code_verifier = "valid-verifier-123";
        let code_challenge = pkce_s256(code_verifier);

        insert_auth_code(
            &state,
            code,
            &session.id,
            redirect_uri,
            Some(code_challenge),
            Utc::now(),
        );

        let app = router(state.clone());
        let body = encode_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ]);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/xmpp/token")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["access_token"], session.id);
        assert_eq!(payload["token_type"], "Bearer");
        assert!(!state.xmpp_auth_codes.contains_key(code));
    }

    #[tokio::test]
    async fn xmpp_token_rejects_invalid_pkce_verifier() {
        let state = create_test_auth_state().await;
        let session = create_session(&state).await;

        let code = "auth-code-invalid-pkce";
        let redirect_uri = "https://client.example/callback";

        insert_auth_code(
            &state,
            code,
            &session.id,
            redirect_uri,
            Some(pkce_s256("expected-verifier")),
            Utc::now(),
        );

        let app = router(state);
        let body = encode_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", "wrong-verifier"),
        ]);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/xmpp/token")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response_json(response).await;
        assert_eq!(payload["error"], "invalid_grant");
        assert_eq!(payload["message"], "PKCE verification failed");
    }

    #[tokio::test]
    async fn xmpp_token_rejects_edge_cases() {
        let state = create_test_auth_state().await;
        let session = create_session(&state).await;
        let app = router(state.clone());

        // unsupported grant type
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/xmpp/token")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(encode_form(&[
                        ("grant_type", "refresh_token"),
                        ("code", "any-code"),
                        ("redirect_uri", "https://client.example/callback"),
                    ])))
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response_json(response).await;
        assert_eq!(payload["error"], "unsupported_grant_type");

        // redirect_uri mismatch
        insert_auth_code(
            &state,
            "code-redirect-mismatch",
            &session.id,
            "https://client.example/callback-a",
            None,
            Utc::now(),
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/xmpp/token")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(encode_form(&[
                        ("grant_type", "authorization_code"),
                        ("code", "code-redirect-mismatch"),
                        ("redirect_uri", "https://client.example/callback-b"),
                    ])))
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response_json(response).await;
        assert_eq!(payload["error"], "invalid_grant");
        assert_eq!(payload["message"], "redirect_uri mismatch");

        // expired code
        insert_auth_code(
            &state,
            "code-expired",
            &session.id,
            "https://client.example/callback",
            None,
            Utc::now() - Duration::minutes(11),
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/xmpp/token")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(encode_form(&[
                        ("grant_type", "authorization_code"),
                        ("code", "code-expired"),
                        ("redirect_uri", "https://client.example/callback"),
                    ])))
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response_json(response).await;
        assert_eq!(payload["error"], "invalid_grant");
        assert_eq!(payload["message"], "Authorization code expired");

        // missing session
        insert_auth_code(
            &state,
            "code-missing-session",
            "missing-session-id",
            "https://client.example/callback",
            None,
            Utc::now(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/xmpp/token")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(encode_form(&[
                        ("grant_type", "authorization_code"),
                        ("code", "code-missing-session"),
                        ("redirect_uri", "https://client.example/callback"),
                    ])))
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response_json(response).await;
        assert_eq!(payload["error"], "invalid_grant");
        assert_eq!(payload["message"], "Session not found");
    }
}
