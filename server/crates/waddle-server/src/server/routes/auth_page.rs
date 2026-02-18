//! Web-based authentication page for XMPP client credential retrieval.
//!
//! Provides a user-friendly HTML page at `/auth` that:
//! 1. Shows a login form for Bluesky handle
//! 2. Handles OAuth flow
//! 3. Displays XMPP credentials for use in standard XMPP clients

use crate::auth::did_to_jid;
use axum::{
    body::Body,
    extract::{Query, State},
    http::{Response, StatusCode},
    response::{Html, IntoResponse, Redirect},
    routing::get,
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

use super::auth::AuthState;

/// Create the auth page router
pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route("/auth", get(auth_page_handler))
        .route("/auth/start", get(start_auth_handler))
        .route("/auth/callback", get(auth_callback_handler))
        .with_state(auth_state)
}

/// Query parameters for the auth page
#[derive(Debug, Deserialize)]
pub struct AuthPageQuery {
    /// Session ID (after successful login)
    session_id: Option<String>,
    /// Error message (if login failed)
    error: Option<String>,
}

/// Query parameters for starting auth
#[derive(Debug, Deserialize)]
pub struct StartAuthQuery {
    /// Bluesky handle
    handle: String,
}

/// Query parameters for auth callback
#[derive(Debug, Deserialize)]
pub struct AuthCallbackQuery {
    /// Authorization code (missing if there was an OAuth error)
    code: Option<String>,
    /// State parameter
    state: String,
    /// Issuer (optional)
    #[serde(rename = "iss")]
    _iss: Option<String>,
    /// OAuth error code (present if authorization failed)
    error: Option<String>,
    /// OAuth error description
    error_description: Option<String>,
}

/// GET /auth
///
/// Main auth page - shows login form or credentials based on session state.
pub async fn auth_page_handler(
    State(state): State<Arc<AuthState>>,
    Query(params): Query<AuthPageQuery>,
) -> Response<Body> {
    // If we have a session_id, show credentials
    if let Some(session_id) = params.session_id {
        return show_credentials_page(&state, &session_id).await;
    }

    // If we have an error, show error page
    if let Some(error) = params.error {
        return show_error_page(&error);
    }

    // Otherwise show login form
    show_login_page()
}

/// GET /auth/start?handle=xxx
///
/// Starts the OAuth flow - redirects to Bluesky authorization.
pub async fn start_auth_handler(
    State(state): State<Arc<AuthState>>,
    Query(params): Query<StartAuthQuery>,
) -> Response<Body> {
    info!("Starting auth flow for handle: {}", params.handle);

    // Use our web callback URL
    let redirect_uri = format!("{}/auth/callback", state.base_url);

    match state
        .oauth_client
        .start_authorization_with_redirect(&params.handle, Some(&redirect_uri))
        .await
    {
        Ok(auth_request) => {
            // Store pending authorization
            let pending = crate::auth::session::PendingAuthorization::from_authorization_request(
                &auth_request,
            );
            state
                .pending_auths
                .insert(auth_request.state.clone(), pending);

            // Redirect to authorization URL
            Redirect::temporary(&auth_request.authorization_url).into_response()
        }
        Err(err) => {
            warn!("Failed to start auth for {}: {}", params.handle, err);
            let error_msg = err.to_string();
            let error = urlencoding::encode(&error_msg);
            Redirect::temporary(&format!("/auth?error={}", error)).into_response()
        }
    }
}

/// GET /auth/callback
///
/// Handles OAuth callback - exchanges code for tokens and redirects to /auth with session.
pub async fn auth_callback_handler(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<AuthCallbackQuery>,
) -> Response<Body> {
    info!("Auth callback received with state: {}", query.state);

    // Check if OAuth returned an error
    if let Some(error) = &query.error {
        let error_msg = query.error_description.as_deref().unwrap_or(error);
        warn!("OAuth error: {} - {}", error, error_msg);
        let encoded_error = urlencoding::encode(error_msg);
        return Redirect::temporary(&format!("/auth?error={}", encoded_error)).into_response();
    }

    // Extract the authorization code
    let code = match &query.code {
        Some(code) => code,
        None => {
            warn!("No authorization code in callback");
            return Redirect::temporary("/auth?error=No%20authorization%20code%20received")
                .into_response();
        }
    };

    // Look up pending authorization
    let pending = match state.pending_auths.remove(&query.state) {
        Some((_, pending)) => {
            if pending.is_expired() {
                warn!("Pending authorization expired");
                return Redirect::temporary("/auth?error=Authorization%20expired").into_response();
            }
            pending
        }
        None => {
            warn!("No pending authorization found for state: {}", query.state);
            return Redirect::temporary("/auth?error=Invalid%20state").into_response();
        }
    };

    // Exchange code for tokens
    match state
        .oauth_client
        .exchange_code(
            &pending.token_endpoint,
            code,
            &pending.code_verifier,
            &pending.dpop_keypair,
            &pending.redirect_uri,
        )
        .await
    {
        Ok(tokens) => {
            info!("Token exchange successful for DID: {}", pending.did);

            // Create session
            let session = crate::auth::Session::from_token_response(
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
                    Redirect::temporary(&format!("/auth?session_id={}", session.id)).into_response()
                }
                Err(err) => {
                    warn!("Failed to create session: {}", err);
                    let error_msg = err.to_string();
                    let error = urlencoding::encode(&error_msg);
                    Redirect::temporary(&format!("/auth?error={}", error)).into_response()
                }
            }
        }
        Err(err) => {
            warn!("Token exchange failed: {}", err);
            let error_msg = err.to_string();
            let error = urlencoding::encode(&error_msg);
            Redirect::temporary(&format!("/auth?error={}", error)).into_response()
        }
    }
}

/// Show the login form page
fn show_login_page() -> Response<Body> {
    let html = r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Waddle - XMPP Login</title>
    <style>
        * {
            box-sizing: border-box;
            margin: 0;
            padding: 0;
        }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            color: #e0e0e0;
        }
        .container {
            background: rgba(255, 255, 255, 0.05);
            backdrop-filter: blur(10px);
            border-radius: 16px;
            padding: 40px;
            width: 100%;
            max-width: 420px;
            box-shadow: 0 8px 32px rgba(0, 0, 0, 0.3);
            border: 1px solid rgba(255, 255, 255, 0.1);
        }
        .logo {
            text-align: center;
            margin-bottom: 24px;
        }
        .logo h1 {
            font-size: 2.5rem;
            color: #ff6b6b;
            margin-bottom: 8px;
        }
        .logo p {
            color: #888;
            font-size: 0.95rem;
        }
        .form-group {
            margin-bottom: 20px;
        }
        label {
            display: block;
            margin-bottom: 8px;
            font-weight: 500;
            color: #ccc;
        }
        input {
            width: 100%;
            padding: 14px 16px;
            border: 1px solid rgba(255, 255, 255, 0.1);
            border-radius: 8px;
            font-size: 1rem;
            background: rgba(0, 0, 0, 0.2);
            color: #fff;
            transition: border-color 0.2s, box-shadow 0.2s;
        }
        input:focus {
            outline: none;
            border-color: #ff6b6b;
            box-shadow: 0 0 0 3px rgba(255, 107, 107, 0.2);
        }
        input::placeholder {
            color: #666;
        }
        button {
            width: 100%;
            padding: 14px;
            background: linear-gradient(135deg, #ff6b6b 0%, #ee5a5a 100%);
            color: white;
            border: none;
            border-radius: 8px;
            font-size: 1rem;
            font-weight: 600;
            cursor: pointer;
            transition: transform 0.2s, box-shadow 0.2s;
        }
        button:hover {
            transform: translateY(-1px);
            box-shadow: 0 4px 12px rgba(255, 107, 107, 0.4);
        }
        button:active {
            transform: translateY(0);
        }
        .info {
            margin-top: 24px;
            padding: 16px;
            background: rgba(255, 255, 255, 0.03);
            border-radius: 8px;
            font-size: 0.9rem;
            color: #888;
            line-height: 1.5;
        }
        .info strong {
            color: #ccc;
        }
    </style>
</head>
<body>
    <div class="container">
        <div class="logo">
            <h1>🐧 Waddle</h1>
            <p>Get your XMPP credentials</p>
        </div>

        <form action="/auth/start" method="get">
            <div class="form-group">
                <label for="handle">Bluesky Handle</label>
                <input
                    type="text"
                    id="handle"
                    name="handle"
                    placeholder="yourname.bsky.social"
                    required
                    autocomplete="username"
                    autocapitalize="none"
                    autocorrect="off"
                >
            </div>

            <button type="submit">Login with Bluesky</button>
        </form>

        <div class="info">
            <strong>What is this?</strong><br>
            Login with your Bluesky account to get XMPP credentials for use in standard XMPP clients like Profanity, Gajim, or Conversations.
        </div>
    </div>
</body>
</html>"##;

    (StatusCode::OK, Html(html)).into_response()
}

/// Show the credentials page
async fn show_credentials_page(state: &AuthState, session_id: &str) -> Response<Body> {
    // Validate session and get credentials
    let session = match state.session_manager.validate_session(session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Failed to validate session: {}", err);
            return show_error_page(&format!("Session error: {}", err));
        }
    };

    // Convert DID to JID
    // Use WADDLE_XMPP_DOMAIN env var, or extract domain from base_url
    let xmpp_domain = std::env::var("WADDLE_XMPP_DOMAIN").unwrap_or_else(|_| {
        url::Url::parse(&state.base_url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "localhost".to_string())
    });
    let jid = match did_to_jid(&session.did, &xmpp_domain) {
        Ok(jid) => jid,
        Err(err) => {
            warn!("Failed to convert DID to JID: {}", err);
            return show_error_page(&format!("JID error: {}", err));
        }
    };

    let xmpp_port = std::env::var("WADDLE_XMPP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5222u16);

    let expires_at = session
        .expires_at
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Waddle - Your XMPP Credentials</title>
    <style>
        * {{
            box-sizing: border-box;
            margin: 0;
            padding: 0;
        }}
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            color: #e0e0e0;
            padding: 20px;
        }}
        .container {{
            background: rgba(255, 255, 255, 0.05);
            backdrop-filter: blur(10px);
            border-radius: 16px;
            padding: 40px;
            width: 100%;
            max-width: 520px;
            box-shadow: 0 8px 32px rgba(0, 0, 0, 0.3);
            border: 1px solid rgba(255, 255, 255, 0.1);
        }}
        .header {{
            text-align: center;
            margin-bottom: 32px;
        }}
        .header h1 {{
            font-size: 1.8rem;
            color: #4ade80;
            margin-bottom: 8px;
        }}
        .header p {{
            color: #888;
        }}
        .credentials {{
            background: rgba(0, 0, 0, 0.3);
            border-radius: 12px;
            padding: 24px;
            margin-bottom: 24px;
        }}
        .cred-row {{
            display: flex;
            margin-bottom: 16px;
            align-items: center;
        }}
        .cred-row:last-child {{
            margin-bottom: 0;
        }}
        .cred-label {{
            width: 90px;
            font-weight: 500;
            color: #888;
            flex-shrink: 0;
        }}
        .cred-value {{
            flex: 1;
            font-family: 'SF Mono', Monaco, 'Courier New', monospace;
            background: rgba(255, 255, 255, 0.05);
            padding: 10px 14px;
            border-radius: 6px;
            font-size: 0.9rem;
            word-break: break-all;
            color: #fff;
            position: relative;
        }}
        .cred-value.password {{
            font-size: 0.8rem;
        }}
        .copy-btn {{
            background: rgba(255, 255, 255, 0.1);
            border: none;
            color: #888;
            padding: 6px 12px;
            border-radius: 4px;
            cursor: pointer;
            font-size: 0.8rem;
            margin-left: 8px;
            transition: all 0.2s;
            flex-shrink: 0;
        }}
        .copy-btn:hover {{
            background: rgba(255, 255, 255, 0.2);
            color: #fff;
        }}
        .copy-btn.copied {{
            background: #4ade80;
            color: #000;
        }}
        .section-title {{
            font-size: 0.85rem;
            color: #666;
            text-transform: uppercase;
            letter-spacing: 0.5px;
            margin-bottom: 12px;
        }}
        .instructions {{
            background: rgba(255, 255, 255, 0.03);
            border-radius: 12px;
            padding: 20px;
            margin-bottom: 24px;
        }}
        .instructions h3 {{
            color: #ccc;
            margin-bottom: 12px;
            font-size: 1rem;
        }}
        .instructions code {{
            display: block;
            background: rgba(0, 0, 0, 0.3);
            padding: 12px 16px;
            border-radius: 6px;
            font-family: 'SF Mono', Monaco, 'Courier New', monospace;
            font-size: 0.85rem;
            color: #4ade80;
            margin-top: 8px;
            overflow-x: auto;
        }}
        .expiry {{
            text-align: center;
            color: #666;
            font-size: 0.85rem;
            margin-bottom: 20px;
        }}
        .actions {{
            display: flex;
            gap: 12px;
        }}
        .btn {{
            flex: 1;
            padding: 12px;
            border-radius: 8px;
            font-size: 0.95rem;
            font-weight: 500;
            cursor: pointer;
            transition: all 0.2s;
            text-align: center;
            text-decoration: none;
        }}
        .btn-primary {{
            background: linear-gradient(135deg, #ff6b6b 0%, #ee5a5a 100%);
            color: white;
            border: none;
        }}
        .btn-primary:hover {{
            transform: translateY(-1px);
            box-shadow: 0 4px 12px rgba(255, 107, 107, 0.4);
        }}
        .btn-secondary {{
            background: rgba(255, 255, 255, 0.1);
            color: #ccc;
            border: 1px solid rgba(255, 255, 255, 0.1);
        }}
        .btn-secondary:hover {{
            background: rgba(255, 255, 255, 0.15);
            color: #fff;
        }}
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>✓ Login Successful</h1>
            <p>Welcome, @{handle}</p>
        </div>

        <div class="credentials">
            <div class="section-title">Your XMPP Credentials</div>

            <div class="cred-row">
                <span class="cred-label">JID</span>
                <span class="cred-value" id="jid">{jid}</span>
                <button class="copy-btn" onclick="copyToClipboard('jid', this)">Copy</button>
            </div>

            <div class="cred-row">
                <span class="cred-label">Password</span>
                <span class="cred-value password" id="password">{password}</span>
                <button class="copy-btn" onclick="copyToClipboard('password', this)">Copy</button>
            </div>

            <div class="cred-row">
                <span class="cred-label">Server</span>
                <span class="cred-value" id="server">{server}:{port}</span>
                <button class="copy-btn" onclick="copyToClipboard('server', this)">Copy</button>
            </div>
        </div>

        <p class="expiry">Expires: {expires}</p>

        <div class="instructions">
            <h3>Profanity (CLI)</h3>
            <code>/connect {jid}</code>
        </div>

        <div class="actions">
            <a href="/auth" class="btn btn-secondary">Login as different user</a>
            <button class="btn btn-primary" onclick="copyAll()">Copy All</button>
        </div>
    </div>

    <script>
        function copyToClipboard(id, btn) {{
            const text = document.getElementById(id).textContent;
            navigator.clipboard.writeText(text).then(() => {{
                btn.textContent = 'Copied!';
                btn.classList.add('copied');
                setTimeout(() => {{
                    btn.textContent = 'Copy';
                    btn.classList.remove('copied');
                }}, 2000);
            }});
        }}

        function copyAll() {{
            const jid = document.getElementById('jid').textContent;
            const password = document.getElementById('password').textContent;
            const server = document.getElementById('server').textContent;
            const text = `JID: ${{jid}}\nPassword: ${{password}}\nServer: ${{server}}`;
            navigator.clipboard.writeText(text).then(() => {{
                const btn = event.target;
                btn.textContent = 'Copied!';
                setTimeout(() => {{
                    btn.textContent = 'Copy All';
                }}, 2000);
            }});
        }}
    </script>
</body>
</html>"##,
        handle = html_escape::encode_text(&session.handle),
        jid = html_escape::encode_text(&jid),
        password = html_escape::encode_text(session_id),
        server = html_escape::encode_text(&xmpp_domain),
        port = xmpp_port,
        expires = html_escape::encode_text(&expires_at),
    );

    (StatusCode::OK, Html(html)).into_response()
}

/// Show error page
fn show_error_page(error: &str) -> Response<Body> {
    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Waddle - Error</title>
    <style>
        * {{
            box-sizing: border-box;
            margin: 0;
            padding: 0;
        }}
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            color: #e0e0e0;
        }}
        .container {{
            background: rgba(255, 255, 255, 0.05);
            backdrop-filter: blur(10px);
            border-radius: 16px;
            padding: 40px;
            width: 100%;
            max-width: 420px;
            box-shadow: 0 8px 32px rgba(0, 0, 0, 0.3);
            border: 1px solid rgba(255, 255, 255, 0.1);
            text-align: center;
        }}
        .icon {{
            font-size: 3rem;
            margin-bottom: 16px;
        }}
        h1 {{
            color: #ff6b6b;
            margin-bottom: 16px;
        }}
        .error {{
            background: rgba(255, 107, 107, 0.1);
            border: 1px solid rgba(255, 107, 107, 0.3);
            border-radius: 8px;
            padding: 16px;
            margin-bottom: 24px;
            color: #ff8a8a;
            font-size: 0.95rem;
            word-break: break-word;
        }}
        a {{
            display: inline-block;
            padding: 12px 24px;
            background: linear-gradient(135deg, #ff6b6b 0%, #ee5a5a 100%);
            color: white;
            text-decoration: none;
            border-radius: 8px;
            font-weight: 500;
            transition: transform 0.2s, box-shadow 0.2s;
        }}
        a:hover {{
            transform: translateY(-1px);
            box-shadow: 0 4px 12px rgba(255, 107, 107, 0.4);
        }}
    </style>
</head>
<body>
    <div class="container">
        <div class="icon">⚠️</div>
        <h1>Something went wrong</h1>
        <div class="error">{error}</div>
        <a href="/auth">Try Again</a>
    </div>
</body>
</html>"##,
        error = html_escape::encode_text(error),
    );

    (StatusCode::OK, Html(html)).into_response()
}
