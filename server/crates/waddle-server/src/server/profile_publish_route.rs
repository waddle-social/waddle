//! Test-only HTTP route exposing `ensure_pep_profile_published` so
//! the wire-conformance test suites can exercise the full chain
//! without driving an OIDC mock.
//!
//! Hardening (defense in depth — the route is *intended* to be
//! unreachable in production but every layer here is a backstop in
//! case an operator misconfigures one of them):
//!
//! 1. Mounted only when [`fixed_test_account_enabled`] is true AND a
//!    non-empty `WADDLE_TEST_PROFILE_PUBLISH_TOKEN` env var is set.
//! 2. Every request MUST carry `X-Waddle-Test-Token` matching that
//!    token. Missing/wrong token → 401.
//! 3. Every request's `jid` MUST be one of the configured fixed test
//!    accounts (matching localpart and domain). Anything else → 403.
//! 4. The fetch policy relaxes loopback + HTTPS for wiremock-driven
//!    tests; this is acceptable because the route can only target the
//!    test fixed accounts (which are themselves throwaway).
//!
//! Request:
//!
//! ```json
//! POST /api/test/profile-publish
//! X-Waddle-Test-Token: <token>
//! { "jid": "alice@localhost", "displayName": "Alice", "avatarUrl": "https://..." }
//! ```
//!
//! `displayName` and `avatarUrl` are independently optional. Empty
//! object is a no-op. Returns 200 with the typed
//! `ProfilePublishOutcome` JSON on success, 4xx with a typed error
//! string on failure.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use jid::BareJid;
use serde::{Deserialize, Serialize};
use tracing::warn;
use url::Url;

use crate::profile::{
    ensure_pep_profile_published, FetchPolicy, ProfilePublishDeps, ProfileSource,
};
use crate::server::routes::websocket::WebSocketState;

/// Header name carrying the per-process auth token.
const AUTH_HEADER: &str = "x-waddle-test-token";

/// Authorization parameters for the test seam, captured at startup
/// from environment when the route is mounted.
#[derive(Debug, Clone)]
pub struct TestSeamAuth {
    /// Random per-process token. Requests that don't carry this in
    /// `X-Waddle-Test-Token` are rejected.
    pub token: String,
    /// JIDs whose profile this seam is allowed to mutate. Lookup is by
    /// (localpart, domain). Built from the configured fixed test
    /// accounts at startup.
    pub allowed_jids: Vec<BareJid>,
}

#[derive(Clone)]
struct RouteState {
    websocket: Arc<WebSocketState>,
    auth: Arc<TestSeamAuth>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePublishRequest {
    pub jid: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePublishResponse {
    pub photo_sha1_hex: Option<String>,
    pub photo_mime: Option<String>,
    pub photo_bytes_len: Option<usize>,
    pub published_avatar_data: bool,
    pub published_avatar_metadata: bool,
    pub mirrored_vcard_temp: bool,
    pub mirrored_vcard4: bool,
}

pub fn router(websocket_state: Arc<WebSocketState>, auth: TestSeamAuth) -> Router {
    let state = RouteState {
        websocket: websocket_state,
        auth: Arc::new(auth),
    };
    Router::new()
        .route("/api/test/profile-publish", post(handler))
        .with_state(state)
}

async fn handler(
    State(state): State<RouteState>,
    headers: HeaderMap,
    Json(req): Json<ProfilePublishRequest>,
) -> Result<Json<ProfilePublishResponse>, (StatusCode, String)> {
    let provided_token = headers
        .get(AUTH_HEADER)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if provided_token != state.auth.token || provided_token.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "missing or invalid X-Waddle-Test-Token".to_string(),
        ));
    }

    let jid: BareJid = req
        .jid
        .parse()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid jid: {e}")))?;
    if !state
        .auth
        .allowed_jids
        .iter()
        .any(|allowed| allowed == &jid)
    {
        return Err((
            StatusCode::FORBIDDEN,
            "jid is not in the test seam allowlist".to_string(),
        ));
    }

    let avatar_url = req
        .avatar_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(Url::parse)
        .transpose()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid avatar_url: {e}")))?;
    let display_name = req.display_name.filter(|s| !s.is_empty());

    let db = state
        .websocket
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(crate::db::actor::GetDatabase)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db: {e}")))?;
    let deps = ProfilePublishDeps {
        pubsub_storage: Arc::clone(&state.websocket.deps.protocol.pubsub_storage),
        vcard_store: crate::vcard::VCardStore::new(db.into()),
        // Tests serve avatars from wiremock on 127.0.0.1 over plain
        // HTTP; relax the loopback block AND the HTTPS requirement.
        // Production callers (OIDC bridge) build a
        // FetchPolicy::default() instead.
        fetch_policy: FetchPolicy {
            block_non_global_ips: false,
            allow_http_for_tests: true,
            ..FetchPolicy::default()
        },
    };

    let outcome = ensure_pep_profile_published(
        &deps,
        &jid,
        ProfileSource::Oidc {
            avatar_url,
            display_name,
        },
    )
    .await
    .map_err(|e| {
        warn!(error = %e, "test profile-publish helper failed");
        (
            StatusCode::BAD_GATEWAY,
            "profile publish chain failed".to_string(),
        )
    })?;

    Ok(Json(ProfilePublishResponse {
        photo_sha1_hex: outcome.photo_sha1_hex,
        photo_mime: outcome.photo_mime,
        photo_bytes_len: outcome.photo_bytes_len,
        published_avatar_data: outcome.published_avatar_data,
        published_avatar_metadata: outcome.published_avatar_metadata,
        mirrored_vcard_temp: outcome.mirrored_vcard_temp,
        mirrored_vcard4: outcome.mirrored_vcard4,
    }))
}
