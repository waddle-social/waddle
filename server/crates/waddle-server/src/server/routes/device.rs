//! OAuth device flow routes (v2).

use super::auth::{AuthState, DeviceAuthStatus, DeviceAuthorization, ErrorResponse, PendingFlow};
use crate::auth::localpart_to_jid;
use axum::{
    extract::{Form, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use chrono::{Duration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, instrument, warn};

pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route("/v2/auth/device/start", post(device_start_handler))
        .route("/v2/auth/device/poll", post(device_poll_handler))
        .route("/v2/auth/device/verify", get(device_verify_page_handler))
        .route("/v2/auth/device/verify", post(device_verify_submit_handler))
        .with_state(auth_state)
}

#[derive(Debug, Deserialize)]
pub struct DeviceStartRequest {
    pub provider: String,
}

#[derive(Debug, Serialize)]
pub struct DeviceStartResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub interval: u32,
    pub expires_in: u32,
}

#[derive(Debug, Deserialize)]
pub struct DevicePollRequest {
    pub device_code: String,
}

#[derive(Debug, Serialize)]
pub struct DevicePollPendingResponse {
    pub status: String,
    pub expires_in: u32,
}

#[derive(Debug, Serialize)]
pub struct DevicePollCompleteResponse {
    pub status: String,
    pub session_id: String,
    pub user_id: String,
    pub username: String,
    pub provider_id: String,
    pub jid: String,
    pub xmpp_host: String,
    pub xmpp_port: u16,
}

#[derive(Debug, Deserialize)]
pub struct VerifyQuery {
    pub code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VerifySubmitRequest {
    pub user_code: String,
}

#[derive(Debug, Serialize)]
pub struct VerifySubmitResponse {
    pub authorization_url: String,
}

fn generate_device_code() -> String {
    let bytes: [u8; 32] = rand::rng().random();
    hex::encode(bytes)
}

fn generate_user_code() -> String {
    let mut rng = rand::rng();
    let letters: String = (0..4)
        .map(|_| {
            let idx = rng.random_range(0..26);
            (b'A' + idx) as char
        })
        .collect();
    let numbers: String = (0..4)
        .map(|_| {
            let idx = rng.random_range(0..10);
            (b'0' + idx) as char
        })
        .collect();
    format!("{}-{}", letters, numbers)
}

#[instrument(skip(state))]
pub async fn device_start_handler(
    State(state): State<Arc<AuthState>>,
    Json(request): Json<DeviceStartRequest>,
) -> impl IntoResponse {
    if state.providers.get(&request.provider).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_provider",
                "Provider is not configured",
            )),
        )
            .into_response();
    }

    let auth = DeviceAuthorization {
        device_code: generate_device_code(),
        user_code: generate_user_code(),
        provider_id: request.provider,
        created_at: Utc::now(),
        expires_at: Utc::now() + Duration::minutes(15),
        status: DeviceAuthStatus::Pending,
        session_id: None,
    };

    let device_code = auth.device_code.clone();
    let user_code = auth.user_code.clone();
    let expires_in = (auth.expires_at - Utc::now()).num_seconds() as u32;

    state.device_auth.insert(device_code.clone(), auth);

    let verification_uri = format!("{}/v2/auth/device/verify", state.base_url);

    (
        StatusCode::OK,
        Json(DeviceStartResponse {
            device_code,
            user_code: user_code.clone(),
            verification_uri: verification_uri.clone(),
            verification_uri_complete: format!("{}?code={}", verification_uri, user_code),
            interval: 5,
            expires_in,
        }),
    )
        .into_response()
}

#[instrument(skip(state))]
pub async fn device_poll_handler(
    State(state): State<Arc<AuthState>>,
    Json(request): Json<DevicePollRequest>,
) -> impl IntoResponse {
    let auth = match state.device_auth.get(&request.device_code) {
        Some(v) => v.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_device_code",
                    "Device code not found or expired",
                )),
            )
                .into_response()
        }
    };

    if auth.is_expired() {
        state.device_auth.remove(&request.device_code);
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "expired_token",
                "Device code has expired",
            )),
        )
            .into_response();
    }

    match auth.status {
        DeviceAuthStatus::Pending | DeviceAuthStatus::InProgress => {
            let expires_in = (auth.expires_at - Utc::now()).num_seconds() as u32;
            (
                StatusCode::OK,
                Json(DevicePollPendingResponse {
                    status: "authorization_pending".to_string(),
                    expires_in,
                }),
            )
                .into_response()
        }
        DeviceAuthStatus::Approved => {
            let Some(session_id) = auth.session_id.clone() else {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse::new(
                        "server_error",
                        "Device authorization approved without session",
                    )),
                )
                    .into_response();
            };

            match state.session_manager.get_session(&session_id).await {
                Ok(Some(session)) => {
                    let xmpp_host = std::env::var("WADDLE_XMPP_DOMAIN")
                        .unwrap_or_else(|_| "localhost".to_string());
                    let xmpp_port = std::env::var("WADDLE_XMPP_PORT")
                        .ok()
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(5222);

                    let jid = localpart_to_jid(&session.xmpp_localpart, &xmpp_host)
                        .unwrap_or_else(|_| format!("{}@{}", session.xmpp_localpart, xmpp_host));

                    (
                        StatusCode::OK,
                        Json(DevicePollCompleteResponse {
                            status: "complete".to_string(),
                            session_id,
                            user_id: session.user_id,
                            username: session.username,
                            provider_id: auth.provider_id,
                            jid,
                            xmpp_host,
                            xmpp_port,
                        }),
                    )
                        .into_response()
                }
                Ok(None) => (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse::new(
                        "invalid_grant",
                        "Session no longer exists",
                    )),
                )
                    .into_response(),
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse::new("server_error", &err.to_string())),
                )
                    .into_response(),
            }
        }
    }
}

#[instrument(skip(state))]
pub async fn device_verify_page_handler(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<VerifyQuery>,
) -> impl IntoResponse {
    let code = query.code.unwrap_or_default();

    let html = format!(
        r#"<!doctype html>
<html>
<head>
<meta charset=\"utf-8\" />
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />
<title>Device Authorization</title>
<style>
body {{ font-family: sans-serif; background:#0f172a; color:#e2e8f0; display:grid; place-items:center; min-height:100vh; margin:0; }}
.card {{ width:min(480px,92vw); background:#1e293b; border:1px solid #334155; border-radius:12px; padding:24px; }}
input, button {{ width:100%; font-size:16px; padding:10px; border-radius:8px; border:1px solid #475569; background:#0f172a; color:#e2e8f0; }}
button {{ margin-top:12px; background:#2563eb; border-color:#1d4ed8; cursor:pointer; }}
.error {{ color:#fca5a5; margin-top:10px; }}
</style>
</head>
<body>
  <div class=\"card\">
    <h1>Authorize Device</h1>
    <p>Enter the code shown in your terminal.</p>
    <form id=\"f\" method=\"post\" action=\"/v2/auth/device/verify\">
      <input name=\"user_code\" value=\"{code}\" placeholder=\"ABCD-1234\" required />
      <button type=\"submit\">Continue</button>
    </form>
  </div>
</body>
</html>"#
    );

    let _ = state; // reserved for future template customization
    Html(html)
}

#[instrument(skip(state))]
pub async fn device_verify_submit_handler(
    State(state): State<Arc<AuthState>>,
    Form(request): Form<VerifySubmitRequest>,
) -> impl IntoResponse {
    let normalized = request.user_code.trim().to_uppercase();

    let selected = state
        .device_auth
        .iter()
        .find(|entry| entry.value().user_code == normalized)
        .map(|entry| entry.key().clone());

    let Some(device_code) = selected else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new("invalid_user_code", "Invalid user code")),
        )
            .into_response();
    };

    {
        let mut auth = match state.device_auth.get_mut(&device_code) {
            Some(v) => v,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse::new(
                        "invalid_device_code",
                        "Device code no longer exists",
                    )),
                )
                    .into_response();
            }
        };

        if auth.is_expired() {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "expired_token",
                    "Device code has expired",
                )),
            )
                .into_response();
        }

        auth.status = DeviceAuthStatus::InProgress;
    }

    let provider_id = match state.device_auth.get(&device_code) {
        Some(v) => v.provider_id.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_device_code",
                    "Device code no longer exists",
                )),
            )
                .into_response();
        }
    };

    let provider = match state.providers.get(&provider_id) {
        Some(v) => v,
        None => {
            warn!(provider_id = %provider_id, "Provider no longer configured during device flow");
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_provider",
                    "Provider no longer configured",
                )),
            )
                .into_response();
        }
    };

    match state
        .start_authorization(provider, PendingFlow::Device { device_code })
        .await
    {
        Ok(url) => {
            info!(provider = %provider.id, "Device flow continuing via provider authorization");
            Json(VerifySubmitResponse {
                authorization_url: url,
            })
            .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse::new("authorization_failed", &err.to_string())),
        )
            .into_response(),
    }
}
