//! OAuth device flow routes.

use super::auth::{AuthState, DeviceAuthStatus, DeviceAuthorization, ErrorResponse, PendingFlow};
use super::auth_telemetry::{record_auth_failure, record_auth_success, AuthFailure};
use crate::auth::localpart_to_jid;
use axum::{
    extract::{Form, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, instrument, warn};
use waddle_xmpp::telemetry::attributes::AuthStage;

pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route("/api/auth/device/start", post(device_start_handler))
        .route("/api/auth/device/poll", post(device_poll_handler))
        .route("/api/auth/device/verify", get(device_verify_page_handler))
        .route(
            "/api/auth/device/verify",
            post(device_verify_submit_handler),
        )
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
    pub username: String,
    pub provider_id: String,
    pub jid: String,
    pub xmpp_host: String,
    pub websocket_url: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyQuery {
    pub code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VerifySubmitRequest {
    pub user_code: String,
}

fn escape_html_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn render_device_verify_html(code: &str) -> String {
    let escaped_code = escape_html_attr(code);
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
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
  <div class="card">
    <h1>Authorize Device</h1>
    <p>Enter the code shown in your terminal.</p>
    <form id="f" method="post" action="/api/auth/device/verify">
      <input name="user_code" value="{escaped_code}" placeholder="ABCD-1234" required />
      <button type="submit">Continue</button>
    </form>
  </div>
</body>
</html>"#
    )
}

fn generate_device_code() -> String {
    use rand::RngExt;
    let bytes: [u8; 32] = rand::rng().random();
    hex::encode(bytes)
}

fn generate_user_code() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    let letters: String = (0..4)
        .map(|_| {
            let idx: u8 = rng.random_range(0..26);
            (b'A' + idx) as char
        })
        .collect();
    let numbers: String = (0..4)
        .map(|_| {
            let idx: u8 = rng.random_range(0..10);
            (b'0' + idx) as char
        })
        .collect();
    format!("{}-{}", letters, numbers)
}

#[instrument(skip(state, request))]
pub async fn device_start_handler(
    State(state): State<Arc<AuthState>>,
    Json(request): Json<DeviceStartRequest>,
) -> impl IntoResponse {
    if state.providers.get(&request.provider).is_none() {
        record_auth_failure("unknown", None, AuthFailure::DeviceInvalidClient);
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_provider",
                "Provider is not configured",
            )),
        )
            .into_response();
    }

    // `user_code` is unique in the store; regenerate both codes on a
    // collision with another active grant (the insert is ON CONFLICT
    // DO NOTHING, so a loss is reported as `false`, not an error).
    const INSERT_ATTEMPTS: usize = 5;
    let mut auth = None;
    for _ in 0..INSERT_ATTEMPTS {
        let candidate = DeviceAuthorization {
            device_code: generate_device_code(),
            user_code: generate_user_code(),
            provider_id: request.provider.clone(),
            expires_at: Utc::now() + Duration::minutes(15),
            status: DeviceAuthStatus::Pending,
            session_id: None,
        };

        match state.auth_handshake.insert_device(&candidate).await {
            Ok(true) => {
                auth = Some(candidate);
                break;
            }
            Ok(false) => continue,
            Err(err) => {
                record_auth_failure(
                    &request.provider,
                    None,
                    AuthFailure::from_error(AuthStage::DeviceFlow, &err),
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse::new("server_error", &err.to_string())),
                )
                    .into_response();
            }
        }
    }

    let Some(auth) = auth else {
        record_auth_failure(&request.provider, None, AuthFailure::DeviceOther);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(
                "server_error",
                "Could not allocate a device code; try again",
            )),
        )
            .into_response();
    };

    let device_code = auth.device_code.clone();
    let user_code = auth.user_code.clone();
    let expires_in = (auth.expires_at - Utc::now()).num_seconds() as u32;

    let verification_uri = format!("{}/api/auth/device/verify", state.base_url);

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

#[instrument(skip(state, request))]
pub async fn device_poll_handler(
    State(state): State<Arc<AuthState>>,
    Json(request): Json<DevicePollRequest>,
) -> impl IntoResponse {
    let auth = match state.auth_handshake.get_device(&request.device_code).await {
        Ok(Some(v)) => v,
        Ok(None) => {
            record_auth_failure("unknown", None, AuthFailure::DeviceInvalidCode);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_device_code",
                    "Device code not found or expired",
                )),
            )
                .into_response();
        }
        Err(err) => {
            record_auth_failure(
                "unknown",
                None,
                AuthFailure::from_error(AuthStage::DeviceFlow, &err),
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new("server_error", &err.to_string())),
            )
                .into_response();
        }
    };

    if auth.is_expired() {
        if let Err(err) = state
            .auth_handshake
            .remove_device(&request.device_code)
            .await
        {
            warn!(error = %err, "Failed to remove expired device authorization");
        }
        record_auth_failure(&auth.provider_id, None, AuthFailure::DeviceExpired);
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
                record_auth_failure(&auth.provider_id, None, AuthFailure::DeviceInvalidGrant);
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
                    let websocket_url = state.websocket_url();

                    let jid = localpart_to_jid(&session.xmpp_localpart, &xmpp_host)
                        .unwrap_or_else(|_| format!("{}@{}", session.xmpp_localpart, xmpp_host));

                    (
                        StatusCode::OK,
                        Json(DevicePollCompleteResponse {
                            status: "complete".to_string(),
                            session_id,
                            username: session.username,
                            provider_id: auth.provider_id,
                            jid,
                            xmpp_host,
                            websocket_url,
                        }),
                    )
                        .into_response()
                }
                Ok(None) => {
                    record_auth_failure(&auth.provider_id, None, AuthFailure::DeviceInvalidGrant);
                    (
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse::new(
                            "invalid_grant",
                            "Session no longer exists",
                        )),
                    )
                        .into_response()
                }
                Err(err) => {
                    record_auth_failure(
                        &auth.provider_id,
                        None,
                        AuthFailure::from_error(AuthStage::DeviceFlow, &err),
                    );
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse::new("server_error", &err.to_string())),
                    )
                        .into_response()
                }
            }
        }
    }
}

#[instrument(skip(state, query))]
pub async fn device_verify_page_handler(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<VerifyQuery>,
) -> impl IntoResponse {
    let code = query.code.unwrap_or_default();
    let html = render_device_verify_html(&code);

    let _ = state; // reserved for future template customization
    Html(html)
}

#[instrument(skip(state, request))]
pub async fn device_verify_submit_handler(
    State(state): State<Arc<AuthState>>,
    Form(request): Form<VerifySubmitRequest>,
) -> impl IntoResponse {
    let normalized = request.user_code.trim().to_uppercase();

    let mut auth = match state
        .auth_handshake
        .find_device_by_user_code(&normalized)
        .await
    {
        Ok(Some(v)) => v,
        Ok(None) => {
            record_auth_failure("unknown", None, AuthFailure::DeviceInvalidCode);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("invalid_user_code", "Invalid user code")),
            )
                .into_response();
        }
        Err(err) => {
            record_auth_failure(
                "unknown",
                None,
                AuthFailure::from_error(AuthStage::DeviceFlow, &err),
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new("server_error", &err.to_string())),
            )
                .into_response();
        }
    };

    if auth.is_expired() {
        record_auth_failure(&auth.provider_id, None, AuthFailure::DeviceExpired);
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
    match state.auth_handshake.update_device(&auth).await {
        Ok(true) => {}
        Ok(false) => {
            record_auth_failure(&auth.provider_id, None, AuthFailure::DeviceInvalidGrant);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_device_code",
                    "Device code no longer exists",
                )),
            )
                .into_response();
        }
        Err(err) => {
            record_auth_failure(
                &auth.provider_id,
                None,
                AuthFailure::from_error(AuthStage::DeviceFlow, &err),
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new("server_error", &err.to_string())),
            )
                .into_response();
        }
    }

    let device_code = auth.device_code.clone();
    let provider_id = auth.provider_id.clone();

    let provider = match state.providers.get(&provider_id) {
        Some(v) => v,
        None => {
            record_auth_failure(&provider_id, None, AuthFailure::DeviceInvalidClient);
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
            record_auth_success(AuthStage::OidcAuthorization);
            info!(provider = %provider.id, "Device flow continuing via provider authorization");
            // 303 See Other: this handler answers a POSTed form, so the
            // redirect must switch the browser to GET — a 307 would replay
            // the POST against the IdP's GET-only authorize endpoint.
            axum::response::Redirect::to(&url).into_response()
        }
        Err(err) => {
            record_auth_failure(
                &provider.id,
                None,
                AuthFailure::from_error(AuthStage::OidcAuthorization, &err),
            );
            (
                StatusCode::BAD_GATEWAY,
                Json(ErrorResponse::new("authorization_failed", &err.to_string())),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_device_verify_html;
    use super::router;
    use crate::auth::{AuthProviderConfig, AuthProviderKind, AuthProviderTokenEndpointAuthMethod};
    use crate::config::ServerConfig;
    use crate::server::routes::auth::tests::create_test_auth_state;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_oauth2_provider() -> AuthProviderConfig {
        AuthProviderConfig {
            id: "test-idp".to_string(),
            display_name: "Test IdP".to_string(),
            kind: AuthProviderKind::OAuth2,
            dynamic_client_registration: false,
            client_id: "test-client".to_string(),
            client_secret: String::new(),
            token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::NoAuthentication,
            require_dpop: false,
            scopes: vec!["profile".to_string()],
            issuer: Some("https://idp.example.com".to_string()),
            authorization_endpoint: Some("https://idp.example.com/authorize".to_string()),
            token_endpoint: Some("https://idp.example.com/token".to_string()),
            userinfo_endpoint: Some("https://idp.example.com/userinfo".to_string()),
            jwks_uri: None,
            subject_claim: "sub".to_string(),
            username_claim: None,
            email_claim: None,
        }
    }

    /// The verify form is a POST; the redirect to the IdP's GET-only
    /// authorize endpoint must be 303 See Other so the browser switches
    /// to GET instead of replaying the POST (which 307 would).
    #[tokio::test]
    async fn verify_submit_redirects_to_idp_with_303_see_other() {
        // The happy path emits waddle.auth.success; hold the global
        // telemetry guard so guarded export assertions elsewhere don't
        // race this emission.
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let mut server_config = ServerConfig::test_homeserver();
        server_config.auth.providers = vec![test_oauth2_provider()];
        let (auth_state, _actor) = create_test_auth_state(&server_config).await;
        let app = router(auth_state);

        let start_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/device/start")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"provider":"test-idp"}"#))
                    .expect("start request"),
            )
            .await
            .expect("start response");
        assert_eq!(start_response.status(), StatusCode::OK);
        let start_body = start_response
            .into_body()
            .collect()
            .await
            .expect("start body")
            .to_bytes();
        let start_json: serde_json::Value =
            serde_json::from_slice(&start_body).expect("start json");
        let user_code = start_json["user_code"].as_str().expect("user_code");

        let verify_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/device/verify")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("user_code={user_code}")))
                    .expect("verify request"),
            )
            .await
            .expect("verify response");

        assert_eq!(verify_response.status(), StatusCode::SEE_OTHER);
        let location = verify_response
            .headers()
            .get(header::LOCATION)
            .expect("location header")
            .to_str()
            .expect("location utf-8");
        assert!(
            location.starts_with("https://idp.example.com/authorize?"),
            "redirect must target the IdP authorize endpoint, got {location}"
        );
    }

    #[test]
    fn device_verify_html_uses_valid_unescaped_attributes() {
        let html = render_device_verify_html("ABCD-1234");
        assert!(html.contains(r#"action="/api/auth/device/verify""#));
        assert!(html.contains(r#"name="user_code""#));
        assert!(!html.contains(r#"\"/api/auth/device/verify\""#));
        assert!(!html.contains(r#"\"user_code\""#));
    }

    #[test]
    fn device_verify_html_escapes_code_attribute() {
        let html = render_device_verify_html(r#""bad"&<'x'"#);
        assert!(html.contains("value=\"&quot;bad&quot;&amp;&lt;&#39;x&#39;\""));
    }
}
