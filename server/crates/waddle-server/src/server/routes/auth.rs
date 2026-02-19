//! Provider-based authentication routes.
//!
//! v2 API:
//! - GET /v2/auth/providers
//! - GET /v2/auth/start
//! - GET /v2/auth/callback/:provider
//! - GET /v2/auth/session
//! - POST /v2/auth/logout

use crate::auth::identity::IdentityService;
use crate::auth::oauth2;
use crate::auth::oidc;
use crate::auth::{
    AuthError, AuthProviderConfig, AuthProviderKind, ProviderRegistry, Session, SessionManager,
};
use crate::config::ServerConfig;
use crate::server::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Redirect},
    routing::{get, post},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::{error, instrument, warn};
use uuid::Uuid;

/// Shared auth state.
pub struct AuthState {
    pub app_state: Arc<AppState>,
    pub session_manager: SessionManager,
    pub identity_service: IdentityService,
    pub providers: ProviderRegistry,
    pub base_url: String,
    pub http_client: reqwest::Client,
    pub pending_auth: Arc<DashMap<String, PendingAuthorization>>,
    pub device_auth: Arc<DashMap<String, DeviceAuthorization>>,
    pub xmpp_auth_codes: Arc<DashMap<String, XmppAuthCode>>,
}

impl AuthState {
    pub fn new(
        app_state: Arc<AppState>,
        server_config: &ServerConfig,
        encryption_key: Option<&[u8]>,
    ) -> Self {
        let db = Arc::new(app_state.db_pool.global().clone());
        let session_manager = SessionManager::new(Arc::clone(&db), encryption_key);
        let identity_service = IdentityService::new(Arc::clone(&db));
        let providers = ProviderRegistry::new(server_config.auth.providers.clone())
            .unwrap_or_else(|e| panic!("invalid provider config at startup: {}", e));

        Self {
            app_state,
            session_manager,
            identity_service,
            providers,
            base_url: server_config.base_url.trim_end_matches('/').to_string(),
            http_client: reqwest::Client::new(),
            pending_auth: Arc::new(DashMap::new()),
            device_auth: Arc::new(DashMap::new()),
            xmpp_auth_codes: Arc::new(DashMap::new()),
        }
    }

    fn callback_url(&self, provider: &str) -> String {
        format!("{}/v2/auth/callback/{}", self.base_url, provider)
    }

    fn create_pkce_verifier() -> String {
        let bytes: [u8; 32] = rand::rng().random();
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn pkce_challenge(verifier: &str) -> String {
        let digest = Sha256::digest(verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(digest)
    }

    fn random_state() -> String {
        let bytes: [u8; 24] = rand::rng().random();
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub async fn start_authorization(
        &self,
        provider: &AuthProviderConfig,
        flow: PendingFlow,
    ) -> Result<String, AuthError> {
        let state = Self::random_state();
        let nonce = Self::random_state();
        let code_verifier = Self::create_pkce_verifier();
        let code_challenge = Self::pkce_challenge(&code_verifier);
        let redirect_uri = self.callback_url(&provider.id);

        let authorization_endpoint = match provider.kind {
            AuthProviderKind::Oidc => {
                let discovery = oidc::discover(
                    &self.http_client,
                    provider.issuer.as_deref().ok_or_else(|| {
                        AuthError::InvalidRequest("oidc provider missing issuer".to_string())
                    })?,
                )
                .await?;
                provider
                    .authorization_endpoint
                    .clone()
                    .unwrap_or(discovery.authorization_endpoint)
            }
            AuthProviderKind::OAuth2 => {
                provider.authorization_endpoint.clone().ok_or_else(|| {
                    AuthError::InvalidRequest(
                        "oauth2 provider missing authorization_endpoint".to_string(),
                    )
                })?
            }
        };

        let mut url = url::Url::parse(&authorization_endpoint).map_err(|e| {
            AuthError::InvalidRequest(format!("invalid authorization endpoint: {}", e))
        })?;

        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("response_type", "code");
            qp.append_pair("client_id", &provider.client_id);
            qp.append_pair("redirect_uri", &redirect_uri);
            qp.append_pair("scope", &provider.scopes_string());
            qp.append_pair("state", &state);
            qp.append_pair("code_challenge", &code_challenge);
            qp.append_pair("code_challenge_method", "S256");
            qp.append_pair("nonce", &nonce);
        }

        self.pending_auth.insert(
            state.clone(),
            PendingAuthorization {
                state,
                provider_id: provider.id.clone(),
                nonce,
                code_verifier,
                redirect_uri,
                flow,
                created_at: Utc::now(),
            },
        );

        Ok(url.to_string())
    }

    fn extract_session_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
        let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
        for pair in cookie_header.split(';') {
            let trimmed = pair.trim();
            if let Some(v) = trimmed.strip_prefix("waddle_session=") {
                return Some(v.to_string());
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct PendingAuthorization {
    pub state: String,
    pub provider_id: String,
    pub nonce: String,
    pub code_verifier: String,
    pub redirect_uri: String,
    pub flow: PendingFlow,
    pub created_at: DateTime<Utc>,
}

impl PendingAuthorization {
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.created_at + Duration::minutes(10)
    }
}

#[derive(Debug, Clone)]
pub enum PendingFlow {
    Browser {
        next: Option<String>,
    },
    Device {
        device_code: String,
    },
    Xmpp {
        client_redirect_uri: String,
        client_state: Option<String>,
        client_code_challenge: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub provider_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub status: DeviceAuthStatus,
    pub session_id: Option<String>,
}

impl DeviceAuthorization {
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAuthStatus {
    Pending,
    InProgress,
    Approved,
}

#[derive(Debug, Clone)]
pub struct XmppAuthCode {
    pub code: String,
    pub session_id: String,
    pub redirect_uri: String,
    pub code_challenge: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl XmppAuthCode {
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.created_at + Duration::minutes(10)
    }
}

pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route("/v2/auth/providers", get(list_providers_handler))
        .route("/v2/auth/start", get(start_handler))
        .route("/v2/auth/callback/:provider", get(callback_handler))
        .route("/v2/auth/session", get(session_handler))
        .route("/v2/auth/logout", post(logout_handler))
        .with_state(auth_state)
}

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    pub provider: String,
    #[serde(default = "default_flow")]
    pub flow: String,
    #[serde(default)]
    pub next: Option<String>,

    // XMPP fields
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub client_state: Option<String>,
    #[serde(default)]
    pub code_challenge: Option<String>,

    // Device field
    #[serde(default)]
    pub device_code: Option<String>,
}

fn default_flow() -> String {
    "browser".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LogoutRequest {
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub user_id: String,
    pub username: String,
    pub xmpp_localpart: String,
    pub is_expired: bool,
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

impl ErrorResponse {
    pub fn new(error: &str, message: &str) -> Self {
        Self {
            error: error.to_string(),
            message: message.to_string(),
        }
    }
}

fn auth_error_to_response(err: AuthError) -> (StatusCode, Json<ErrorResponse>) {
    let status = match err {
        AuthError::InvalidProvider(_) | AuthError::InvalidRequest(_) | AuthError::InvalidState => {
            StatusCode::BAD_REQUEST
        }
        AuthError::SessionNotFound(_) => StatusCode::NOT_FOUND,
        AuthError::SessionExpired => StatusCode::UNAUTHORIZED,
        AuthError::AuthorizationFailed(_)
        | AuthError::TokenExchangeFailed(_)
        | AuthError::UserInfoFailed(_)
        | AuthError::HttpError(_)
        | AuthError::JwtError(_) => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };

    let code = match &err {
        AuthError::InvalidProvider(_) => "invalid_provider",
        AuthError::InvalidRequest(_) => "invalid_request",
        AuthError::InvalidState => "invalid_state",
        AuthError::SessionNotFound(_) => "session_not_found",
        AuthError::SessionExpired => "session_expired",
        AuthError::AuthorizationFailed(_) => "authorization_failed",
        AuthError::TokenExchangeFailed(_) => "token_exchange_failed",
        AuthError::UserInfoFailed(_) => "userinfo_failed",
        AuthError::JwtError(_) => "jwt_error",
        _ => "auth_error",
    };

    (status, Json(ErrorResponse::new(code, &err.to_string())))
}

#[instrument(skip(state))]
pub async fn list_providers_handler(State(state): State<Arc<AuthState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.providers.list()))
}

#[instrument(skip(state))]
pub async fn start_handler(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<StartQuery>,
) -> impl IntoResponse {
    let provider = match state.providers.get(&query.provider) {
        Some(p) => p,
        None => {
            return auth_error_to_response(AuthError::InvalidProvider(query.provider))
                .into_response();
        }
    };

    let flow = match query.flow.as_str() {
        "browser" => PendingFlow::Browser { next: query.next },
        "device" => {
            let Some(device_code) = query.device_code else {
                return auth_error_to_response(AuthError::InvalidRequest(
                    "device flow requires device_code".to_string(),
                ))
                .into_response();
            };
            PendingFlow::Device { device_code }
        }
        "xmpp" => {
            let Some(client_redirect_uri) = query.redirect_uri else {
                return auth_error_to_response(AuthError::InvalidRequest(
                    "xmpp flow requires redirect_uri".to_string(),
                ))
                .into_response();
            };
            PendingFlow::Xmpp {
                client_redirect_uri,
                client_state: query.client_state,
                client_code_challenge: query.code_challenge,
            }
        }
        _ => {
            return auth_error_to_response(AuthError::InvalidRequest(
                "flow must be browser|device|xmpp".to_string(),
            ))
            .into_response();
        }
    };

    match state.start_authorization(provider, flow).await {
        Ok(url) => Redirect::temporary(&url).into_response(),
        Err(err) => auth_error_to_response(err).into_response(),
    }
}

#[instrument(skip(state))]
pub async fn callback_handler(
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> impl IntoResponse {
    if let Some(err) = query.error {
        let msg = query
            .error_description
            .unwrap_or_else(|| "provider returned an error".to_string());
        return auth_error_to_response(AuthError::AuthorizationFailed(format!("{}: {}", err, msg)))
            .into_response();
    }

    let (Some(code), Some(state_key)) = (query.code, query.state) else {
        return auth_error_to_response(AuthError::InvalidRequest("missing code/state".to_string()))
            .into_response();
    };

    let provider = match state.providers.get(&provider_id) {
        Some(p) => p,
        None => {
            return auth_error_to_response(AuthError::InvalidProvider(provider_id)).into_response()
        }
    };

    let pending = match state.pending_auth.remove(&state_key) {
        Some((_, pending)) => pending,
        None => return auth_error_to_response(AuthError::InvalidState).into_response(),
    };

    if pending.is_expired() {
        return auth_error_to_response(AuthError::InvalidState).into_response();
    }

    if pending.provider_id != provider.id {
        return auth_error_to_response(AuthError::InvalidState).into_response();
    }
    if pending.state != state_key {
        return auth_error_to_response(AuthError::InvalidState).into_response();
    }

    let identity_claims = match provider.kind {
        AuthProviderKind::Oidc => {
            let issuer = provider.issuer.as_deref().ok_or_else(|| {
                AuthError::InvalidRequest("oidc provider missing issuer".to_string())
            });
            let issuer = match issuer {
                Ok(v) => v,
                Err(err) => return auth_error_to_response(err).into_response(),
            };

            let discovery = match oidc::discover(&state.http_client, issuer).await {
                Ok(v) => v,
                Err(err) => return auth_error_to_response(err).into_response(),
            };

            let token = match oidc::exchange_authorization_code(
                &state.http_client,
                provider,
                &discovery,
                &code,
                &pending.redirect_uri,
                &pending.code_verifier,
            )
            .await
            {
                Ok(v) => v,
                Err(err) => return auth_error_to_response(err).into_response(),
            };

            match oidc::claims_from_token_response(
                &state.http_client,
                provider,
                &discovery,
                &token,
                Some(&pending.nonce),
            )
            .await
            {
                Ok(v) => v,
                Err(err) => return auth_error_to_response(err).into_response(),
            }
        }
        AuthProviderKind::OAuth2 => {
            let token_endpoint = match provider.token_endpoint.as_deref() {
                Some(v) => v,
                None => {
                    return auth_error_to_response(AuthError::InvalidRequest(
                        "oauth2 provider missing token_endpoint".to_string(),
                    ))
                    .into_response();
                }
            };

            let userinfo_endpoint = match provider.userinfo_endpoint.as_deref() {
                Some(v) => v,
                None => {
                    return auth_error_to_response(AuthError::InvalidRequest(
                        "oauth2 provider missing userinfo_endpoint".to_string(),
                    ))
                    .into_response();
                }
            };

            let token = match oauth2::exchange_code(
                &state.http_client,
                provider,
                token_endpoint,
                &code,
                &pending.redirect_uri,
                &pending.code_verifier,
            )
            .await
            {
                Ok(v) => v,
                Err(err) => return auth_error_to_response(err).into_response(),
            };

            match oidc::claims_from_oauth2_fallback(
                &state.http_client,
                provider,
                provider.issuer.clone(),
                &token.access_token,
                userinfo_endpoint,
            )
            .await
            {
                Ok(v) => v,
                Err(err) => return auth_error_to_response(err).into_response(),
            }
        }
    };

    let linked = match state
        .identity_service
        .resolve_or_create_user(provider, &identity_claims)
        .await
    {
        Ok(v) => v,
        Err(err) => return auth_error_to_response(err).into_response(),
    };

    let session = Session::new(
        &linked.user.id,
        &linked.user.username,
        &linked.user.xmpp_localpart,
    );

    if let Err(err) = state.session_manager.create_session(&session).await {
        return auth_error_to_response(err).into_response();
    }

    match pending.flow {
        PendingFlow::Browser { next } => {
            let redirect_to = next.unwrap_or_else(|| "/".to_string());
            let mut response = Redirect::temporary(&redirect_to).into_response();
            let cookie = format!(
                "waddle_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
                session.id,
                60 * 60 * 24 * 30
            );
            response
                .headers_mut()
                .append(header::SET_COOKIE, cookie.parse().expect("valid cookie"));
            response
        }
        PendingFlow::Device { device_code } => {
            if let Some(mut entry) = state.device_auth.get_mut(&device_code) {
                entry.status = DeviceAuthStatus::Approved;
                entry.session_id = Some(session.id.clone());
            }

            (
                StatusCode::OK,
                axum::response::Html("<html><body><h1>Device authorized</h1><p>You can close this window.</p></body></html>".to_string()),
            )
                .into_response()
        }
        PendingFlow::Xmpp {
            client_redirect_uri,
            client_state,
            client_code_challenge,
        } => {
            let auth_code = Uuid::new_v4().to_string();
            state.xmpp_auth_codes.insert(
                auth_code.clone(),
                XmppAuthCode {
                    code: auth_code.clone(),
                    session_id: session.id,
                    redirect_uri: client_redirect_uri.clone(),
                    code_challenge: client_code_challenge,
                    created_at: Utc::now(),
                },
            );

            let mut redirect = match url::Url::parse(&client_redirect_uri) {
                Ok(v) => v,
                Err(err) => {
                    error!(error = %err, "Invalid XMPP redirect URI");
                    return auth_error_to_response(AuthError::InvalidRequest(
                        "invalid xmpp redirect_uri".to_string(),
                    ))
                    .into_response();
                }
            };

            {
                let mut qp = redirect.query_pairs_mut();
                qp.append_pair("code", &auth_code);
                if let Some(state_value) = client_state {
                    qp.append_pair("state", &state_value);
                }
            }

            Redirect::temporary(redirect.as_str()).into_response()
        }
    }
}

#[instrument(skip(state, headers))]
pub async fn session_handler(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<SessionQuery>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session_id = query
        .session_id
        .or_else(|| AuthState::extract_session_cookie(&headers));

    let Some(session_id) = session_id else {
        return auth_error_to_response(AuthError::SessionNotFound(
            "missing session identifier".to_string(),
        ))
        .into_response();
    };

    match state.session_manager.get_session(&session_id).await {
        Ok(Some(session)) => {
            let is_expired = session.is_expired();
            let expires_at = session.expires_at.map(|v| v.to_rfc3339());
            (
                StatusCode::OK,
                Json(SessionResponse {
                    session_id: session.id,
                    user_id: session.user_id,
                    username: session.username,
                    xmpp_localpart: session.xmpp_localpart,
                    is_expired,
                    expires_at,
                }),
            )
                .into_response()
        }
        Ok(None) => auth_error_to_response(AuthError::SessionNotFound(session_id)).into_response(),
        Err(err) => auth_error_to_response(err).into_response(),
    }
}

#[instrument(skip(state, headers))]
pub async fn logout_handler(
    State(state): State<Arc<AuthState>>,
    headers: axum::http::HeaderMap,
    body: Option<Json<LogoutRequest>>,
) -> impl IntoResponse {
    let requested = body.and_then(|Json(payload)| payload.session_id);
    let session_id = requested.or_else(|| AuthState::extract_session_cookie(&headers));

    if let Some(session_id) = session_id {
        if let Err(err) = state.session_manager.delete_session(&session_id).await {
            warn!(error = %err, "Failed to delete session on logout");
        }
    }

    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().append(
        header::SET_COOKIE,
        "waddle_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
            .parse()
            .expect("valid cookie"),
    );
    resp
}
