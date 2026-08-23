//! LiveKit admin REST client.
//!
//! Talks the Twirp JSON protocol exposed by LiveKit's `RoomService`:
//! `POST /twirp/livekit.RoomService/{ListRooms,ListParticipants,`
//! `RemoveParticipant,UpdateParticipant,DeleteRoom}`.
//! Authentication is an HS256-signed admin JWT. LiveKit's
//! `RoomService` requires `roomCreate` for `DeleteRoom`, room-scoped
//! `roomAdmin` for participant operations, and `roomList` for
//! `ListRooms`; `DeleteRoom` is gated by
//! `pkg/service/roomservice.go`'s `EnsureCreatePermission`. Tokens
//! are minted per operation so those grants do not leak across
//! control-plane calls.
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
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::call::{CallId, Identity, MediaCapabilities, ParticipantSid, RoomSid};
use crate::config::{ApiKey, ApiSecret, WebsocketUrl};
use crate::error::SfuError;
use crate::token::JWT_CLOCK_SKEW;

/// TTL of every admin JWT minted by [`ReqwestLiveKitAdmin`]. Each
/// admin call is a single HTTP round-trip; a 60-second TTL with `nbf`
/// pre-dated by [`crate::token::JWT_CLOCK_SKEW`] absorbs typical NTP
/// skew without keeping a long-lived bearer token in flight. The
/// skew constant is shared with join-token minting (#1140).
const ADMIN_JWT_TTL: Duration = Duration::seconds(60);

/// HTTP timeout for admin requests. Tight because the call sites are
/// fire-and-forget from the teardown hot path: a stuck SFU must not
/// leak runtime tasks indefinitely.
const ADMIN_HTTP_TIMEOUT: StdDuration = StdDuration::from_secs(5);

/// LiveKit control-plane operation reported to [`AdminCallObserver`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminOp {
    DeleteRoom,
    RemoveParticipant,
    UpdateParticipant,
    ListRooms,
    RoomOccupancy,
}

/// Observer for final LiveKit admin failures after idempotent
/// `not_found` responses have been normalized to success.
pub trait AdminCallObserver: Send + Sync + 'static {
    fn admin_call_failed(&self, op: AdminOp);
}

impl AdminCallObserver for () {
    fn admin_call_failed(&self, _op: AdminOp) {}
}

/// LiveKit's own view of who is connected to a room.
///
/// The split is load-bearing (#1445): `waddle` holds the participants
/// whose identity round-trips to a JID we minted, and `foreign` counts
/// everyone else — an egress recorder, a SIP or ingress participant,
/// anything not issued by this server. Ghost reconciliation may only
/// ever reason about `waddle` entries (a foreign participant cannot be
/// a registry ghost we own), but an emptiness decision MUST consider
/// `foreign` too: deleting a room because the only occupant left is
/// the recorder would kill the recording.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomOccupancy {
    /// Connected participants this server minted tokens for, with the
    /// LiveKit participant SID when the listing supplied one. Reconciliation
    /// carries that SID into restored registry entries so SID-fenced teardown
    /// remains decidable after a process restart.
    pub waddle: Vec<(Identity, Option<ParticipantSid>)>,
    /// Connected participants whose identity is not one of ours.
    pub foreign: usize,
}

/// A LiveKit room name classified once at the admin boundary (#1612
/// review round 8): LiveKit may co-host rooms Waddle does not own, so
/// the raw listing string is parsed here and never crosses the public
/// [`LiveKitAdmin`] trait untyped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListedRoomName {
    /// A name Waddle minted — it parses as a [`CallId`].
    Waddle(CallId),
    /// Any other co-hosted room (egress, SIP, third-party); retained
    /// only for diagnostics and never routed into Waddle state.
    Foreign(String),
}

impl ListedRoomName {
    pub fn classify(name: String) -> Self {
        match CallId::new(name.clone()) {
            Ok(call_id) => Self::Waddle(call_id),
            Err(_) => Self::Foreign(name),
        }
    }

    pub fn as_waddle(&self) -> Option<&CallId> {
        match self {
            Self::Waddle(call_id) => Some(call_id),
            Self::Foreign(_) => None,
        }
    }
}

/// An active LiveKit room returned by `RoomService.ListRooms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedRoom {
    pub name: ListedRoomName,
    pub sid: Option<RoomSid>,
    pub num_participants: Option<u64>,
}

impl RoomOccupancy {
    /// True when the only connected participant LiveKit reports is
    /// `departing` (or nobody at all). LiveKit's list can still echo
    /// the participant whose `RemoveParticipant` was just issued, so
    /// that one identity does not count as occupancy — but any other
    /// Waddle participant (typically registered by another replica)
    /// or ANY foreign participant does.
    pub fn is_empty_except(&self, departing: &Identity) -> bool {
        self.foreign == 0
            && self
                .waddle
                .iter()
                .all(|(identity, _participant_sid)| identity == departing)
    }
}

/// Abstract LiveKit admin operations. Public so integration tests in
/// consuming crates (e.g. `waddle-xmpp`'s XEP-0272 Muji suite) can
/// inject a recording mock via [`crate::LiveKitSfu::with_admin`]; the
/// production implementation is [`ReqwestLiveKitAdmin`] and is
/// constructed automatically by [`crate::LiveKitSfu::new`].
pub trait LiveKitAdmin: Send + Sync + 'static {
    /// List every active LiveKit room. This uses the cluster-wide
    /// `roomList` grant rather than a room-scoped `roomAdmin` grant.
    fn list_rooms(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ListedRoom>, SfuError>> + Send + '_>>;

    fn remove_participant<'a>(
        &'a self,
        room: &'a CallId,
        identity: &'a Identity,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>>;

    fn delete_room<'a>(
        &'a self,
        room: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>>;

    /// Replace a live participant's publish/subscribe permission.
    /// LiveKit applies the new permission immediately and
    /// force-unpublishes any track the participant is no longer
    /// allowed to publish, so a mid-call XEP-0045 voice revocation
    /// takes effect without disconnecting the participant. A
    /// `not_found` participant resolves to `Ok(())`: they already
    /// left, so the desired post-condition ("not publishing") holds.
    fn update_participant<'a>(
        &'a self,
        room: &'a CallId,
        identity: &'a Identity,
        capabilities: MediaCapabilities,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>>;

    /// Who LiveKit currently reports as connected to `room` — the
    /// authoritative cross-replica answer to "is anyone actually in
    /// this call right now". Used by the reconciliation backstop to
    /// detect registry ghosts (lost `participant_left` /
    /// `room_finished` webhooks) and by the teardown path to confirm
    /// emptiness before `DeleteRoom` (#1445). A `not_found` room
    /// (LiveKit GC'd it or never saw it) resolves to an empty
    /// occupancy — nobody is connected — not an error.
    fn room_occupancy<'a>(
        &'a self,
        room: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<RoomOccupancy, SfuError>> + Send + 'a>>;
}

/// Production admin client. Holds a long-lived `reqwest::Client` so
/// connection pool + TLS state are reused across teardowns.
#[derive(Clone)]
pub(crate) struct ReqwestLiveKitAdmin {
    http: reqwest::Client,
    base_url: Url,
    api_key: ApiKey,
    api_secret: ApiSecret,
    observer: Arc<dyn AdminCallObserver>,
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
        Self::new_with_observer(base_url, api_key, api_secret, Arc::new(()))
    }

    pub(crate) fn new_with_observer(
        base_url: Url,
        api_key: ApiKey,
        api_secret: ApiSecret,
        observer: Arc<dyn AdminCallObserver>,
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
            observer,
        })
    }

    fn mint_admin_token(&self, room: &CallId) -> Result<String, SfuError> {
        self.mint_token(AdminGrant {
            room: Some(room.as_str().to_string()),
            room_admin: true,
            room_create: false,
            room_list: false,
        })
    }

    fn mint_delete_room_token(&self, room: &CallId) -> Result<String, SfuError> {
        self.mint_token(AdminGrant {
            room: Some(room.as_str().to_string()),
            room_admin: true,
            room_create: true,
            room_list: false,
        })
    }

    fn mint_list_rooms_token(&self) -> Result<String, SfuError> {
        self.mint_token(AdminGrant {
            room: None,
            room_admin: false,
            room_create: false,
            room_list: true,
        })
    }

    fn mint_token(&self, video: AdminGrant) -> Result<String, SfuError> {
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
            nbf: (now - JWT_CLOCK_SKEW).timestamp(),
            exp: (now + ADMIN_JWT_TTL).timestamp(),
            video,
        };
        let key = EncodingKey::from_secret(self.api_secret.as_bytes());
        encode(&Header::new(jsonwebtoken::Algorithm::HS256), &claims, &key)
            .map_err(SfuError::JwtSigning)
    }

    fn observe<T>(&self, op: AdminOp, result: Result<T, SfuError>) -> Result<T, SfuError> {
        if result.is_err() {
            self.observer.admin_call_failed(op);
        }
        result
    }

    async fn post<B: Serialize>(
        &self,
        room: &CallId,
        path: &str,
        body: &B,
    ) -> Result<(), SfuError> {
        let token = self.mint_admin_token(room)?;
        self.post_with_token(token, path, body).await
    }

    async fn post_delete<B: Serialize>(
        &self,
        room: &CallId,
        path: &str,
        body: &B,
    ) -> Result<(), SfuError> {
        let token = self.mint_delete_room_token(room)?;
        self.post_with_token(token, path, body).await
    }

    async fn post_with_token<B: Serialize>(
        &self,
        token: String,
        path: &str,
        body: &B,
    ) -> Result<(), SfuError> {
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
        let body = resp.text().await.unwrap_or_default();
        normalize_empty_response(status, body)
    }

    /// Like [`Self::post`] but deserializes the response body. Returns
    /// `Ok(None)` for the Twirp `not_found` shapes (HTTP 404 or a 4xx
    /// whose envelope `code` is `"not_found"`) so callers can treat a
    /// missing room as "no participants" rather than an error.
    async fn post_returning<B: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        room: &CallId,
        path: &str,
        body: &B,
    ) -> Result<Option<R>, SfuError> {
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
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status.is_success() {
            let parsed = resp.json::<R>().await.map_err(SfuError::AdminRequest)?;
            return Ok(Some(parsed));
        }
        let body = resp.text().await.unwrap_or_default();
        if status.is_client_error() && is_twirp_not_found(&body) {
            return Ok(None);
        }
        Err(SfuError::AdminCallFailed {
            status: status.as_u16(),
            body: truncate(body, 256),
        })
    }
}

impl LiveKitAdmin for ReqwestLiveKitAdmin {
    fn list_rooms(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ListedRoom>, SfuError>> + Send + '_>> {
        Box::pin(async move {
            let result = async {
                let token = self.mint_list_rooms_token()?;
                let url = self
                    .base_url
                    .join("twirp/livekit.RoomService/ListRooms")
                    .map_err(SfuError::AdminUrl)?;
                let response = self
                    .http
                    .post(url)
                    .bearer_auth(token)
                    .json(&ListRoomsRequest::default())
                    .send()
                    .await
                    .map_err(SfuError::AdminRequest)?;
                let status = response.status();
                if status.is_success() {
                    let response = response
                        .json::<ListRoomsResponse>()
                        .await
                        .map_err(SfuError::AdminRequest)?;
                    return Ok(response.rooms);
                }
                let body = response.text().await.unwrap_or_default();
                Err(SfuError::AdminCallFailed {
                    status: status.as_u16(),
                    body: truncate(body, 256),
                })
            }
            .await;
            self.observe(AdminOp::ListRooms, result)
        })
    }

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
            let result = self
                .post(room, "twirp/livekit.RoomService/RemoveParticipant", &body)
                .await;
            self.observe(AdminOp::RemoveParticipant, result)
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
            let result = self
                .post_delete(room, "twirp/livekit.RoomService/DeleteRoom", &body)
                .await;
            self.observe(AdminOp::DeleteRoom, result)
        })
    }

    fn update_participant<'a>(
        &'a self,
        room: &'a CallId,
        identity: &'a Identity,
        capabilities: MediaCapabilities,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        let body = UpdateParticipantRequest {
            room: room.as_str().to_string(),
            identity: identity.as_livekit_identity(),
            permission: ParticipantPermission::from_capabilities(capabilities),
        };
        Box::pin(async move {
            let result = self
                .post(room, "twirp/livekit.RoomService/UpdateParticipant", &body)
                .await;
            self.observe(AdminOp::UpdateParticipant, result)
        })
    }

    fn room_occupancy<'a>(
        &'a self,
        room: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<RoomOccupancy, SfuError>> + Send + 'a>> {
        let body = ListParticipantsRequest {
            room: room.as_str().to_string(),
        };
        Box::pin(async move {
            let result = async {
                let resp: Option<ListParticipantsResponse> = self
                    .post_returning(room, "twirp/livekit.RoomService/ListParticipants", &body)
                    .await?;
                // `None` => room not found on LiveKit => nobody connected.
                let Some(resp) = resp else {
                    return Ok(RoomOccupancy::default());
                };
                // LiveKit identities are the stringified FullJids we minted
                // into the JWT `sub`. Parse each back into a typed
                // [`Identity`]; anything that does not round-trip is a
                // participant we did not issue (egress recorder, SIP,
                // ingress). It can never be a ghost of ours, but it IS
                // occupancy — count it rather than dropping it.
                let mut occupancy = RoomOccupancy::default();
                for participant in resp.participants {
                    match participant.identity.parse::<jid::FullJid>() {
                        Ok(jid) => occupancy
                            .waddle
                            .push((Identity::from_jid(jid), participant.sid)),
                        Err(_) => occupancy.foreign += 1,
                    }
                }
                Ok(occupancy)
            }
            .await;
            self.observe(AdminOp::RoomOccupancy, result)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    room: Option<String>,
    #[serde(rename = "roomAdmin")]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    room_admin: bool,
    /// `DeleteRoom` is gated by `EnsureCreatePermission` in
    /// livekit-server's `pkg/service/roomservice.go`.
    #[serde(rename = "roomCreate")]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    room_create: bool,
    #[serde(rename = "roomList")]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    room_list: bool,
}

/// Twirp body for `livekit.RoomService/ListRooms`. An empty `names`
/// filter requests all active rooms.
#[derive(Default, Serialize)]
struct ListRoomsRequest {
    names: Vec<String>,
}

#[derive(Deserialize)]
struct ListRoomsResponse {
    #[serde(default)]
    rooms: Vec<ListedRoom>,
}

#[derive(Deserialize)]
struct ListedRoomWire {
    name: String,
    sid: Option<RoomSid>,
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    #[serde(rename = "numParticipants")]
    num_participants: Option<u64>,
}

impl From<ListedRoomWire> for ListedRoom {
    fn from(room: ListedRoomWire) -> Self {
        Self {
            name: ListedRoomName::classify(room.name),
            sid: room.sid,
            num_participants: room.num_participants,
        }
    }
}

impl<'de> Deserialize<'de> for ListedRoom {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        ListedRoomWire::deserialize(deserializer).map(Into::into)
    }
}

fn deserialize_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum WireU64 {
        Number(u64),
        String(String),
    }

    match Option::<WireU64>::deserialize(deserializer)? {
        Some(WireU64::Number(value)) => Ok(Some(value)),
        Some(WireU64::String(value)) => value.parse().map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
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

/// Twirp body for `livekit.RoomService/UpdateParticipant`. Only the
/// `permission` field is sent alongside the addressing pair; LiveKit
/// treats absent `metadata`/`name` as "leave unchanged".
#[derive(Serialize)]
struct UpdateParticipantRequest {
    room: String,
    identity: String,
    permission: ParticipantPermission,
}

/// LiveKit `ParticipantPermission` message. Field vocabulary matches
/// the `VideoGrant` camelCase names in [`crate::token`] so the mint
/// grant and the live-update grant stay in the same terms.
#[derive(Serialize)]
struct ParticipantPermission {
    #[serde(rename = "canSubscribe")]
    can_subscribe: bool,
    #[serde(rename = "canPublish")]
    can_publish: bool,
    #[serde(rename = "canPublishData")]
    can_publish_data: bool,
}

impl ParticipantPermission {
    fn from_capabilities(capabilities: MediaCapabilities) -> Self {
        Self {
            can_subscribe: capabilities.can_subscribe,
            can_publish: capabilities.can_publish,
            can_publish_data: capabilities.can_publish_data,
        }
    }
}

/// Twirp body for `livekit.RoomService/ListParticipants`.
#[derive(Serialize)]
struct ListParticipantsRequest {
    room: String,
}

/// Twirp response for `livekit.RoomService/ListParticipants`. LiveKit
/// returns a `participants` array of `ParticipantInfo`; only the
/// `identity` and `sid` are load-bearing for ghost reconciliation. Other
/// fields (`state`, `tracks`, …) are ignored via serde's default unknown-field
/// handling.
#[derive(Deserialize)]
struct ListParticipantsResponse {
    #[serde(default)]
    participants: Vec<ListedParticipant>,
}

#[derive(Deserialize)]
struct ListedParticipant {
    #[serde(default)]
    identity: String,
    #[serde(default)]
    sid: Option<ParticipantSid>,
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

fn normalize_empty_response(status: reqwest::StatusCode, body: String) -> Result<(), SfuError> {
    if status == reqwest::StatusCode::NOT_FOUND
        || (status.is_client_error() && is_twirp_not_found(&body))
    {
        return Ok(());
    }
    Err(SfuError::AdminCallFailed {
        status: status.as_u16(),
        body: truncate(body, 256),
    })
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
    use std::sync::Mutex;

    use jsonwebtoken::{decode, DecodingKey, Validation};
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct DecodedAdminClaims {
        video: serde_json::Value,
    }

    fn decode_grant(token: &str) -> serde_json::Value {
        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_exp = false;
        validation.required_spec_claims.clear();
        decode::<DecodedAdminClaims>(
            token,
            &DecodingKey::from_secret(b"test-secret-test-secret-test-secret"),
            &validation,
        )
        .expect("admin token decodes")
        .claims
        .video
    }

    fn test_admin() -> ReqwestLiveKitAdmin {
        ReqwestLiveKitAdmin::new(
            Url::parse("http://127.0.0.1/").expect("base URL"),
            ApiKey::new("test-key".to_owned()),
            ApiSecret::from_text("test-secret-test-secret-test-secret").expect("API secret"),
        )
        .expect("admin client")
    }

    #[derive(Default)]
    struct RecordingObserver(Mutex<Vec<AdminOp>>);

    impl AdminCallObserver for RecordingObserver {
        fn admin_call_failed(&self, op: AdminOp) {
            self.0.lock().expect("observer lock").push(op);
        }
    }

    fn test_admin_with_observer(
        base_url: Url,
        observer: Arc<dyn AdminCallObserver>,
    ) -> ReqwestLiveKitAdmin {
        ReqwestLiveKitAdmin::new_with_observer(
            base_url,
            ApiKey::new("test-key".to_owned()),
            ApiSecret::from_text("test-secret-test-secret-test-secret").expect("API secret"),
            observer,
        )
        .expect("admin client")
    }

    #[test]
    fn observer_records_final_admin_error() {
        let observer = Arc::new(RecordingObserver::default());
        let admin = test_admin_with_observer(
            Url::parse("http://127.0.0.1/").expect("base URL"),
            observer.clone(),
        );

        let result = normalize_empty_response(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "boom".to_owned(),
        );
        assert!(admin.observe(AdminOp::RemoveParticipant, result).is_err());
        assert_eq!(
            observer.0.lock().expect("observer lock").as_slice(),
            &[AdminOp::RemoveParticipant]
        );
    }

    #[test]
    fn observer_ignores_not_found_normalized_to_success() {
        let observer = Arc::new(RecordingObserver::default());
        let admin = test_admin_with_observer(
            Url::parse("http://127.0.0.1/").expect("base URL"),
            observer.clone(),
        );

        let result = normalize_empty_response(reqwest::StatusCode::NOT_FOUND, String::new());
        assert!(admin.observe(AdminOp::DeleteRoom, result).is_ok());
        assert!(observer.0.lock().expect("observer lock").is_empty());
    }

    #[test]
    fn delete_room_token_carries_create_and_auditing_grants() {
        let admin = test_admin();
        let room = CallId::new("room@muc.example.com".to_owned()).expect("call ID");

        let grant = decode_grant(&admin.mint_delete_room_token(&room).expect("token"));

        assert_eq!(
            grant,
            serde_json::json!({
                "room": "room@muc.example.com",
                "roomAdmin": true,
                "roomCreate": true,
            })
        );
    }

    #[test]
    fn participant_operation_token_omits_create_grant() {
        let admin = test_admin();
        let room = CallId::new("room@muc.example.com".to_owned()).expect("call ID");

        let grant = decode_grant(&admin.mint_admin_token(&room).expect("token"));

        assert_eq!(
            grant,
            serde_json::json!({
                "room": "room@muc.example.com",
                "roomAdmin": true,
            })
        );
    }

    #[test]
    fn list_rooms_token_carries_only_list_grant() {
        let admin = test_admin();

        let grant = decode_grant(&admin.mint_list_rooms_token().expect("token"));

        assert_eq!(grant, serde_json::json!({ "roomList": true }));
    }

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
    fn list_participants_response_parses_livekit_wire_shape() {
        // Lock the LiveKit `ListParticipantsResponse` wire shape: a
        // `participants` array whose elements carry typed `identity` and
        // participant `sid` values (plus fields we ignore). Absent/empty
        // array must parse too.
        let body = r#"{"participants":[
            {"sid":"PA_1","identity":"alice@waddle.social/desktop","state":"ACTIVE"},
            {"sid":"PA_2","identity":"bob@waddle.social/mobile"}
        ]}"#;
        let parsed: ListParticipantsResponse = serde_json::from_str(body).expect("parses");
        let participants: Vec<(String, Option<ParticipantSid>)> = parsed
            .participants
            .into_iter()
            .map(|participant| (participant.identity, participant.sid))
            .collect();
        assert_eq!(
            participants,
            vec![
                (
                    "alice@waddle.social/desktop".to_string(),
                    Some(ParticipantSid::new("PA_1").expect("participant sid")),
                ),
                (
                    "bob@waddle.social/mobile".to_string(),
                    Some(ParticipantSid::new("PA_2").expect("participant sid")),
                ),
            ]
        );

        let empty: ListParticipantsResponse = serde_json::from_str(r#"{}"#).expect("empty parses");
        assert!(empty.participants.is_empty());
    }

    #[test]
    fn list_rooms_response_parses_livekit_wire_shape() {
        let body = r#"{"rooms":[
            {"name":"general@muc.waddle.social","sid":"RM_1","numParticipants":"2"},
            {"name":"empty@muc.waddle.social","numParticipants":0}
        ]}"#;
        let parsed: ListRoomsResponse = serde_json::from_str(body).expect("parses");
        assert_eq!(
            parsed.rooms,
            vec![
                ListedRoom {
                    name: ListedRoomName::classify("general@muc.waddle.social".to_owned()),
                    sid: Some(RoomSid::new("RM_1").expect("room sid")),
                    num_participants: Some(2),
                },
                ListedRoom {
                    name: ListedRoomName::classify("empty@muc.waddle.social".to_owned()),
                    sid: None,
                    num_participants: Some(0),
                },
            ]
        );
    }

    #[test]
    fn list_rooms_request_and_grant_use_cluster_wide_wire_shape() {
        assert_eq!(
            serde_json::to_value(ListRoomsRequest::default()).expect("request serializes"),
            serde_json::json!({ "names": [] })
        );
        assert_eq!(
            serde_json::to_value(AdminGrant {
                room: None,
                room_admin: false,
                room_create: false,
                room_list: true,
            })
            .expect("grant serializes"),
            serde_json::json!({ "roomList": true })
        );
    }

    #[test]
    fn update_participant_request_serializes_livekit_wire_shape() {
        // Pin the Twirp `UpdateParticipant` body: addressing pair plus
        // a camelCase `permission` message, no other fields (absent
        // `metadata`/`name` mean "leave unchanged" on LiveKit's side).
        let body = UpdateParticipantRequest {
            room: "room@muc.example.com".to_string(),
            identity: "bob@example.com/web".to_string(),
            permission: ParticipantPermission::from_capabilities(
                MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted),
            ),
        };
        let json = serde_json::to_value(&body).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({
                "room": "room@muc.example.com",
                "identity": "bob@example.com/web",
                "permission": {
                    "canSubscribe": true,
                    "canPublish": false,
                    "canPublishData": false,
                }
            })
        );
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
