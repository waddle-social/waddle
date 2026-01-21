//! ATProto OAuth Authentication Routes
//!
//! Provides HTTP endpoints for the ATProto OAuth authentication flow:
//! - POST /v1/auth/atproto/authorize - Start OAuth flow for a handle
//! - GET /v1/auth/atproto/callback - Handle OAuth redirect callback

use crate::auth::{
    AtprotoOAuth, AuthError, Session, SessionManager,
    did_to_jid,
    session::PendingAuthorization,
};
use crate::server::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, instrument, warn};

/// In-memory store for pending OAuth authorizations
/// In production, consider using Redis or database storage
pub type PendingAuthStore = Arc<DashMap<String, PendingAuthorization>>;

/// Extended application state for auth routes
pub struct AuthState {
    /// Core app state (kept for future use accessing database directly)
    #[allow(dead_code)]
    pub app_state: Arc<AppState>,
    /// ATProto OAuth client
    pub oauth_client: AtprotoOAuth,
    /// Session manager
    pub session_manager: SessionManager,
    /// Pending authorizations (state -> PendingAuthorization)
    pub pending_auths: PendingAuthStore,
}

impl AuthState {
    /// Create new auth state
    pub fn new(
        app_state: Arc<AppState>,
        client_id: &str,
        redirect_uri: &str,
        encryption_key: Option<&[u8]>,
    ) -> Self {
        let oauth_client = AtprotoOAuth::new(client_id, redirect_uri);
        let session_manager = SessionManager::new(
            Arc::new(app_state.db_pool.global().clone()),
            encryption_key,
        );

        Self {
            app_state,
            oauth_client,
            session_manager,
            pending_auths: Arc::new(DashMap::new()),
        }
    }
}

/// Create the auth router
pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route("/v1/auth/atproto/authorize", post(authorize_handler))
        .route("/v1/auth/atproto/callback", get(callback_handler))
        .route("/v1/auth/session", get(session_info_handler))
        .route("/v1/auth/logout", post(logout_handler))
        .route("/v1/auth/xmpp-token", get(xmpp_token_handler))
        .with_state(auth_state)
}

/// Request body for authorize endpoint
#[derive(Debug, Deserialize)]
pub struct AuthorizeRequest {
    /// Bluesky/ATProto handle (e.g., "user.bsky.social")
    pub handle: String,
}

/// Response for authorize endpoint
#[derive(Debug, Serialize)]
pub struct AuthorizeResponse {
    /// URL to redirect user to for authorization
    pub authorization_url: String,
    /// State parameter (for debugging/verification)
    pub state: String,
}

/// Query parameters for callback endpoint
#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    /// Authorization code from OAuth server
    pub code: String,
    /// State parameter (must match original request)
    pub state: String,
    /// Issuer (optional, for verification) - prefixed with underscore as we don't use it yet
    #[serde(rename = "iss")]
    pub _iss: Option<String>,
}

/// Response for callback endpoint (on success)
#[derive(Debug, Serialize)]
pub struct CallbackResponse {
    /// Session ID for the authenticated user
    pub session_id: String,
    /// User's DID
    pub did: String,
    /// User's handle
    pub handle: String,
}

/// Session info response
#[derive(Debug, Serialize)]
pub struct SessionInfoResponse {
    /// Session ID
    pub session_id: String,
    /// User's DID
    pub did: String,
    /// User's handle
    pub handle: String,
    /// Whether the session is expired
    pub is_expired: bool,
    /// When the session expires (if set)
    pub expires_at: Option<String>,
}

/// XMPP token response for client connection
#[derive(Debug, Serialize)]
pub struct XmppTokenResponse {
    /// Full JID for XMPP connection (localpart@domain)
    pub jid: String,
    /// Session token for SASL PLAIN authentication
    pub token: String,
    /// XMPP server hostname
    pub xmpp_host: String,
    /// XMPP server port (typically 5222)
    pub xmpp_port: u16,
    /// WebSocket URL for web clients (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket_url: Option<String>,
    /// When the token expires
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Error response
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

impl ErrorResponse {
    fn new(error: &str, message: &str) -> Self {
        Self {
            error: error.to_string(),
            message: message.to_string(),
        }
    }
}

/// Convert AuthError to HTTP response
fn auth_error_to_response(err: AuthError) -> (StatusCode, Json<ErrorResponse>) {
    let (status, error_code) = match &err {
        AuthError::InvalidHandle(_) => (StatusCode::BAD_REQUEST, "invalid_handle"),
        AuthError::DidResolutionFailed(_) => (StatusCode::BAD_GATEWAY, "did_resolution_failed"),
        AuthError::DidDocumentFetchFailed(_) => (StatusCode::BAD_GATEWAY, "did_document_failed"),
        AuthError::OAuthDiscoveryFailed(_) => (StatusCode::BAD_GATEWAY, "oauth_discovery_failed"),
        AuthError::OAuthAuthorizationFailed(_) => (StatusCode::BAD_REQUEST, "oauth_failed"),
        AuthError::TokenExchangeFailed(_) => (StatusCode::BAD_GATEWAY, "token_exchange_failed"),
        AuthError::SessionNotFound(_) => (StatusCode::NOT_FOUND, "session_not_found"),
        AuthError::SessionExpired => (StatusCode::UNAUTHORIZED, "session_expired"),
        AuthError::InvalidState => (StatusCode::BAD_REQUEST, "invalid_state"),
        AuthError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "database_error"),
        AuthError::HttpError(_) => (StatusCode::BAD_GATEWAY, "http_error"),
        AuthError::DnsError(_) => (StatusCode::BAD_GATEWAY, "dns_error"),
        AuthError::InvalidDid(_) => (StatusCode::BAD_REQUEST, "invalid_did"),
    };

    (status, Json(ErrorResponse::new(error_code, &err.to_string())))
}

/// POST /v1/auth/atproto/authorize
///
/// Start the OAuth authorization flow for a Bluesky handle.
/// Returns the authorization URL that the client should redirect to.
#[instrument(skip(state))]
pub async fn authorize_handler(
    State(state): State<Arc<AuthState>>,
    Json(request): Json<AuthorizeRequest>,
) -> impl IntoResponse {
    info!("Starting authorization for handle: {}", request.handle);

    match state.oauth_client.start_authorization(&request.handle).await {
        Ok(auth_request) => {
            // Store pending authorization
            let pending = PendingAuthorization::from_authorization_request(&auth_request);
            state.pending_auths.insert(auth_request.state.clone(), pending);

            info!(
                "Authorization URL generated for handle: {}",
                request.handle
            );

            (
                StatusCode::OK,
                Json(AuthorizeResponse {
                    authorization_url: auth_request.authorization_url,
                    state: auth_request.state,
                }),
            )
                .into_response()
        }
        Err(err) => {
            error!("Authorization failed for {}: {}", request.handle, err);
            let (status, json) = auth_error_to_response(err);
            (status, json).into_response()
        }
    }
}

/// GET /v1/auth/atproto/callback
///
/// Handle the OAuth callback after user authentication.
/// Exchanges the authorization code for tokens and creates a session.
#[instrument(skip(state))]
pub async fn callback_handler(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<CallbackQuery>,
) -> impl IntoResponse {
    info!("OAuth callback received with state: {}", query.state);

    // Look up pending authorization
    let pending = match state.pending_auths.remove(&query.state) {
        Some((_, pending)) => {
            if pending.is_expired() {
                warn!("Pending authorization expired for state: {}", query.state);
                return auth_error_to_response(AuthError::InvalidState).into_response();
            }
            pending
        }
        None => {
            warn!("No pending authorization found for state: {}", query.state);
            return auth_error_to_response(AuthError::InvalidState).into_response();
        }
    };

    // Exchange code for tokens
    match state
        .oauth_client
        .exchange_code(&pending.token_endpoint, &query.code, &pending.code_verifier)
        .await
    {
        Ok(tokens) => {
            info!("Token exchange successful for DID: {}", pending.did);

            // Create session
            let session = Session::from_token_response(
                &pending.did,
                &pending.handle,
                &tokens,
                &pending.token_endpoint,
                &pending.pds_url,
            );

            // Store session
            match state.session_manager.create_session(&session).await {
                Ok(()) => {
                    info!("Session created: {} for {}", session.id, pending.handle);

                    (
                        StatusCode::OK,
                        Json(CallbackResponse {
                            session_id: session.id,
                            did: pending.did,
                            handle: pending.handle,
                        }),
                    )
                        .into_response()
                }
                Err(err) => {
                    error!("Failed to create session: {}", err);
                    auth_error_to_response(err).into_response()
                }
            }
        }
        Err(err) => {
            error!(
                "Token exchange failed for DID {}: {}",
                pending.did, err
            );
            auth_error_to_response(err).into_response()
        }
    }
}

/// GET /v1/auth/session
///
/// Get information about the current session.
/// Requires session_id as a query parameter or header.
#[instrument(skip(state))]
pub async fn session_info_handler(
    State(state): State<Arc<AuthState>>,
    Query(params): Query<SessionQuery>,
) -> impl IntoResponse {
    let session_id = match params.session_id {
        Some(id) => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "missing_session_id",
                    "session_id query parameter is required",
                )),
            )
                .into_response();
        }
    };

    match state.session_manager.get_session(&session_id).await {
        Ok(Some(session)) => {
            let is_expired = session.is_expired();
            let expires_at = session.expires_at.map(|dt| dt.to_rfc3339());
            (
                StatusCode::OK,
                Json(SessionInfoResponse {
                    session_id: session.id,
                    did: session.did,
                    handle: session.handle,
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

#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LogoutRequest {
    pub session_id: String,
}

/// POST /v1/auth/logout
///
/// End the current session.
#[instrument(skip(state))]
pub async fn logout_handler(
    State(state): State<Arc<AuthState>>,
    Json(request): Json<LogoutRequest>,
) -> impl IntoResponse {
    info!("Logout request for session: {}", request.session_id);

    match state.session_manager.delete_session(&request.session_id).await {
        Ok(()) => {
            info!("Session deleted: {}", request.session_id);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            error!("Failed to delete session: {}", err);
            auth_error_to_response(err).into_response()
        }
    }
}

/// Query parameters for XMPP token endpoint
#[derive(Debug, Deserialize)]
pub struct XmppTokenQuery {
    /// Session ID from ATProto authentication
    pub session_id: String,
}

/// GET /v1/auth/xmpp-token
///
/// Get XMPP connection credentials for an authenticated session.
///
/// Takes an ATProto session and returns XMPP connection info:
/// - JID derived from the user's DID
/// - Token for SASL PLAIN authentication
/// - XMPP server host and port
#[instrument(skip(state))]
pub async fn xmpp_token_handler(
    State(state): State<Arc<AuthState>>,
    Query(params): Query<XmppTokenQuery>,
) -> impl IntoResponse {
    info!("XMPP token request for session: {}", params.session_id);

    // Validate the session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Failed to validate session for XMPP token: {}", err);
            return auth_error_to_response(err).into_response();
        }
    };

    // Convert DID to JID
    // Default domain - in production this should come from configuration
    let xmpp_domain = "waddle.social";
    let jid = match did_to_jid(&session.did, xmpp_domain) {
        Ok(jid) => jid,
        Err(err) => {
            error!("Failed to convert DID to JID: {}", err);
            return auth_error_to_response(err).into_response();
        }
    };

    // Use the session ID as the XMPP token
    // In a full implementation, this would be a separate XMPP-specific token
    // that's validated by the XMPP server against the session store
    let token = session.id.clone();

    let expires_at = session.expires_at.map(|dt| dt.to_rfc3339());

    info!("XMPP token generated for JID: {}", jid);

    (
        StatusCode::OK,
        Json(XmppTokenResponse {
            jid,
            token,
            xmpp_host: xmpp_domain.to_string(),
            xmpp_port: 5222,
            websocket_url: Some(format!("wss://{}/xmpp-websocket", xmpp_domain)),
            expires_at,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn create_test_auth_state() -> Arc<AuthState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        let app_state = Arc::new(AppState::new(db_pool));
        Arc::new(AuthState::new(
            app_state,
            "https://waddle.social/oauth/client",
            "https://waddle.social/v1/auth/atproto/callback",
            Some(b"test-encryption-key-32-bytes!!!"),
        ))
    }

    #[tokio::test]
    async fn test_authorize_invalid_handle() {
        let auth_state = create_test_auth_state().await;
        let app = router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/atproto/authorize")
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"handle": "invalid"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "invalid_handle");
    }

    #[tokio::test]
    async fn test_callback_invalid_state() {
        let auth_state = create_test_auth_state().await;
        let app = router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/auth/atproto/callback?code=test&state=invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "invalid_state");
    }

    #[tokio::test]
    async fn test_session_info_missing_id() {
        let auth_state = create_test_auth_state().await;
        let app = router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/auth/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_session_info_not_found() {
        let auth_state = create_test_auth_state().await;
        let app = router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/auth/session?session_id=nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_xmpp_token_missing_session() {
        let auth_state = create_test_auth_state().await;
        let app = router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/auth/xmpp-token?session_id=nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return 404 for non-existent session
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_xmpp_token_response_format() {
        use crate::auth::Session;
        use chrono::{Duration, Utc};

        let auth_state = create_test_auth_state().await;

        // Create a test session directly
        let session = Session {
            id: "test-session-id".to_string(),
            did: "did:plc:abc123xyz789def".to_string(),
            handle: "test.bsky.social".to_string(),
            access_token: "test-token".to_string(),
            refresh_token: None,
            token_endpoint: "https://bsky.social/oauth/token".to_string(),
            pds_url: "https://bsky.social".to_string(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            created_at: Utc::now(),
            last_used_at: Utc::now(),
        };

        // Store the session
        auth_state
            .session_manager
            .create_session(&session)
            .await
            .unwrap();

        let app = router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/auth/xmpp-token?session_id=test-session-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Verify response structure
        assert_eq!(json["jid"], "abc123xyz789def@waddle.social");
        assert_eq!(json["token"], "test-session-id");
        assert_eq!(json["xmpp_host"], "waddle.social");
        assert_eq!(json["xmpp_port"], 5222);
        assert!(json["websocket_url"].is_string());
    }
}
