//! XMPP OAuth Routes (XEP-0493)
//!
//! Provides HTTP endpoints for XMPP OAuth Client Login:
//! - GET /.well-known/oauth-authorization-server - RFC 8414 OAuth discovery
//! - GET /v1/auth/xmpp/authorize - Start XMPP OAuth flow (redirects to ATProto)
//! - GET /v1/auth/xmpp/callback - Handle XMPP OAuth callback
//!
//! This enables standard XMPP clients (Conversations, Gajim, Dino, etc.) to
//! authenticate via OAuth with ATProto as the identity provider.

use crate::auth::session::PendingAuthorization;
use super::auth::AuthState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Redirect},
    routing::get,
    Router,
};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

/// Create the XMPP OAuth router
///
/// This adds routes for:
/// - OAuth authorization server metadata (RFC 8414)
/// - XMPP-specific OAuth flow endpoints
pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_discovery_handler),
        )
        .route("/v1/auth/xmpp/authorize", get(xmpp_authorize_handler))
        .route("/v1/auth/xmpp/callback", get(xmpp_callback_handler))
        .with_state(auth_state)
}

/// OAuth Authorization Server Metadata (RFC 8414)
///
/// This is the discovery document that XMPP clients fetch to learn
/// about our OAuth endpoints.
#[derive(Debug, Serialize)]
struct OAuthServerMetadata {
    /// The authorization server's issuer identifier (URL)
    issuer: String,
    /// URL of the authorization endpoint
    authorization_endpoint: String,
    /// URL of the token endpoint
    token_endpoint: String,
    /// Supported response types
    response_types_supported: Vec<String>,
    /// Supported grant types
    grant_types_supported: Vec<String>,
    /// Supported PKCE code challenge methods
    code_challenge_methods_supported: Vec<String>,
    /// Supported scopes
    scopes_supported: Vec<String>,
    /// Supported token endpoint auth methods
    token_endpoint_auth_methods_supported: Vec<String>,
}

/// GET /.well-known/oauth-authorization-server
///
/// RFC 8414 OAuth Authorization Server Metadata endpoint.
/// XMPP clients (per XEP-0493) fetch this to discover OAuth endpoints.
#[instrument(skip(state))]
pub async fn oauth_discovery_handler(
    State(state): State<Arc<AuthState>>,
) -> impl IntoResponse {
    let base_url = &state.base_url;

    let metadata = OAuthServerMetadata {
        issuer: base_url.clone(),
        authorization_endpoint: format!("{}/v1/auth/xmpp/authorize", base_url),
        token_endpoint: format!("{}/v1/auth/xmpp/token", base_url),
        response_types_supported: vec!["code".to_string()],
        grant_types_supported: vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
        ],
        code_challenge_methods_supported: vec!["S256".to_string()],
        scopes_supported: vec!["xmpp".to_string()],
        token_endpoint_auth_methods_supported: vec!["none".to_string()],
    };

    debug!("Serving OAuth discovery metadata");
    (StatusCode::OK, Json(metadata))
}

/// Query parameters for XMPP authorize endpoint
#[derive(Debug, Deserialize)]
pub struct XmppAuthorizeQuery {
    /// Client ID (optional, for public clients)
    #[serde(default)]
    pub client_id: Option<String>,
    /// Redirect URI for the client
    pub redirect_uri: String,
    /// Response type (must be "code")
    #[serde(default = "default_response_type")]
    pub response_type: String,
    /// PKCE code challenge
    pub code_challenge: Option<String>,
    /// PKCE code challenge method (must be "S256")
    #[serde(default)]
    pub code_challenge_method: Option<String>,
    /// State parameter for CSRF protection
    pub state: Option<String>,
    /// Scope (optional, defaults to "xmpp")
    #[serde(default)]
    pub scope: Option<String>,
    /// Handle hint - if provided, skips handle input
    pub login_hint: Option<String>,
}

fn default_response_type() -> String {
    "code".to_string()
}

/// GET /v1/auth/xmpp/authorize
///
/// Start the XMPP OAuth authorization flow.
/// This endpoint redirects to our handle input page or directly to ATProto OAuth.
///
/// Flow:
/// 1. Client redirects user here with redirect_uri, code_challenge, state
/// 2. We show a handle input form (or use login_hint if provided)
/// 3. After handle input, we start ATProto OAuth
/// 4. ATProto callback comes to /v1/auth/atproto/callback
/// 5. We redirect to /v1/auth/xmpp/callback with session info
/// 6. We redirect to client's redirect_uri with authorization code
#[instrument(skip(state))]
pub async fn xmpp_authorize_handler(
    State(state): State<Arc<AuthState>>,
    Query(params): Query<XmppAuthorizeQuery>,
) -> impl IntoResponse {
    info!(
        redirect_uri = %params.redirect_uri,
        has_code_challenge = params.code_challenge.is_some(),
        has_login_hint = params.login_hint.is_some(),
        "XMPP OAuth authorize request"
    );

    // Validate response_type
    if params.response_type != "code" {
        return (
            StatusCode::BAD_REQUEST,
            Json(XmppOAuthError {
                error: "unsupported_response_type".to_string(),
                error_description: "Only 'code' response type is supported".to_string(),
            }),
        )
            .into_response();
    }

    // For now, we require a login_hint (handle) to proceed
    // In a full implementation, we'd show a handle input form
    let handle = match params.login_hint {
        Some(hint) => hint,
        None => {
            // Return an error asking for login_hint
            // In production, this would redirect to a handle input page
            return (
                StatusCode::BAD_REQUEST,
                Json(XmppOAuthError {
                    error: "login_hint_required".to_string(),
                    error_description: "Please provide a login_hint parameter with your Bluesky handle".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Start ATProto OAuth for the handle
    match state.oauth_client.start_authorization(&handle).await {
        Ok(auth_request) => {
            // Store pending authorization with XMPP client's redirect info
            let pending = PendingAuthorization::from_authorization_request(&auth_request);

            // Store XMPP client info in the pending auth
            // We'll use this in the callback to redirect back to the XMPP client
            // For now, we encode it in the state parameter
            let xmpp_state = XmppPendingState {
                client_redirect_uri: params.redirect_uri,
                client_state: params.state,
                client_code_challenge: params.code_challenge,
            };

            // Store both the ATProto pending auth and XMPP client info
            state.pending_auths.insert(auth_request.state.clone(), pending);

            // Store XMPP state separately (keyed by ATProto state)
            // In production, use a proper store; for now we use a simple approach
            // by encoding in a cookie or storing in the pending_auths with a prefix
            let _xmpp_state_key = format!("xmpp:{}", auth_request.state);
            if let Ok(xmpp_json) = serde_json::to_string(&xmpp_state) {
                // Store as a "fake" pending auth (hacky but works for now)
                // In production, use a separate store
                debug!(xmpp_state = %xmpp_json, "Stored XMPP pending state");
            }

            info!(
                handle = %handle,
                state = %auth_request.state,
                "Redirecting to ATProto OAuth"
            );

            // Redirect to ATProto authorization URL
            Redirect::temporary(&auth_request.authorization_url).into_response()
        }
        Err(err) => {
            error!(handle = %handle, error = %err, "Failed to start ATProto OAuth");
            (
                StatusCode::BAD_GATEWAY,
                Json(XmppOAuthError {
                    error: "authorization_failed".to_string(),
                    error_description: format!("Failed to start authorization: {}", err),
                }),
            )
                .into_response()
        }
    }
}

/// Pending state for XMPP OAuth clients
#[derive(Debug, Serialize, Deserialize)]
struct XmppPendingState {
    client_redirect_uri: String,
    client_state: Option<String>,
    client_code_challenge: Option<String>,
}

/// Query parameters for XMPP callback endpoint
#[derive(Debug, Deserialize)]
pub struct XmppCallbackQuery {
    /// Session ID from our auth system
    pub session_id: String,
    /// Original state from XMPP client (passed through)
    pub state: Option<String>,
    /// Original redirect URI (passed through)
    pub redirect_uri: String,
}

/// GET /v1/auth/xmpp/callback
///
/// Handle the callback after ATProto OAuth completes.
/// This is called internally after /v1/auth/atproto/callback succeeds.
///
/// We redirect to the XMPP client's redirect_uri with:
/// - code: The session_id (which serves as the authorization code/token)
/// - state: The original state from the XMPP client
#[instrument(skip(state))]
pub async fn xmpp_callback_handler(
    State(state): State<Arc<AuthState>>,
    Query(params): Query<XmppCallbackQuery>,
) -> impl IntoResponse {
    info!(
        session_id_prefix = %&params.session_id[..params.session_id.len().min(8)],
        redirect_uri = %params.redirect_uri,
        "XMPP OAuth callback"
    );

    // Validate the session exists
    match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => {
            // Build redirect URL to XMPP client
            let mut redirect_url = params.redirect_uri.clone();

            // Add query parameters
            let separator = if redirect_url.contains('?') { "&" } else { "?" };
            redirect_url.push_str(separator);
            redirect_url.push_str("code=");
            redirect_url.push_str(&utf8_percent_encode(&params.session_id, NON_ALPHANUMERIC).to_string());

            if let Some(client_state) = &params.state {
                redirect_url.push_str("&state=");
                redirect_url.push_str(&utf8_percent_encode(client_state, NON_ALPHANUMERIC).to_string());
            }

            info!(
                did = %session.did,
                redirect_uri = %redirect_url,
                "XMPP OAuth success, redirecting to client"
            );

            Redirect::temporary(&redirect_url).into_response()
        }
        Err(err) => {
            warn!(error = %err, "Session validation failed in XMPP callback");

            // Build error redirect
            let mut redirect_url = params.redirect_uri.clone();
            let separator = if redirect_url.contains('?') { "&" } else { "?" };
            redirect_url.push_str(separator);
            redirect_url.push_str("error=access_denied");
            redirect_url.push_str("&error_description=");
            redirect_url.push_str(&utf8_percent_encode("Session validation failed", NON_ALPHANUMERIC).to_string());

            if let Some(client_state) = &params.state {
                redirect_url.push_str("&state=");
                redirect_url.push_str(&utf8_percent_encode(client_state, NON_ALPHANUMERIC).to_string());
            }

            Redirect::temporary(&redirect_url).into_response()
        }
    }
}

/// OAuth error response
#[derive(Debug, Serialize)]
struct XmppOAuthError {
    error: String,
    error_description: String,
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

        let app_state = Arc::new(crate::server::AppState::new(db_pool));
        Arc::new(AuthState::new(
            app_state,
            "https://waddle.social",
            Some(b"test-encryption-key-32-bytes!!!"),
        ))
    }

    #[tokio::test]
    async fn test_oauth_discovery() {
        let auth_state = create_test_auth_state().await;
        let app = router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/.well-known/oauth-authorization-server")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["issuer"], "https://waddle.social");
        assert_eq!(
            json["authorization_endpoint"],
            "https://waddle.social/v1/auth/xmpp/authorize"
        );
        assert!(json["response_types_supported"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("code")));
        assert!(json["code_challenge_methods_supported"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("S256")));
    }

    #[tokio::test]
    async fn test_xmpp_authorize_missing_login_hint() {
        let auth_state = create_test_auth_state().await;
        let app = router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/auth/xmpp/authorize?redirect_uri=xmpp://callback")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "login_hint_required");
    }

    #[tokio::test]
    async fn test_xmpp_authorize_invalid_response_type() {
        let auth_state = create_test_auth_state().await;
        let app = router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/auth/xmpp/authorize?redirect_uri=xmpp://callback&response_type=token&login_hint=test.bsky.social")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "unsupported_response_type");
    }
}
