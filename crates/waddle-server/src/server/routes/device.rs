//! OAuth Device Flow Authentication
//!
//! Implements RFC 8628 Device Authorization Grant for CLI clients.
//!
//! Flow:
//! 1. CLI calls POST /v1/auth/device with ATProto handle
//! 2. Server returns device_code, user_code, and verification_uri
//! 3. User visits verification_uri in browser
//! 4. User completes ATProto OAuth in browser
//! 5. CLI polls POST /v1/auth/device/poll until approved
//! 6. Server returns session credentials to CLI

use crate::auth::{did_to_jid, Session};
use crate::server::routes::auth::{AuthState, ErrorResponse};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, instrument, warn};

/// Device authorization request
#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    /// Unique device code (long, for server verification)
    pub device_code: String,
    /// Short user-facing code (e.g., "ABCD-1234")
    pub user_code: String,
    /// ATProto handle being authenticated
    pub handle: String,
    /// When this authorization was created
    pub created_at: DateTime<Utc>,
    /// When this authorization expires
    pub expires_at: DateTime<Utc>,
    /// Current status
    pub status: DeviceAuthStatus,
    /// Session ID once approved
    pub session_id: Option<String>,
    /// OAuth state for linking browser auth to device code
    pub oauth_state: Option<String>,
}

/// Status of a device authorization
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAuthStatus {
    /// Waiting for user to complete browser auth
    Pending,
    /// User has started browser auth but not completed
    InProgress,
    /// User has completed auth - session is ready
    Approved,
    /// User denied the request
    Denied,
    /// Authorization expired
    Expired,
}

impl DeviceAuthorization {
    /// Create a new pending device authorization
    pub fn new(handle: String) -> Self {
        let device_code = generate_device_code();
        let user_code = generate_user_code();
        let now = Utc::now();

        Self {
            device_code,
            user_code,
            handle,
            created_at: now,
            expires_at: now + Duration::minutes(15), // 15 minute expiry
            status: DeviceAuthStatus::Pending,
            session_id: None,
            oauth_state: None,
        }
    }

    /// Check if the authorization has expired
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

/// Generate a secure random device code (32 bytes, hex encoded)
fn generate_device_code() -> String {
    let bytes: [u8; 32] = rand::rng().random();
    hex::encode(bytes)
}

/// Generate a human-readable user code (e.g., "ABCD-1234")
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

/// In-memory store for device authorizations
/// Key: device_code -> DeviceAuthorization
pub type DeviceAuthStore = Arc<dashmap::DashMap<String, DeviceAuthorization>>;

/// Request to start device authorization
#[derive(Debug, Deserialize)]
pub struct DeviceAuthRequest {
    /// ATProto handle (e.g., "user.bsky.social")
    pub handle: String,
}

/// Response for device authorization request
#[derive(Debug, Serialize)]
pub struct DeviceAuthResponse {
    /// Device code for polling (keep secret, don't show to user)
    pub device_code: String,
    /// User code to display (e.g., "ABCD-1234")
    pub user_code: String,
    /// URL where user should go to authorize
    pub verification_uri: String,
    /// URL with code pre-filled (optional convenience)
    pub verification_uri_complete: String,
    /// How often CLI should poll (seconds)
    pub interval: u32,
    /// When this authorization expires (seconds)
    pub expires_in: u32,
}

/// Request to poll device authorization status
#[derive(Debug, Deserialize)]
pub struct DevicePollRequest {
    /// Device code from initial request
    pub device_code: String,
}

/// Response for device poll - authorization pending
#[derive(Debug, Serialize)]
pub struct DevicePollPendingResponse {
    /// Status is always "authorization_pending"
    pub status: String,
    /// How long until expiry (seconds)
    pub expires_in: u32,
}

/// Response for device poll - authorization complete
#[derive(Debug, Serialize)]
pub struct DevicePollCompleteResponse {
    /// Status is "complete"
    pub status: String,
    /// Session ID for the authenticated user
    pub session_id: String,
    /// User's DID
    pub did: String,
    /// User's handle
    pub handle: String,
    /// JID for XMPP connection
    pub jid: String,
    /// XMPP token (same as session_id)
    pub xmpp_token: String,
    /// XMPP server host
    pub xmpp_host: String,
    /// XMPP server port
    pub xmpp_port: u16,
}

/// Query params for verification page
#[derive(Debug, Deserialize)]
pub struct VerifyQuery {
    /// Pre-filled user code (optional)
    pub code: Option<String>,
}

/// Request to submit user code in browser
#[derive(Debug, Deserialize)]
pub struct VerifySubmitRequest {
    /// User code entered by user
    pub user_code: String,
}

/// Response for verify submit - returns OAuth URL
#[derive(Debug, Serialize)]
pub struct VerifySubmitResponse {
    /// URL to redirect user to for ATProto OAuth
    pub authorization_url: String,
}

/// Create the device auth router
pub fn router(auth_state: Arc<AuthState>, device_store: DeviceAuthStore) -> Router {
    Router::new()
        .route("/v1/auth/device", post(device_auth_handler))
        .route("/v1/auth/device/poll", post(device_poll_handler))
        .route("/v1/auth/device/verify", get(verify_page_handler))
        .route("/v1/auth/device/verify", post(verify_submit_handler))
        .route(
            "/v1/auth/device/callback",
            get(device_callback_handler),
        )
        .with_state((auth_state, device_store))
}

/// POST /v1/auth/device
///
/// Start the device authorization flow.
/// Returns device_code for polling and user_code for display.
#[instrument(skip(auth_state, device_store))]
pub async fn device_auth_handler(
    State((auth_state, device_store)): State<(Arc<AuthState>, DeviceAuthStore)>,
    Json(request): Json<DeviceAuthRequest>,
) -> impl IntoResponse {
    info!("Device auth request for handle: {}", request.handle);

    // Validate handle format (basic check)
    if !request.handle.contains('.') {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_handle",
                "Handle must be in format 'user.domain'",
            )),
        )
            .into_response();
    }

    // Create new device authorization
    let auth = DeviceAuthorization::new(request.handle);
    let device_code = auth.device_code.clone();
    let user_code = auth.user_code.clone();
    let expires_in = (auth.expires_at - Utc::now()).num_seconds() as u32;

    // Store it
    device_store.insert(device_code.clone(), auth);

    // Get base URL from environment or use default
    let base_url =
        std::env::var("WADDLE_BASE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    info!("Device authorization created with code: {}", user_code);

    (
        StatusCode::OK,
        Json(DeviceAuthResponse {
            device_code,
            user_code: user_code.clone(),
            verification_uri: format!("{}/v1/auth/device/verify", base_url),
            verification_uri_complete: format!(
                "{}/v1/auth/device/verify?code={}",
                base_url, user_code
            ),
            interval: 5, // Poll every 5 seconds
            expires_in,
        }),
    )
        .into_response()
}

/// POST /v1/auth/device/poll
///
/// Poll for device authorization status.
/// Returns pending, complete, or error.
#[instrument(skip(auth_state, device_store))]
pub async fn device_poll_handler(
    State((auth_state, device_store)): State<(Arc<AuthState>, DeviceAuthStore)>,
    Json(request): Json<DevicePollRequest>,
) -> impl IntoResponse {
    // Look up the device authorization
    let auth = match device_store.get(&request.device_code) {
        Some(auth) => auth.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_device_code",
                    "Device code not found or expired",
                )),
            )
                .into_response();
        }
    };

    // Check if expired
    if auth.is_expired() {
        device_store.remove(&request.device_code);
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new("expired_token", "Device code has expired")),
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
            // Get session info
            let session_id = auth.session_id.clone().unwrap_or_default();

            // Get session details
            match auth_state.session_manager.get_session(&session_id).await {
                Ok(Some(session)) => {
                    // Clean up the device authorization
                    device_store.remove(&request.device_code);

                    // Get XMPP domain from env or default
                    let xmpp_domain = std::env::var("WADDLE_XMPP_DOMAIN")
                        .unwrap_or_else(|_| "localhost".to_string());

                    let jid = did_to_jid(&session.did, &xmpp_domain).unwrap_or_default();

                    info!("Device authorization complete for: {}", session.handle);

                    (
                        StatusCode::OK,
                        Json(DevicePollCompleteResponse {
                            status: "complete".to_string(),
                            session_id: session.id.clone(),
                            did: session.did.clone(),
                            handle: session.handle.clone(),
                            jid,
                            xmpp_token: session.id,
                            xmpp_host: xmpp_domain,
                            xmpp_port: 5222,
                        }),
                    )
                        .into_response()
                }
                _ => {
                    warn!("Session not found for approved device auth");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse::new(
                            "session_error",
                            "Failed to retrieve session",
                        )),
                    )
                        .into_response()
                }
            }
        }
        DeviceAuthStatus::Denied => {
            device_store.remove(&request.device_code);
            (
                StatusCode::FORBIDDEN,
                Json(ErrorResponse::new(
                    "access_denied",
                    "User denied the authorization request",
                )),
            )
                .into_response()
        }
        DeviceAuthStatus::Expired => {
            device_store.remove(&request.device_code);
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("expired_token", "Device code has expired")),
            )
                .into_response()
        }
    }
}

/// GET /v1/auth/device/verify
///
/// Display the verification page where users enter their code.
#[instrument(skip_all)]
pub async fn verify_page_handler(
    State(_state): State<(Arc<AuthState>, DeviceAuthStore)>,
    Query(params): Query<VerifyQuery>,
) -> impl IntoResponse {
    let prefilled_code = params.code.unwrap_or_default();

    // Simple HTML page for code entry
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Waddle - Device Authorization</title>
    <style>
        * {{ box-sizing: border-box; }}
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%);
            min-height: 100vh;
            margin: 0;
            display: flex;
            align-items: center;
            justify-content: center;
            color: #fff;
        }}
        .container {{
            background: rgba(255, 255, 255, 0.05);
            backdrop-filter: blur(10px);
            border-radius: 16px;
            padding: 40px;
            max-width: 400px;
            width: 90%;
            text-align: center;
            border: 1px solid rgba(255, 255, 255, 0.1);
        }}
        .logo {{ font-size: 48px; margin-bottom: 16px; }}
        h1 {{ margin: 0 0 8px; font-size: 24px; font-weight: 600; }}
        .subtitle {{ color: #a0a0a0; margin-bottom: 32px; }}
        .code-input {{
            font-family: monospace;
            font-size: 32px;
            text-align: center;
            letter-spacing: 4px;
            padding: 16px;
            border: 2px solid rgba(255, 255, 255, 0.2);
            border-radius: 8px;
            background: rgba(0, 0, 0, 0.2);
            color: #fff;
            width: 100%;
            text-transform: uppercase;
            margin-bottom: 24px;
        }}
        .code-input:focus {{
            outline: none;
            border-color: #ff6b6b;
        }}
        .code-input::placeholder {{
            color: #666;
            letter-spacing: 2px;
        }}
        .submit-btn {{
            background: #ff6b6b;
            color: #fff;
            border: none;
            padding: 16px 32px;
            font-size: 16px;
            font-weight: 600;
            border-radius: 8px;
            cursor: pointer;
            width: 100%;
            transition: background 0.2s;
        }}
        .submit-btn:hover {{ background: #ff5252; }}
        .submit-btn:disabled {{
            background: #666;
            cursor: not-allowed;
        }}
        .error {{
            background: rgba(255, 107, 107, 0.2);
            border: 1px solid #ff6b6b;
            border-radius: 8px;
            padding: 12px;
            margin-bottom: 24px;
            display: none;
        }}
        .help {{ margin-top: 24px; font-size: 14px; color: #888; }}
    </style>
</head>
<body>
    <div class="container">
        <div class="logo">🐧</div>
        <h1>Connect Your Device</h1>
        <p class="subtitle">Enter the code shown in your CLI</p>

        <div class="error" id="error"></div>

        <form id="verify-form">
            <input
                type="text"
                class="code-input"
                id="user-code"
                name="user_code"
                placeholder="ABCD-1234"
                maxlength="9"
                autocomplete="off"
                value="{prefilled_code}"
                required
            />
            <button type="submit" class="submit-btn" id="submit-btn">
                Continue with Bluesky
            </button>
        </form>

        <p class="help">
            This code expires in 15 minutes.<br>
            You'll sign in with your Bluesky account.
        </p>
    </div>

    <script>
        const form = document.getElementById('verify-form');
        const input = document.getElementById('user-code');
        const error = document.getElementById('error');
        const btn = document.getElementById('submit-btn');

        // Auto-format input (uppercase, add dash)
        input.addEventListener('input', (e) => {{
            let v = e.target.value.toUpperCase().replace(/[^A-Z0-9]/g, '');
            if (v.length > 4) {{
                v = v.slice(0, 4) + '-' + v.slice(4, 8);
            }}
            e.target.value = v;
        }});

        form.addEventListener('submit', async (e) => {{
            e.preventDefault();
            error.style.display = 'none';
            btn.disabled = true;
            btn.textContent = 'Verifying...';

            try {{
                const res = await fetch('/v1/auth/device/verify', {{
                    method: 'POST',
                    headers: {{ 'Content-Type': 'application/json' }},
                    body: JSON.stringify({{ user_code: input.value }})
                }});

                const data = await res.json();

                if (res.ok && data.authorization_url) {{
                    window.location.href = data.authorization_url;
                }} else {{
                    throw new Error(data.message || 'Invalid code');
                }}
            }} catch (err) {{
                error.textContent = err.message;
                error.style.display = 'block';
                btn.disabled = false;
                btn.textContent = 'Continue with Bluesky';
            }}
        }});

        // Focus input on load
        input.focus();
    </script>
</body>
</html>"#
    );

    Html(html)
}

/// POST /v1/auth/device/verify
///
/// Submit user code and start OAuth flow.
/// Returns authorization URL for redirect.
#[instrument(skip(auth_state, device_store))]
pub async fn verify_submit_handler(
    State((auth_state, device_store)): State<(Arc<AuthState>, DeviceAuthStore)>,
    Json(request): Json<VerifySubmitRequest>,
) -> impl IntoResponse {
    let user_code = request.user_code.to_uppercase().replace([' ', '-'], "");

    // Find the device authorization by user code
    let device_code = {
        let mut found_code = None;
        for entry in device_store.iter() {
            let stored_code = entry.value().user_code.replace('-', "");
            if stored_code == user_code {
                found_code = Some(entry.key().clone());
                break;
            }
        }
        found_code
    };

    let device_code = match device_code {
        Some(code) => code,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_code",
                    "Invalid or expired code. Please check and try again.",
                )),
            )
                .into_response();
        }
    };

    // Get the authorization
    let mut auth = match device_store.get(&device_code) {
        Some(auth) => auth.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("invalid_code", "Code not found")),
            )
                .into_response();
        }
    };

    // Check if expired
    if auth.is_expired() {
        device_store.remove(&device_code);
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "expired_code",
                "This code has expired. Please request a new one.",
            )),
        )
            .into_response();
    }

    // Start OAuth flow for the handle with device-specific callback URL
    let device_callback_url = format!("{}/v1/auth/device/callback", auth_state.base_url);
    match auth_state.oauth_client.start_authorization_with_redirect(&auth.handle, Some(&device_callback_url)).await {
        Ok(oauth_request) => {
            // Store the OAuth state to link callback to device code
            auth.status = DeviceAuthStatus::InProgress;
            auth.oauth_state = Some(oauth_request.state.clone());
            device_store.insert(device_code.clone(), auth.clone());

            // Also store in pending auths for the callback to find
            let pending = crate::auth::session::PendingAuthorization::from_authorization_request(
                &oauth_request,
            );

            // Store with a special key that links to device flow
            auth_state.pending_auths.insert(
                oauth_request.state.clone(),
                PendingAuthorizationWithDevice {
                    pending,
                    device_code: Some(device_code),
                }
                .into(),
            );

            info!(
                "OAuth started for device auth, redirecting to: {}",
                oauth_request.authorization_url
            );

            (
                StatusCode::OK,
                Json(VerifySubmitResponse {
                    authorization_url: oauth_request.authorization_url,
                }),
            )
                .into_response()
        }
        Err(err) => {
            warn!("Failed to start OAuth for device auth: {}", err);
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "oauth_failed",
                    &format!("Failed to start authentication: {}", err),
                )),
            )
                .into_response()
        }
    }
}

/// Extended pending authorization that tracks device flow
#[derive(Debug, Clone)]
struct PendingAuthorizationWithDevice {
    pending: crate::auth::session::PendingAuthorization,
    device_code: Option<String>,
}

impl From<PendingAuthorizationWithDevice> for crate::auth::session::PendingAuthorization {
    fn from(val: PendingAuthorizationWithDevice) -> Self {
        val.pending
    }
}

/// GET /v1/auth/device/callback
///
/// OAuth callback for device flow.
/// Completes the authorization and marks device code as approved.
#[instrument(skip(auth_state, device_store))]
pub async fn device_callback_handler(
    State((auth_state, device_store)): State<(Arc<AuthState>, DeviceAuthStore)>,
    Query(query): Query<DeviceCallbackQuery>,
) -> impl IntoResponse {
    info!("Device OAuth callback received with state: {}", query.state);

    // Look up pending authorization
    let pending = match auth_state.pending_auths.remove(&query.state) {
        Some((_, pending)) => {
            if pending.is_expired() {
                warn!("Pending authorization expired");
                return Html(error_page("Authorization expired. Please try again.")).into_response();
            }
            pending
        }
        None => {
            warn!("No pending authorization found");
            return Html(error_page("Invalid authorization state. Please try again."))
                .into_response();
        }
    };

    // Find the device authorization linked to this OAuth state
    let device_code = {
        let mut found = None;
        for entry in device_store.iter() {
            if entry.value().oauth_state.as_ref() == Some(&query.state) {
                found = Some(entry.key().clone());
                break;
            }
        }
        found
    };

    let device_code = match device_code {
        Some(code) => code,
        None => {
            warn!("No device authorization found for OAuth state");
            return Html(error_page(
                "Device authorization not found. Please try again.",
            ))
            .into_response();
        }
    };

    // Exchange code for tokens (with DPoP)
    match auth_state
        .oauth_client
        .exchange_code(&pending.token_endpoint, &query.code, &pending.code_verifier, &pending.dpop_keypair, &pending.redirect_uri)
        .await
    {
        Ok(tokens) => {
            info!("Token exchange successful for device auth");

            // Create session
            let session = Session::from_token_response(
                &pending.did,
                &pending.handle,
                &tokens,
                &pending.token_endpoint,
                &pending.pds_url,
            );

            // Store session
            match auth_state.session_manager.create_session(&session).await {
                Ok(()) => {
                    info!("Session created for device auth: {}", session.id);

                    // Update device authorization to approved
                    if let Some(mut auth) = device_store.get_mut(&device_code) {
                        auth.status = DeviceAuthStatus::Approved;
                        auth.session_id = Some(session.id.clone());
                    }

                    Html(success_page(&pending.handle)).into_response()
                }
                Err(err) => {
                    warn!("Failed to create session: {}", err);
                    Html(error_page("Failed to create session. Please try again.")).into_response()
                }
            }
        }
        Err(err) => {
            warn!("Token exchange failed: {}", err);
            Html(error_page("Authentication failed. Please try again.")).into_response()
        }
    }
}

/// Query params for device callback
#[derive(Debug, Deserialize)]
pub struct DeviceCallbackQuery {
    pub code: String,
    pub state: String,
    #[serde(rename = "iss")]
    pub _iss: Option<String>,
}

/// Generate success page HTML
fn success_page(handle: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Success - Waddle</title>
    <style>
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%);
            min-height: 100vh;
            margin: 0;
            display: flex;
            align-items: center;
            justify-content: center;
            color: #fff;
        }}
        .container {{
            background: rgba(255, 255, 255, 0.05);
            backdrop-filter: blur(10px);
            border-radius: 16px;
            padding: 40px;
            max-width: 400px;
            width: 90%;
            text-align: center;
            border: 1px solid rgba(255, 255, 255, 0.1);
        }}
        .success {{ font-size: 64px; margin-bottom: 16px; }}
        h1 {{ margin: 0 0 8px; font-size: 24px; color: #4ade80; }}
        .handle {{ color: #ff6b6b; font-weight: 600; }}
        .message {{ color: #a0a0a0; margin-top: 16px; }}
    </style>
</head>
<body>
    <div class="container">
        <div class="success">✓</div>
        <h1>You're all set!</h1>
        <p>Signed in as <span class="handle">@{handle}</span></p>
        <p class="message">You can close this window and return to your CLI.</p>
    </div>
</body>
</html>"#
    )
}

/// Generate error page HTML
fn error_page(message: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Error - Waddle</title>
    <style>
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%);
            min-height: 100vh;
            margin: 0;
            display: flex;
            align-items: center;
            justify-content: center;
            color: #fff;
        }}
        .container {{
            background: rgba(255, 255, 255, 0.05);
            backdrop-filter: blur(10px);
            border-radius: 16px;
            padding: 40px;
            max-width: 400px;
            width: 90%;
            text-align: center;
            border: 1px solid rgba(255, 255, 255, 0.1);
        }}
        .error-icon {{ font-size: 64px; margin-bottom: 16px; }}
        h1 {{ margin: 0 0 16px; font-size: 24px; color: #ff6b6b; }}
        .message {{ color: #a0a0a0; }}
    </style>
</head>
<body>
    <div class="container">
        <div class="error-icon">✗</div>
        <h1>Something went wrong</h1>
        <p class="message">{message}</p>
    </div>
</body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_user_code_format() {
        let code = generate_user_code();
        assert_eq!(code.len(), 9); // XXXX-XXXX
        assert!(code.chars().nth(4) == Some('-'));
    }

    #[test]
    fn test_generate_device_code_length() {
        let code = generate_device_code();
        assert_eq!(code.len(), 64); // 32 bytes = 64 hex chars
    }

    #[test]
    fn test_device_authorization_expiry() {
        let auth = DeviceAuthorization::new("test.bsky.social".to_string());
        assert!(!auth.is_expired());
        assert_eq!(auth.status, DeviceAuthStatus::Pending);
    }
}
