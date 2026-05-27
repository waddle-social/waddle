//! LiveKit admin REST client.
//!
//! Talks the Twirp JSON protocol exposed by LiveKit's `RoomService`:
//! `POST /twirp/livekit.RoomService/{RemoveParticipant,DeleteRoom}`.
//! Authentication is an HS256-signed admin JWT carrying
//! `video.roomAdmin = true` (RemoveParticipant) plus
//! `video.roomCreate = true` (DeleteRoom). Both flags travel together
//! because every admin call sites either evicts a participant *and*
//! best-effort closes the empty room.
//!
//! Idempotency: LiveKit returns Twirp `not_found` (HTTP 404 / 4xx with
//! `not_found` in the body) when the participant or room is already
//! gone. The teardown hot path runs unconditionally so duplicate
//! teardowns must succeed — those `not_found` shapes are mapped to
//! `Ok(())` here so callers never re-invoke retry logic for a
//! steady-state.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::Serialize;
use url::Url;

use crate::call::{CallId, Identity};
use crate::config::{ApiKey, ApiSecret, WebsocketUrl};
use crate::error::SfuError;

/// TTL of every admin JWT minted by [`ReqwestLiveKitAdmin`]. Each
/// admin call is a single HTTP round-trip, so 30 seconds is generous
/// for clock skew without keeping a long-lived bearer token in flight.
const ADMIN_JWT_TTL: Duration = Duration::seconds(30);

/// HTTP timeout for admin requests. Tight because the call sites are
/// fire-and-forget from the teardown hot path: a stuck SFU must not
/// leak runtime tasks indefinitely.
const ADMIN_HTTP_TIMEOUT: StdDuration = StdDuration::from_secs(5);

/// Abstract LiveKit admin operations. Public so integration tests in
/// consuming crates (e.g. `waddle-xmpp`'s XEP-0272 Muji suite) can
/// inject a recording mock via [`crate::LiveKitSfu::with_admin`]; the
/// production implementation is [`ReqwestLiveKitAdmin`] and is
/// constructed automatically by [`crate::LiveKitSfu::new`].
pub trait LiveKitAdmin: Send + Sync + 'static {
    fn remove_participant<'a>(
        &'a self,
        room: &'a CallId,
        identity: &'a Identity,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>>;

    fn delete_room<'a>(
        &'a self,
        room: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>>;
}

/// Production admin client. Holds a long-lived `reqwest::Client` so
/// connection pool + TLS state are reused across teardowns.
#[derive(Debug, Clone)]
pub(crate) struct ReqwestLiveKitAdmin {
    http: reqwest::Client,
    base_url: Url,
    api_key: ApiKey,
    api_secret: ApiSecret,
}

impl ReqwestLiveKitAdmin {
    /// Build a new admin client. `base_url` is the LiveKit admin REST
    /// origin (`https://sfu.example.com/`); typically derived from the
    /// client-facing websocket URL via [`admin_base_url_from_ws`].
    pub(crate) fn new(
        base_url: Url,
        api_key: ApiKey,
        api_secret: ApiSecret,
    ) -> Result<Self, SfuError> {
        let http = reqwest::Client::builder()
            .timeout(ADMIN_HTTP_TIMEOUT)
            .build()
            .map_err(SfuError::AdminHttpInit)?;
        Ok(Self {
            http,
            base_url,
            api_key,
            api_secret,
        })
    }

    fn mint_admin_token(&self, room: &CallId) -> Result<String, SfuError> {
        let now = Utc::now();
        let claims = AdminClaims {
            iss: self.api_key.as_str().to_string(),
            iat: now.timestamp(),
            nbf: now.timestamp(),
            exp: (now + ADMIN_JWT_TTL).timestamp(),
            video: AdminGrant {
                room: room.as_str().to_string(),
                room_admin: true,
                room_create: true,
            },
        };
        let key = EncodingKey::from_secret(self.api_secret.as_bytes());
        encode(&Header::new(jsonwebtoken::Algorithm::HS256), &claims, &key)
            .map_err(SfuError::JwtSigning)
    }

    async fn post(
        &self,
        room: &CallId,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(), SfuError> {
        let token = self.mint_admin_token(room)?;
        let url = self.base_url.join(path).map_err(SfuError::AdminUrl)?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .map_err(SfuError::AdminRequest)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        // LiveKit Twirp returns either HTTP 404 or a 4xx with a JSON
        // body containing `"code":"not_found"` when the participant
        // or room has already been removed. Both shapes are the
        // teardown idempotency contract: the desired post-condition
        // (gone) already holds, so map them to `Ok(())`.
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        if status.is_client_error() && body.contains("not_found") {
            return Ok(());
        }
        Err(SfuError::AdminCallFailed {
            status: status.as_u16(),
            body: truncate(body, 256),
        })
    }
}

impl LiveKitAdmin for ReqwestLiveKitAdmin {
    fn remove_participant<'a>(
        &'a self,
        room: &'a CallId,
        identity: &'a Identity,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        let body = serde_json::json!({
            "room": room.as_str(),
            "identity": identity.as_livekit_identity(),
        });
        Box::pin(async move {
            self.post(room, "twirp/livekit.RoomService/RemoveParticipant", &body)
                .await
        })
    }

    fn delete_room<'a>(
        &'a self,
        room: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        let body = serde_json::json!({ "room": room.as_str() });
        Box::pin(async move {
            self.post(room, "twirp/livekit.RoomService/DeleteRoom", &body)
                .await
        })
    }
}

/// Derive the LiveKit admin REST origin from the client-facing
/// websocket URL. Both endpoints sit on the same Go binary in a
/// stock LiveKit deployment, so swapping the scheme
/// (`wss://` → `https://`, `ws://` → `http://`) and pinning the path
/// at `/` gives the right base.
pub(crate) fn admin_base_url_from_ws(ws_url: &WebsocketUrl) -> Result<Url, SfuError> {
    let mut url: Url = ws_url.as_str().parse().map_err(SfuError::AdminUrl)?;
    let target = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        other => return Err(SfuError::AdminScheme(other.to_string())),
    };
    url.set_scheme(target)
        .map_err(|_| SfuError::AdminScheme(target.to_string()))?;
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

#[derive(Serialize)]
struct AdminClaims {
    iss: String,
    iat: i64,
    nbf: i64,
    exp: i64,
    video: AdminGrant,
}

#[derive(Serialize)]
struct AdminGrant {
    /// Room-scoped grant for least-privilege: the admin token may only
    /// touch the call we're tearing down. LiveKit honours per-room
    /// scoping for both `roomAdmin` and `roomCreate` claims.
    room: String,
    #[serde(rename = "roomAdmin")]
    room_admin: bool,
    #[serde(rename = "roomCreate")]
    room_create: bool,
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    // Truncate on a char boundary so the ellipsis attaches to valid
    // UTF-8. `floor_char_boundary` is unstable, so walk char_indices
    // and stop at the last index that fits within `max`.
    let mut boundary = 0;
    for (idx, _) in s.char_indices() {
        if idx > max {
            break;
        }
        boundary = idx;
    }
    s.truncate(boundary);
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_base_url_swaps_wss_to_https() {
        let ws = WebsocketUrl::new("wss://sfu.waddle.social/".parse().unwrap()).unwrap();
        let admin = admin_base_url_from_ws(&ws).unwrap();
        assert_eq!(admin.as_str(), "https://sfu.waddle.social/");
    }

    #[test]
    fn admin_base_url_swaps_ws_to_http() {
        let ws = WebsocketUrl::new("ws://livekit.local:7880/".parse().unwrap()).unwrap();
        let admin = admin_base_url_from_ws(&ws).unwrap();
        assert_eq!(admin.as_str(), "http://livekit.local:7880/");
    }

    #[test]
    fn admin_base_url_drops_query_and_fragment() {
        let ws = WebsocketUrl::new("wss://sfu.waddle.social/?x=1#y".parse().unwrap()).unwrap();
        let admin = admin_base_url_from_ws(&ws).unwrap();
        assert_eq!(admin.as_str(), "https://sfu.waddle.social/");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let utf8 = "héllo wörld".repeat(50);
        let out = truncate(utf8.clone(), 10);
        assert!(
            out.len() <= 10 + '…'.len_utf8(),
            "truncated string must fit within budget + ellipsis: got {}",
            out.len()
        );
        assert!(out.ends_with('…'));
        // The truncated prefix must remain valid UTF-8 (no split-char
        // panics on round-trip).
        let _: &str = out.as_str();
    }

    #[test]
    fn truncate_passes_short_strings_through_untouched() {
        let s = "short".to_string();
        assert_eq!(truncate(s.clone(), 256), s);
    }
}
