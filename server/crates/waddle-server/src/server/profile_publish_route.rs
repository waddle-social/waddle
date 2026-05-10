//! Test-only HTTP route exposing `ensure_pep_profile_published` so
//! the wire-conformance test suites can exercise the full chain
//! (set + remove + name) without driving an OIDC mock.
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
//! {
//!   "jid": "alice@localhost",
//!   "photo": { "setFromUrl": "https://..." }   // OR { "removeIfOidcOwned": null } OR omit
//!   "name":  { "set": "Alice" }                // OR { "remove": null } OR omit
//! }
//! ```
//!
//! `photo` and `name` are independently optional. Empty object is a
//! no-op. Returns 200 with the typed `ProfilePublishOutcome` JSON on
//! success. Failure status is mapped from the typed
//! [`ProfileSyncError`] kind:
//!
//! - 400 — invalid request (bad JID, malformed `photo.setFromUrl`)
//! - 401 — missing or wrong `X-Waddle-Test-Token`
//! - 403 — JID outside the test seam allowlist
//! - 422 — fetch policy rejected the bytes (scheme/MIME/size/SSRF/magic)
//! - 502 — upstream HTTP error / network / DNS / vCard storage
//! - 500 — database actor unavailable

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
    ensure_pep_profile_published, FetchError, FetchPolicy, NameIntent, PhotoIntent,
    ProfilePublishDeps, ProfileSource, ProfileSyncError,
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
    pub photo: Option<PhotoIntentDto>,
    #[serde(default)]
    pub name: Option<NameIntentDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PhotoIntentDto {
    /// `{ "setFromUrl": "https://..." }` — fetch + publish.
    SetFromUrl(String),
    /// `{ "removeIfOidcOwned": null }` — XEP-0084 §4.3 empty
    /// `<metadata/>` if the user-managed avatar guard allows it.
    RemoveIfOidcOwned,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NameIntentDto {
    /// `{ "set": "Alice" }` — replace/insert FN.
    Set(String),
    /// `{ "remove": null }` — strip `<FN>` / `<fn>`.
    Remove,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePublishResponse {
    pub photo_sha1_hex: Option<String>,
    pub photo_mime: Option<String>,
    pub photo_bytes_len: Option<usize>,
    pub published_avatar_data: bool,
    pub published_avatar_metadata: bool,
    pub published_avatar_removal: bool,
    pub mirrored_vcard_temp: bool,
    pub mirrored_vcard4: bool,
    pub removed_vcard_temp_photo: bool,
    pub removed_vcard_temp_fn: bool,
    pub removed_vcard4_photo: bool,
    pub removed_vcard4_fn: bool,
    pub photo_removal_guarded_by_user_managed: bool,
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

    let photo = match req.photo {
        Some(PhotoIntentDto::SetFromUrl(raw)) => PhotoIntent::SetFromUrl(
            Url::parse(&raw)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("photo.setFromUrl: {e}")))?,
        ),
        Some(PhotoIntentDto::RemoveIfOidcOwned) => PhotoIntent::RemoveIfOidcOwned,
        None => PhotoIntent::Skip,
    };
    let name = match req.name {
        Some(NameIntentDto::Set(s)) => {
            if s.trim().is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "name.set must not be empty/whitespace; use {\"remove\": null} to clear FN"
                        .to_string(),
                ));
            }
            NameIntent::Set(s)
        }
        Some(NameIntentDto::Remove) => NameIntent::Remove,
        None => NameIntent::Skip,
    };

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
        state: Arc::clone(&state.websocket),
        vcard_store: crate::vcard::VCardStore::new(db.into()),
        // Tests serve avatars from wiremock on 127.0.0.1 over plain
        // HTTP. The fetcher's defense in depth requires loopback for
        // any non-https URL; the route is also gated on a
        // per-process auth token + JID allowlist (see
        // [`TestSeamAuth`]).
        fetch_policy: FetchPolicy {
            block_non_global_ips: false,
            allow_http_for_tests: true,
            ..FetchPolicy::default()
        },
    };

    let outcome = ensure_pep_profile_published(&deps, &jid, ProfileSource::Oidc { photo, name })
        .await
        .map_err(|e| {
            let status = profile_sync_error_status(&e);
            warn!(error = %e, status = %status, "test profile-publish helper failed");
            (status, "profile publish chain failed".to_string())
        })?;

    Ok(Json(ProfilePublishResponse {
        photo_sha1_hex: outcome.photo_sha1_hex,
        photo_mime: outcome.photo_mime,
        photo_bytes_len: outcome.photo_bytes_len,
        published_avatar_data: outcome.published_avatar_data,
        published_avatar_metadata: outcome.published_avatar_metadata,
        published_avatar_removal: outcome.published_avatar_removal,
        mirrored_vcard_temp: outcome.mirrored_vcard_temp,
        mirrored_vcard4: outcome.mirrored_vcard4,
        removed_vcard_temp_photo: outcome.removed_vcard_temp_photo,
        removed_vcard_temp_fn: outcome.removed_vcard_temp_fn,
        removed_vcard4_photo: outcome.removed_vcard4_photo,
        removed_vcard4_fn: outcome.removed_vcard4_fn,
        photo_removal_guarded_by_user_managed: outcome.photo_removal_guarded_by_user_managed,
    }))
}

/// Map a typed [`ProfileSyncError`] to an HTTP status. Distinguishes
/// "the OIDC payload is bad / the upstream rejected the bytes"
/// (4xx) from "we couldn't reach upstream / our storage is down"
/// (5xx) so wire tests can assert the failure mode.
fn profile_sync_error_status(error: &ProfileSyncError) -> StatusCode {
    match error {
        ProfileSyncError::Fetch(FetchError::InvalidScheme(_) | FetchError::MissingHost) => {
            StatusCode::BAD_REQUEST
        }
        ProfileSyncError::Fetch(
            FetchError::SsrfBlocked(_)
            | FetchError::MimeRejected(_)
            | FetchError::MagicByteMismatch
            | FetchError::SizeExceeded(_),
        ) => StatusCode::UNPROCESSABLE_ENTITY,
        ProfileSyncError::Fetch(_) => StatusCode::BAD_GATEWAY,
        ProfileSyncError::PubSub(_)
        | ProfileSyncError::VCardTemp(_)
        | ProfileSyncError::VCard4Malformed(_)
        | ProfileSyncError::AvatarSource(_) => StatusCode::BAD_GATEWAY,
    }
}
