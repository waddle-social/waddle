//! Test-only HTTP route exposing `ensure_pep_profile_published` so
//! the wire-conformance test suites can exercise the full chain
//! without driving an OIDC mock.
//!
//! The route is **only registered when `WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true`**.
//! It is unguarded otherwise — production deployments never see it.
//!
//! Request:
//!
//! ```json
//! POST /api/test/profile-publish
//! { "jid": "alice@localhost", "displayName": "Alice", "avatarUrl": "https://..." }
//! ```
//!
//! `displayName` and `avatarUrl` are independently optional. Empty
//! object is a no-op. Returns 200 with the typed
//! `ProfilePublishOutcome` JSON on success, 4xx with a typed error
//! string on failure.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
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

pub fn router(websocket_state: Arc<WebSocketState>) -> Router {
    Router::new()
        .route("/api/test/profile-publish", post(handler))
        .with_state(websocket_state)
}

async fn handler(
    State(state): State<Arc<WebSocketState>>,
    Json(req): Json<ProfilePublishRequest>,
) -> Result<Json<ProfilePublishResponse>, (StatusCode, String)> {
    let jid: BareJid = req
        .jid
        .parse()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid jid: {e}")))?;
    let avatar_url = req
        .avatar_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(Url::parse)
        .transpose()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid avatar_url: {e}")))?;
    let display_name = req.display_name.filter(|s| !s.is_empty());

    let db = state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(crate::db::actor::GetDatabase)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db: {e}")))?;
    let deps = ProfilePublishDeps {
        pubsub_storage: Arc::clone(&state.deps.protocol.pubsub_storage),
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
        (StatusCode::BAD_GATEWAY, e.to_string())
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
