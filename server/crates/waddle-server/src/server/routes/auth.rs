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
    localpart_to_jid, AuthError, AuthProviderConfig, AuthProviderKind,
    AuthProviderTokenEndpointAuthMethod, ProviderRegistry, Session, SessionManager,
};
use crate::config::ServerConfig;
use crate::db::actor::{DbExecute, DbQueryOne};
use crate::db::{row_value, ValueExt};
use crate::permissions::PermissionActor;
use crate::server::bootstrap_membership::{reconcile_user_membership, BootstrapMembershipConfig};
use crate::server::routes::auth_telemetry::{
    record_auth_failure, record_auth_success, AuthFailure,
};
use crate::server::AppState;
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Redirect, Response},
    routing::{get, post},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use kameo::actor::ActorRef;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, instrument, warn};
use uuid::Uuid;
use waddle_xmpp::telemetry::attributes::AuthStage;

mod callback;
mod handshake;
mod state;

use callback::callback_handler;
pub use handshake::AuthHandshakeStore;
pub use state::{AuthState, ProfilePublishHook};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAuthorization {
    pub state: String,
    pub provider_id: String,
    pub nonce: String,
    pub code_verifier: String,
    pub redirect_uri: String,
    pub client_id: String,
    pub client_secret: String,
    pub token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod,
    pub require_dpop: bool,
    pub flow: PendingFlow,
    pub created_at: DateTime<Utc>,
}

impl PendingAuthorization {
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.created_at + Duration::minutes(10)
    }

    /// Inclusive boundary (`>=`) to match the store's SQL gates
    /// (`expires_at_ms > now` to act, `<= now` to prune), so an entry
    /// can never pass this check yet be rejected by the database.
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub provider_id: String,
    pub expires_at: DateTime<Utc>,
    pub status: DeviceAuthStatus,
    pub session_id: Option<String>,
}

impl DeviceAuthorization {
    /// Inclusive boundary (`>=`) — see `PendingAuthorization::is_expired`.
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceAuthStatus {
    Pending,
    InProgress,
    Approved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XmppAuthCode {
    pub session_id: String,
    pub redirect_uri: String,
    pub code_challenge: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl XmppAuthCode {
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.created_at + Duration::minutes(10)
    }

    /// Inclusive boundary (`>=`) — see `PendingAuthorization::is_expired`.
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at()
    }
}

pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route("/api/auth/providers", get(list_providers_handler))
        .route("/api/auth/start", get(start_handler))
        .route("/api/auth/callback", get(callback_handler))
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

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub username: String,
    pub avatar_url: Option<String>,
    pub xmpp_localpart: String,
    pub jid: String,
    pub xmpp_websocket_url: String,
    pub link_preview_media_origin: String,
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

pub(super) fn auth_error_to_response(err: AuthError) -> (StatusCode, Json<ErrorResponse>) {
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

pub(super) fn auth_error_response(provider: &str, stage: AuthStage, err: AuthError) -> Response {
    record_auth_failure(provider, None, AuthFailure::from_error(stage, &err));
    match err {
        AuthError::HttpError(detail) => {
            let (code, message) = match stage {
                AuthStage::OidcAuthorization => (
                    "authorization_failed",
                    format!("Authorization failed: {detail}"),
                ),
                AuthStage::OidcCallback => {
                    ("jwt_error", format!("JWT validation failed: {detail}"))
                }
                AuthStage::TokenExchange => (
                    "token_exchange_failed",
                    format!("Token exchange failed: {detail}"),
                ),
                AuthStage::Userinfo => (
                    "userinfo_failed",
                    format!("User info fetch failed: {detail}"),
                ),
                AuthStage::State | AuthStage::DeviceFlow | AuthStage::Scram => {
                    ("auth_error", format!("HTTP request failed: {detail}"))
                }
            };
            (
                StatusCode::BAD_GATEWAY,
                Json(ErrorResponse::new(code, &message)),
            )
                .into_response()
        }
        other => auth_error_to_response(other).into_response(),
    }
}

async fn load_avatar_url(
    actor: &kameo::actor::ActorRef<crate::db::actor::DbActor>,
    user_jid: &str,
) -> Result<Option<String>, AuthError> {
    let row = actor
        .ask(DbQueryOne {
            sql: "SELECT u.avatar_url,
                         (
                             SELECT ai.raw_claims_json
                             FROM auth_identities ai
                             WHERE ai.user_jid = u.jid
                             ORDER BY ai.last_login_at DESC
                             LIMIT 1
                         )
                  FROM users u
                  WHERE u.jid = ? LIMIT 1"
                .to_string(),
            params: vec![user_jid.into()],
        })
        .await
        .map_err(|e| AuthError::DatabaseError(format!("Failed to query avatar_url: {}", e)))?;

    match row {
        Some(row) => {
            let avatar_url = row_value(&row, 0)
                .and_then(ValueExt::as_optional_string)
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to get avatar_url: {}", e))
                })?;
            if avatar_url.is_some() {
                return Ok(avatar_url);
            }

            let raw_claims_json = row_value(&row, 1)
                .and_then(ValueExt::as_optional_string)
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to get raw_claims_json: {}", e))
                })?;
            let Some(raw_claims_json) = raw_claims_json else {
                return Ok(None);
            };

            let claims =
                serde_json::from_str::<serde_json::Value>(&raw_claims_json).map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to parse raw_claims_json: {}", e))
                })?;
            let avatar_url = oidc::avatar_url_from_claims(&claims);
            if let Some(ref avatar_url) = avatar_url {
                actor
                    .ask(DbExecute {
                        sql: "UPDATE users SET avatar_url = ?, updated_at = ? WHERE jid = ?"
                            .to_string(),
                        params: vec![
                            avatar_url.clone().into(),
                            Utc::now().to_rfc3339().into(),
                            user_jid.into(),
                        ],
                    })
                    .await
                    .map_err(|e| {
                        AuthError::DatabaseError(format!("Failed to backfill avatar_url: {}", e))
                    })?;
            }
            Ok(avatar_url)
        }
        None => Ok(None),
    }
}

#[instrument(skip(state))]
pub async fn list_providers_handler(State(state): State<Arc<AuthState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.providers.list()))
}

#[instrument(skip(state, query))]
pub async fn start_handler(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<StartQuery>,
) -> impl IntoResponse {
    let provider = match state.providers.get(&query.provider) {
        Some(p) => p,
        None => {
            record_auth_failure("unknown", None, AuthFailure::AuthorizationInvalidClient);
            return auth_error_to_response(AuthError::InvalidProvider(query.provider))
                .into_response();
        }
    };

    let flow = match query.flow.as_str() {
        "browser" => {
            let session_transport =
                match BrowserSessionTransport::from_query(&query.session_transport) {
                    Ok(value) => value,
                    Err(err) => {
                        return auth_error_response(&provider.id, AuthStage::OidcAuthorization, err)
                    }
                };
            PendingFlow::Browser {
                next: query.next,
                session_transport,
            }
        }
        "device" => {
            let Some(device_code) = query.device_code else {
                record_auth_failure(&provider.id, None, AuthFailure::DeviceMalformed);
                return auth_error_to_response(AuthError::InvalidRequest(
                    "device flow requires device_code".to_string(),
                ))
                .into_response();
            };
            PendingFlow::Device { device_code }
        }
        "xmpp" => {
            let Some(client_redirect_uri) = query.redirect_uri else {
                return auth_error_response(
                    &provider.id,
                    AuthStage::OidcAuthorization,
                    AuthError::InvalidRequest("xmpp flow requires redirect_uri".to_string()),
                );
            };
            PendingFlow::Xmpp {
                client_redirect_uri,
                client_state: query.client_state,
                client_code_challenge: query.code_challenge,
            }
        }
        _ => {
            return auth_error_response(
                &provider.id,
                AuthStage::OidcAuthorization,
                AuthError::InvalidRequest("flow must be browser|device|xmpp".to_string()),
            );
        }
    };

    match state.start_authorization(provider, flow).await {
        Ok(url) => {
            record_auth_success(AuthStage::OidcAuthorization);
            Redirect::temporary(&url).into_response()
        }
        Err(err) => auth_error_response(&provider.id, AuthStage::OidcAuthorization, err),
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
            let jid = localpart_to_jid(&session.xmpp_localpart, &state.xmpp_domain)
                .unwrap_or_else(|_| format!("{}@{}", session.xmpp_localpart, state.xmpp_domain));
            let avatar_url = match load_avatar_url(
                &state.session_manager.actor_ref(),
                &session.user_jid,
            )
            .await
            {
                Ok(avatar_url) => avatar_url,
                Err(err) => return auth_error_to_response(err).into_response(),
            };
            (
                StatusCode::OK,
                Json(SessionResponse {
                    session_id: session.id,
                    username: session.username,
                    avatar_url,
                    xmpp_localpart: session.xmpp_localpart,
                    jid,
                    xmpp_websocket_url: state.websocket_url(),
                    link_preview_media_origin: state.base_url.clone(),
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
        state
            .session_cookie_header(None, 0)
            .parse()
            .expect("valid cookie"),
    );
    resp
}

#[cfg(test)]
mod tests;
