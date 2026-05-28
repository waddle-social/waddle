//! LiveKit admin REST client.
//!
//! Talks the Twirp JSON protocol exposed by LiveKit's `RoomService`:
//! `POST /twirp/livekit.RoomService/{RemoveParticipant,DeleteRoom}`.
//! Authentication is an HS256-signed admin JWT carrying
//! `video.roomAdmin = true`, room-scoped for least-privilege.
//! `RemoveParticipant` and `DeleteRoom` are both gated on `roomAdmin`
//! per LiveKit's docs; `roomCreate` is for `CreateRoom`, which this
//! client never calls, so the grant intentionally omits it.
//!
//! Idempotency: LiveKit returns Twirp `not_found` (HTTP 404 or a 4xx
//! whose envelope's `code` is `"not_found"`) when the participant or
//! room is already gone. The teardown hot path runs unconditionally
//! so duplicate teardowns must succeed — those `not_found` shapes are
//! mapped to `Ok(())` here so callers never re-invoke retry logic for
//! a steady-state. The 4xx body match parses the Twirp error envelope
//! explicitly (not a substring match) so an unrelated error whose
//! human-readable message merely mentions "not found" still surfaces
//! as a failure.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::call::{CallId, Identity};
use crate::config::{ApiKey, ApiSecret, WebsocketUrl};
use crate::error::SfuError;

/// TTL of every admin JWT minted by [`ReqwestLiveKitAdmin`]. Each
/// admin call is a single HTTP round-trip; a 60-second TTL with `nbf`
/// pre-dated by [`ADMIN_JWT_CLOCK_SKEW`] absorbs typical NTP skew
/// without keeping a long-lived bearer token in flight.
const ADMIN_JWT_TTL: Duration = Duration::seconds(60);

/// Backdate `nbf` by this much when minting admin JWTs so a LiveKit
/// pod whose wall clock is slightly ahead does not immediately
/// reject a freshly-minted token with `token not yet valid`. Matches
/// the slack LiveKit's own server SDKs apply.
const ADMIN_JWT_CLOCK_SKEW: Duration = Duration::seconds(30);

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
            // Reject every redirect: an admin-token Bearer carrying
            // root rights over LiveKit rooms must not be replayed
            // against an attacker-controlled origin if the SFU host
            // is compromised or misconfigured to issue 30x to a
            // foreign URL. Mirrors the convention in
            // `server/crates/waddle-xmpp/src/push/sender.rs`.
            .redirect(reqwest::redirect::Policy::none())
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
            // Admin tokens carry no participant identity. Set `sub`
            // to the API key so LiveKit access logs attribute the
            // call to a meaningful subject (instead of dropping the
            // claim entirely), giving the on-call an audit trail
            // when correlating admin actions against API-key
            // rotation events.
            sub: self.api_key.as_str().to_string(),
            iat: now.timestamp(),
            nbf: (now - ADMIN_JWT_CLOCK_SKEW).timestamp(),
            exp: (now + ADMIN_JWT_TTL).timestamp(),
            video: AdminGrant {
                room: room.as_str().to_string(),
                room_admin: true,
            },
        };
        let key = EncodingKey::from_secret(self.api_secret.as_bytes());
        encode(&Header::new(jsonwebtoken::Algorithm::HS256), &claims, &key)
            .map_err(SfuError::JwtSigning)
    }

    async fn post<B: Serialize>(
        &self,
        room: &CallId,
        path: &str,
        body: &B,
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
        // envelope whose `code` is exactly `"not_found"` when the
        // participant or room is already gone. Both shapes are the
        // teardown idempotency contract: the desired post-condition
        // (gone) already holds, so map them to `Ok(())`. The body
        // match parses the envelope explicitly (not a substring test)
        // so an unrelated error whose human-readable `msg` happens to
        // contain "not found" still surfaces as a failure.
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        if status.is_client_error() && is_twirp_not_found(&body) {
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
        let body = RemoveParticipantRequest {
            room: room.as_str().to_string(),
            identity: identity.as_livekit_identity(),
        };
        Box::pin(async move {
            self.post(room, "twirp/livekit.RoomService/RemoveParticipant", &body)
                .await
        })
    }

    fn delete_room<'a>(
        &'a self,
        room: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        let body = DeleteRoomRequest {
            room: room.as_str().to_string(),
        };
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
/// at `/` gives the right base. Userinfo, query, and fragment are
/// stripped: an operator who happened to set
/// `LIVEKIT_WS_URL=wss://user:pass@sfu/` must not leak basic-auth
/// alongside the admin Bearer.
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
    // Best-effort strip of any embedded userinfo. `set_username`
    // and `set_password` only fail when the URL has no host, which
    // a `WebsocketUrl` always does.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Ok(url)
}

#[derive(Serialize)]
struct AdminClaims {
    iss: String,
    sub: String,
    iat: i64,
    nbf: i64,
    exp: i64,
    video: AdminGrant,
}

#[derive(Serialize)]
struct AdminGrant {
    /// Room-scoped grant for least-privilege: the admin token may
    /// only touch the call we're tearing down. LiveKit honours
    /// per-room scoping of `roomAdmin`.
    room: String,
    #[serde(rename = "roomAdmin")]
    room_admin: bool,
}

/// Twirp body for `livekit.RoomService/RemoveParticipant`. Typed so
/// the wire boundary is the only place that turns a typed value into
/// JSON, matching the project's typed-payloads hard rule.
#[derive(Serialize)]
struct RemoveParticipantRequest {
    room: String,
    identity: String,
}

/// Twirp body for `livekit.RoomService/DeleteRoom`.
#[derive(Serialize)]
struct DeleteRoomRequest {
    room: String,
}

/// Twirp error envelope: a JSON object with `code` and `msg` fields
/// returned by LiveKit for any 4xx response. Only `code` is read
/// here; `msg` is human-text and may carry the operator's room name
/// verbatim, so it intentionally never participates in control flow.
#[derive(Deserialize)]
struct TwirpError {
    code: Option<String>,
}

fn is_twirp_not_found(body: &str) -> bool {
    serde_json::from_str::<TwirpError>(body)
        .ok()
        .and_then(|env| env.code)
        .is_some_and(|c| c == "not_found")
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

    #[test]
    fn admin_base_url_strips_userinfo() {
        // Defensive: an operator who set
        // `LIVEKIT_WS_URL=wss://user:pass@sfu/` must not have
        // basic-auth credentials leak alongside the admin Bearer.
        let ws = WebsocketUrl::new("wss://user:pass@sfu.waddle.social/".parse().unwrap()).unwrap();
        let admin = admin_base_url_from_ws(&ws).unwrap();
        assert_eq!(admin.as_str(), "https://sfu.waddle.social/");
        assert_eq!(admin.username(), "");
        assert!(admin.password().is_none());
    }

    #[test]
    fn is_twirp_not_found_matches_canonical_envelope() {
        assert!(is_twirp_not_found(
            r#"{"code":"not_found","msg":"participant not found"}"#
        ));
    }

    #[test]
    fn is_twirp_not_found_does_not_match_other_codes() {
        // Permission failures, validation errors, etc. that mention
        // "not found" in their `msg` must NOT be swallowed as
        // idempotent successes.
        assert!(!is_twirp_not_found(
            r#"{"code":"permission_denied","msg":"identity not found in this room"}"#
        ));
        assert!(!is_twirp_not_found(r#"{"code":"invalid_argument"}"#));
        assert!(!is_twirp_not_found(""));
        assert!(!is_twirp_not_found("not_found"));
    }
}
