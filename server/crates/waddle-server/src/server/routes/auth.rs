//! Provider-based authentication routes.
//!
//! API:
//! - GET /api/auth/providers
//! - GET /api/auth/start
//! - GET /api/auth/callback
//! - GET /api/auth/session
//! - POST /api/auth/logout

use crate::auth::identity::IdentityService;
use crate::auth::oauth2;
use crate::auth::oidc;
use crate::auth::{
    jid_to_localpart, localpart_to_jid, username_to_localpart, AuthError, AuthProviderConfig,
    AuthProviderKind, NativeUserStore, ProviderRegistry, RegisterRequest, Session, SessionManager,
};
use crate::config::ServerConfig;
use crate::server::AppState;
use axum::{
    extract::{Query, State},
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
    pub session_manager: SessionManager,
    pub identity_service: IdentityService,
    pub native_user_store: NativeUserStore,
    pub providers: ProviderRegistry,
    pub base_url: String,
    pub xmpp_domain: String,
    pub native_auth_enabled: bool,
    pub registration_enabled: bool,
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
        native_auth_enabled: bool,
        registration_enabled: bool,
    ) -> Self {
        let db = Arc::new(app_state.db_pool.global().clone());
        let session_manager =
            SessionManager::new(app_state.db_pool.global_actor().clone(), encryption_key);
        let identity_service = IdentityService::new(Arc::clone(&db));
        let native_user_store = NativeUserStore::new(Arc::clone(&db));
        let providers = ProviderRegistry::new(server_config.auth.providers.clone())
            .unwrap_or_else(|e| panic!("invalid provider config at startup: {}", e));

        Self {
            session_manager,
            identity_service,
            native_user_store,
            providers,
            base_url: server_config.base_url.trim_end_matches('/').to_string(),
            xmpp_domain: std::env::var("WADDLE_XMPP_DOMAIN")
                .unwrap_or_else(|_| "localhost".to_string()),
            native_auth_enabled,
            registration_enabled,
            http_client: reqwest::Client::new(),
            pending_auth: Arc::new(DashMap::new()),
            device_auth: Arc::new(DashMap::new()),
            xmpp_auth_codes: Arc::new(DashMap::new()),
        }
    }

    fn callback_url(&self) -> String {
        format!("{}/api/auth/callback", self.base_url)
    }

    fn websocket_url(&self) -> String {
        let parsed = match url::Url::parse(&self.base_url) {
            Ok(parsed) => parsed,
            Err(_) => return "ws://localhost/xmpp-websocket".to_string(),
        };

        let scheme = match parsed.scheme() {
            "https" => "wss",
            _ => "ws",
        };

        let Some(host) = parsed.host_str() else {
            return "ws://localhost/xmpp-websocket".to_string();
        };

        let authority = match parsed.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };

        format!("{scheme}://{authority}/xmpp-websocket")
    }

    fn session_cookie_header(&self, session_id: Option<&str>, max_age: i64) -> String {
        let mut parts = vec![
            format!("waddle_session={}", session_id.unwrap_or_default()),
            "Path=/".to_string(),
            "HttpOnly".to_string(),
            "SameSite=Lax".to_string(),
            format!("Max-Age={max_age}"),
        ];

        if self.base_url.starts_with("https://") {
            parts.push("Secure".to_string());
        }

        parts.join("; ")
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

    fn session_response(&self, session: Session) -> SessionResponse {
        let is_expired = session.is_expired();
        let expires_at = session.expires_at.map(|v| v.to_rfc3339());
        let jid = localpart_to_jid(&session.xmpp_localpart, &self.xmpp_domain)
            .unwrap_or_else(|_| format!("{}@{}", session.xmpp_localpart, self.xmpp_domain));

        SessionResponse {
            session_id: session.id,
            user_id: session.user_id,
            username: session.username,
            xmpp_localpart: session.xmpp_localpart,
            jid,
            xmpp_websocket_url: self.websocket_url(),
            is_expired,
            expires_at,
        }
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
        let redirect_uri = self.callback_url();

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
        session_transport: BrowserSessionTransport,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserSessionTransport {
    Cookie,
    Fragment,
}

impl BrowserSessionTransport {
    fn from_query(value: &str) -> Result<Self, AuthError> {
        match value {
            "cookie" => Ok(Self::Cookie),
            "fragment" => Ok(Self::Fragment),
            _ => Err(AuthError::InvalidRequest(
                "session_transport must be cookie|fragment".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub provider_id: String,
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
        .route("/api/auth/providers", get(list_providers_handler))
        .route("/api/auth/start", get(start_handler))
        .route("/api/auth/callback", get(callback_handler))
        .route("/api/auth/native/login", post(native_login_handler))
        .route("/api/auth/native/register", post(native_register_handler))
        .route("/api/auth/session", get(session_handler))
        .route("/api/auth/logout", post(logout_handler))
        .with_state(auth_state)
}

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    pub provider: String,
    #[serde(default = "default_flow")]
    pub flow: String,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default = "default_session_transport")]
    pub session_transport: String,

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

fn default_session_transport() -> String {
    "cookie".to_string()
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

#[derive(Debug, Deserialize)]
pub struct NativeLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct NativeRegisterRequest {
    pub username: String,
    pub password: String,
    pub email: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub user_id: String,
    pub username: String,
    pub xmpp_localpart: String,
    pub jid: String,
    pub xmpp_websocket_url: String,
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
        AuthError::InvalidUsername(_) => StatusCode::BAD_REQUEST,
        AuthError::SessionNotFound(_) => StatusCode::NOT_FOUND,
        AuthError::SessionExpired => StatusCode::UNAUTHORIZED,
        AuthError::InvalidPassword(_) => StatusCode::UNAUTHORIZED,
        AuthError::RegistrationDisabled => StatusCode::FORBIDDEN,
        AuthError::UserAlreadyExists(_) => StatusCode::CONFLICT,
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
        AuthError::InvalidUsername(_) => "invalid_username",
        AuthError::InvalidState => "invalid_state",
        AuthError::SessionNotFound(_) => "session_not_found",
        AuthError::SessionExpired => "session_expired",
        AuthError::InvalidPassword(_) => "invalid_password",
        AuthError::RegistrationDisabled => "registration_disabled",
        AuthError::UserAlreadyExists(_) => "user_already_exists",
        AuthError::AuthorizationFailed(_) => "authorization_failed",
        AuthError::TokenExchangeFailed(_) => "token_exchange_failed",
        AuthError::UserInfoFailed(_) => "userinfo_failed",
        AuthError::JwtError(_) => "jwt_error",
        _ => "auth_error",
    };

    (status, Json(ErrorResponse::new(code, &err.to_string())))
}

fn invalid_credentials_response() -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse::new(
            "invalid_credentials",
            "Invalid username or password",
        )),
    )
        .into_response()
}

fn normalize_native_username(input: &str) -> Result<String, AuthError> {
    let localpart = jid_to_localpart(input)?;
    if !localpart.chars().any(|ch| ch.is_alphanumeric()) {
        return Err(AuthError::InvalidUsername(
            "username must contain at least one letter or number".to_string(),
        ));
    }

    Ok(username_to_localpart(&localpart))
}

async fn create_native_session_response(
    state: &AuthState,
    username: &str,
    email: Option<&str>,
) -> Result<axum::response::Response, AuthError> {
    let user = state
        .native_user_store
        .resolve_or_create_user(username, &state.xmpp_domain, email)
        .await?;
    let session = Session::new(&user.id, &user.username, &user.xmpp_localpart);
    state.session_manager.create_session(&session).await?;

    let cookie = state.session_cookie_header(Some(&session.id), 60 * 60 * 24 * 30);
    let body = state.session_response(session);
    let mut response = (StatusCode::OK, Json(body)).into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, cookie.parse().expect("valid cookie"));
    Ok(response)
}

#[instrument(skip(state))]
pub async fn list_providers_handler(State(state): State<Arc<AuthState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.providers.list()))
}

#[instrument(skip(state, body))]
pub async fn native_login_handler(
    State(state): State<Arc<AuthState>>,
    Json(body): Json<NativeLoginRequest>,
) -> impl IntoResponse {
    if !state.native_auth_enabled {
        return auth_error_to_response(AuthError::InvalidRequest(
            "native auth is disabled".to_string(),
        ))
        .into_response();
    }

    let username = match normalize_native_username(&body.username) {
        Ok(value) => value,
        Err(_) => return invalid_credentials_response(),
    };

    match state
        .native_user_store
        .verify_password(&username, &state.xmpp_domain, &body.password)
        .await
    {
        Ok(true) => match create_native_session_response(&state, &username, None).await {
            Ok(response) => response,
            Err(err) => auth_error_to_response(err).into_response(),
        },
        Ok(false) => invalid_credentials_response(),
        Err(err) => auth_error_to_response(err).into_response(),
    }
}

#[instrument(skip(state, body))]
pub async fn native_register_handler(
    State(state): State<Arc<AuthState>>,
    Json(body): Json<NativeRegisterRequest>,
) -> impl IntoResponse {
    if !state.native_auth_enabled {
        return auth_error_to_response(AuthError::InvalidRequest(
            "native auth is disabled".to_string(),
        ))
        .into_response();
    }
    if !state.registration_enabled {
        return auth_error_to_response(AuthError::RegistrationDisabled).into_response();
    }
    if body.password.len() < 8 {
        return auth_error_to_response(AuthError::InvalidPassword(
            "password must be at least 8 characters".to_string(),
        ))
        .into_response();
    }

    let username = match normalize_native_username(&body.username) {
        Ok(value) => value,
        Err(err) => return auth_error_to_response(err).into_response(),
    };
    let email = body
        .email
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());

    let register_result = state
        .native_user_store
        .register(RegisterRequest {
            username: username.clone(),
            domain: state.xmpp_domain.clone(),
            password: body.password,
            email: email.map(str::to_string),
        })
        .await;

    match register_result {
        Ok(_) => match create_native_session_response(&state, &username, email).await {
            Ok(response) => response,
            Err(err) => auth_error_to_response(err).into_response(),
        },
        Err(err) => auth_error_to_response(err).into_response(),
    }
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
        "browser" => {
            let session_transport =
                match BrowserSessionTransport::from_query(&query.session_transport) {
                    Ok(value) => value,
                    Err(err) => return auth_error_to_response(err).into_response(),
                };
            PendingFlow::Browser {
                next: query.next,
                session_transport,
            }
        }
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

    let pending = match state.pending_auth.remove(&state_key) {
        Some((_, pending)) => pending,
        None => return auth_error_to_response(AuthError::InvalidState).into_response(),
    };

    if pending.is_expired() {
        return auth_error_to_response(AuthError::InvalidState).into_response();
    }

    if pending.state != state_key {
        return auth_error_to_response(AuthError::InvalidState).into_response();
    }

    let provider = match state.providers.get(&pending.provider_id) {
        Some(p) => p,
        None => {
            return auth_error_to_response(AuthError::InvalidProvider(pending.provider_id))
                .into_response();
        }
    };

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
        PendingFlow::Browser {
            next,
            session_transport,
        } => {
            let redirect_to = match session_transport {
                BrowserSessionTransport::Cookie => next.unwrap_or_else(|| "/".to_string()),
                BrowserSessionTransport::Fragment => {
                    let target = next.unwrap_or_else(|| "/".to_string());
                    let mut url = match url::Url::parse(&target) {
                        Ok(parsed) => parsed,
                        Err(_) => match url::Url::parse(&state.base_url)
                            .and_then(|base| base.join(&target))
                        {
                            Ok(parsed) => parsed,
                            Err(err) => {
                                return auth_error_to_response(AuthError::InvalidRequest(format!(
                                    "invalid browser redirect target: {}",
                                    err
                                )))
                                .into_response();
                            }
                        },
                    };

                    let mut fragment = url::form_urlencoded::Serializer::new(String::new());
                    if let Some(existing) = url.fragment() {
                        for (key, value) in
                            url::form_urlencoded::parse(existing.as_bytes()).into_owned()
                        {
                            if key != "waddle_session_id" {
                                fragment.append_pair(&key, &value);
                            }
                        }
                    }
                    fragment.append_pair("waddle_session_id", &session.id);
                    url.set_fragment(Some(&fragment.finish()));
                    url.to_string()
                }
            };

            let mut response = Redirect::temporary(&redirect_to).into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                state
                    .session_cookie_header(Some(&session.id), 60 * 60 * 24 * 30)
                    .parse()
                    .expect("valid cookie"),
            );
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
            (StatusCode::OK, Json(state.session_response(session))).into_response()
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
        state
            .session_cookie_header(None, 0)
            .parse()
            .expect("valid cookie"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::server::AppState;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    async fn create_test_auth_state(server_config: &ServerConfig) -> Arc<AuthState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .unwrap();

        let app_state = Arc::new(AppState::new(Arc::new(db_pool)));
        Arc::new(AuthState::new(app_state, server_config, None, true, true))
    }

    #[tokio::test]
    async fn session_response_includes_jid_and_websocket_url() {
        let server_config = ServerConfig::test_homeserver();
        let auth_state = create_test_auth_state(&server_config).await;
        let session = Session::new("user-1", "alice", "alice");
        auth_state
            .session_manager
            .create_session(&session)
            .await
            .unwrap();

        let app = router(auth_state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/session")
                    .header(header::COOKIE, format!("waddle_session={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let expected_jid = format!("alice@{}", auth_state.xmpp_domain);
        assert_eq!(json["username"], "alice");
        assert_eq!(json["jid"].as_str(), Some(expected_jid.as_str()));
        assert_eq!(
            json["xmpp_websocket_url"].as_str(),
            Some("ws://localhost:3000/xmpp-websocket")
        );
    }

    #[tokio::test]
    async fn secure_cookie_header_tracks_base_url_scheme() {
        let mut secure_config = ServerConfig::test_homeserver();
        secure_config.base_url = "https://server.waddle.social".to_string();
        let secure_state = create_test_auth_state(&secure_config).await;
        assert!(secure_state
            .session_cookie_header(Some("token"), 60)
            .contains("Secure"));

        let insecure_config = ServerConfig::test_homeserver();
        let insecure_state = create_test_auth_state(&insecure_config).await;
        assert!(!insecure_state
            .session_cookie_header(Some("token"), 60)
            .contains("Secure"));
    }

    #[tokio::test]
    async fn native_register_and_login_create_sessions() {
        let server_config = ServerConfig::test_homeserver();
        let auth_state = create_test_auth_state(&server_config).await;
        let app = router(auth_state.clone());

        let register_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/native/register")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "username": "Alice",
                            "password": "secret123",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(register_response.status(), StatusCode::OK);
        assert!(register_response
            .headers()
            .get(header::SET_COOKIE)
            .is_some());
        let body = register_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let registered: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(registered["username"], "alice");
        assert_eq!(registered["jid"], "alice@localhost");

        let login_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/native/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "username": "alice@localhost",
                            "password": "secret123",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(login_response.status(), StatusCode::OK);
        let body = login_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let logged_in: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(logged_in["username"], "alice");
        assert_eq!(logged_in["jid"], "alice@localhost");
    }
}
