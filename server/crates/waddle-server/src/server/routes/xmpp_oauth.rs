//! XMPP OAuth routes (XEP-0493) backed by the auth broker.

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
        .with_state(auth_state)
}

#[derive(Debug, Serialize)]
struct OAuthServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    response_types_supported: Vec<String>,
    grant_types_supported: Vec<String>,
    code_challenge_methods_supported: Vec<String>,
    scopes_supported: Vec<String>,
    token_endpoint_auth_methods_supported: Vec<String>,
}

#[instrument(skip(state))]
pub async fn oauth_discovery_handler(State(state): State<Arc<AuthState>>) -> impl IntoResponse {
    let metadata = OAuthServerMetadata {
        issuer: state.base_url.clone(),
        authorization_endpoint: format!("{}/api/auth/xmpp/authorize", state.base_url),
        token_endpoint: format!("{}/api/auth/xmpp/token", state.base_url),
        response_types_supported: vec!["code".to_string()],
        grant_types_supported: vec!["authorization_code".to_string()],
        code_challenge_methods_supported: vec!["S256".to_string()],
        scopes_supported: vec!["xmpp".to_string()],
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
        "Issued XMPP OAuth bearer token"
    );

    (
        StatusCode::OK,
        Json(XmppTokenResponse {
            access_token: session.id,
            token_type: "Bearer".to_string(),
            expires_in: 3600,
            scope: "xmpp".to_string(),
        }),
    )
        .into_response()
}
