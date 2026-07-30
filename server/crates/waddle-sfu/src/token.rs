//! LiveKit JWT minting.
//!
//! Per LiveKit's access-token spec, a join token is an HS256-signed
//! JWT carrying an `iss` (API key), `sub` (participant identity),
//! `iat`/`nbf`/`exp` lifetime triple, and a `video` grant struct
//! controlling room access and publish/subscribe rights.

use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::call::{CallId, Identity, MediaCapabilities};
use crate::config::{ApiKey, ApiSecret, WebsocketUrl};
use crate::error::SfuError;

/// Backdate `nbf` by this much when minting LiveKit JWTs so a
/// LiveKit pod whose wall clock is slightly ahead of ours does not
/// reject a freshly-minted token with `token not yet valid`. Matches
/// the slack LiveKit's own server SDKs apply. Shared by the join
/// tokens minted here and the admin tokens in [`crate::admin`]
/// (#1140 — join tokens previously set `nbf = now` exactly, causing
/// intermittent not-yet-valid rejections under small clock skew).
pub(crate) const JWT_CLOCK_SKEW: Duration = Duration::seconds(30);

/// JWT identifier (RFC 7519 §4.1.7). A fresh UUID is minted per
/// token so the SFU can track and revoke individual issuances even
/// when the same (call, identity) pair gets multiple tokens.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Jti(String);

impl Jti {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Reconstruct from a decoded JWT `jti` claim. Only for looking up
    /// the server's OWN issuance bookkeeping (e.g. targeted revocation
    /// of a token this server minted, #1444) — never an
    /// authentication input.
    pub fn from_claim(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Jti {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for Jti {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque encoded JWT. Wire boundary value; never inspected by the
/// XMPP layer apart from being placed in a `<token/>` child of the
/// `urn:waddle:transports:livekit:0` transport element.
#[derive(Clone, PartialEq, Eq)]
pub struct Jwt(String);

impl Jwt {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reconstruct from a wire-format token string read off an
    /// incoming Jingle transport element. The JWT is not validated
    /// here; signature verification happens inside LiveKit when the
    /// client connects to the SFU.
    pub fn from_wire(value: String) -> Self {
        Self(value)
    }

    /// The `jti` claim, decoded WITHOUT signature verification. Safe
    /// only for issuance-bookkeeping lookups — identifying which of
    /// the server's own minted tokens a stanza carried so exactly that
    /// issuance can be revoked (#1444). A forged claim can at worst
    /// revoke nothing (unknown jti) or a token of the forger's own
    /// stanza; it never authenticates anything.
    pub fn unverified_jti(&self) -> Option<Jti> {
        use base64::Engine as _;
        let payload = self.0.split('.').nth(1)?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .ok()?;
        let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        claims.get("jti")?.as_str().map(Jti::from_claim)
    }
}

impl std::fmt::Debug for Jwt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jwt")
            .field("len", &self.0.len())
            .field("value", &"<redacted>")
            .finish()
    }
}

/// A `(jti, exp)` pair the SFU records for an issued join token.
/// Used by [`crate::LiveKitSfu`] to bound the revocation registry:
/// the `exp` lets the SFU drop revoked entries once the token
/// would have lapsed anyway, since a token past its `exp` is
/// unusable regardless of whether its jti is still remembered.
#[derive(Debug, Clone)]
pub(crate) struct IssuedJti {
    pub jti: Jti,
    pub exp: DateTime<Utc>,
}

/// Fully-issued join credential. Returned by
/// [`crate::SfuService::issue_join_token`] and used to populate the
/// outbound `urn:waddle:transports:livekit:0` transport.
#[derive(Debug, Clone)]
pub struct JoinToken {
    pub url: WebsocketUrl,
    pub room: CallId,
    pub identity: Identity,
    pub jwt: Jwt,
    pub jti: Jti,
    pub expires_at: DateTime<Utc>,
}

/// LiveKit `video` grant claim. Public so tests can assert the
/// shape; production code receives this via the
/// [`crate::SfuService`] surface and never constructs it directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VideoGrant {
    #[serde(rename = "roomJoin")]
    pub room_join: bool,
    pub room: String,
    #[serde(rename = "canPublish")]
    pub can_publish: bool,
    #[serde(rename = "canSubscribe")]
    pub can_subscribe: bool,
    #[serde(rename = "canPublishData")]
    pub can_publish_data: bool,
}

impl VideoGrant {
    fn from_capabilities(call_id: &CallId, caps: MediaCapabilities) -> Self {
        Self {
            room_join: true,
            room: call_id.as_str().to_string(),
            can_publish: caps.can_publish,
            can_subscribe: caps.can_subscribe,
            can_publish_data: caps.can_publish_data,
        }
    }
}

#[derive(Debug, Serialize)]
struct Claims {
    iss: String,
    sub: String,
    iat: i64,
    nbf: i64,
    exp: i64,
    /// RFC 7519 §4.1.7 token identifier; lets the SFU revocation
    /// surface refer to an individual token without round-tripping
    /// the opaque JWT string.
    jti: String,
    video: VideoGrant,
}

/// Inputs to [`mint_join_token`] kept as one struct to keep the
/// signing function arity small.
pub(crate) struct MintInputs<'a> {
    pub api_key: &'a ApiKey,
    pub api_secret: &'a ApiSecret,
    pub ws_url: &'a WebsocketUrl,
    pub call_id: &'a CallId,
    pub identity: &'a Identity,
    pub capabilities: MediaCapabilities,
    pub ttl: Duration,
}

pub(crate) fn mint_join_token(inputs: MintInputs<'_>) -> Result<JoinToken, SfuError> {
    let now = Utc::now();
    let expires_at = now + inputs.ttl;
    let jti = Jti::new();

    let claims = Claims {
        iss: inputs.api_key.as_str().to_string(),
        sub: inputs.identity.as_livekit_identity(),
        iat: now.timestamp(),
        // Backdated for clock skew (#1140): the token becomes valid
        // slightly "in the past" from our perspective so a LiveKit
        // pod running a few seconds ahead still accepts it. The
        // lifetime end (`exp`) is unchanged.
        nbf: (now - JWT_CLOCK_SKEW).timestamp(),
        exp: expires_at.timestamp(),
        jti: jti.as_str().to_string(),
        video: VideoGrant::from_capabilities(inputs.call_id, inputs.capabilities),
    };

    let key = EncodingKey::from_secret(inputs.api_secret.as_bytes());
    let encoded = encode(&Header::new(jsonwebtoken::Algorithm::HS256), &claims, &key)
        .map_err(SfuError::JwtSigning)?;

    Ok(JoinToken {
        url: inputs.ws_url.clone(),
        room: inputs.call_id.clone(),
        identity: inputs.identity.clone(),
        jwt: Jwt(encoded),
        jti,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{decode, DecodingKey, Validation};
    use serde::Deserialize;

    fn fixture_identity() -> Identity {
        let jid: jid::FullJid = "alice@waddle.social/desktop".parse().expect("valid JID");
        Identity::from_jid(jid)
    }

    fn fixture_inputs<'a>(
        api_key: &'a ApiKey,
        secret: &'a ApiSecret,
        ws_url: &'a WebsocketUrl,
        call_id: &'a CallId,
        identity: &'a Identity,
    ) -> MintInputs<'a> {
        MintInputs {
            api_key,
            api_secret: secret,
            ws_url,
            call_id,
            identity,
            capabilities: MediaCapabilities::direct_call_peer(),
            ttl: Duration::seconds(3600),
        }
    }

    #[derive(Debug, Deserialize)]
    struct DecodedClaims {
        iss: String,
        sub: String,
        iat: i64,
        nbf: i64,
        exp: i64,
        jti: String,
        video: VideoGrant,
    }

    #[test]
    fn mints_token_with_expected_claims() {
        let api_key = ApiKey::new("APIxxxxxxxx");
        let secret = ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length");
        let ws_url = WebsocketUrl::new("wss://livekit.waddle.social".parse().expect("valid URL"))
            .expect("valid ws url");
        let call_id = CallId::new("call-abc-123").expect("valid call id");
        let identity = fixture_identity();

        let token = mint_join_token(fixture_inputs(
            &api_key, &secret, &ws_url, &call_id, &identity,
        ))
        .expect("mint should succeed");

        assert_eq!(token.room, call_id);
        assert_eq!(token.identity, identity);
        assert_eq!(token.url.as_str(), ws_url.as_str());

        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_nbf = true;
        let key = DecodingKey::from_secret(secret.as_bytes());
        let decoded = decode::<DecodedClaims>(token.jwt.as_str(), &key, &validation)
            .expect("token decodes with secret");
        let claims = decoded.claims;

        assert_eq!(claims.iss, api_key.as_str());
        assert_eq!(claims.sub, identity.as_livekit_identity());
        assert!(claims.exp > claims.iat);
        // #1140: nbf is backdated by the shared clock-skew constant,
        // matching the admin-token behaviour.
        assert_eq!(claims.nbf, claims.iat - JWT_CLOCK_SKEW.num_seconds());
        assert_eq!(claims.jti, token.jti.as_str());
        assert!(!claims.jti.is_empty());
        assert!(claims.video.room_join);
        assert_eq!(claims.video.room, call_id.as_str());
        assert!(claims.video.can_publish);
        assert!(claims.video.can_subscribe);
        assert!(claims.video.can_publish_data);
    }

    /// The role → grant mapping must survive all the way into the
    /// signed JWT: a visitor's token carries `canPublish: false` /
    /// `canPublishData: false` so the SFU itself enforces
    /// listen-only, regardless of client behaviour.
    #[test]
    fn visitor_capabilities_mint_listen_only_video_grant() {
        let api_key = ApiKey::new("APIxxxxxxxx");
        let secret = ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length");
        let ws_url = WebsocketUrl::new("wss://livekit.waddle.social".parse().expect("valid URL"))
            .expect("valid ws url");
        let call_id = CallId::new("general@muc.waddle.social").expect("valid call id");
        let identity = fixture_identity();

        let mut inputs = fixture_inputs(&api_key, &secret, &ws_url, &call_id, &identity);
        inputs.capabilities =
            MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted);
        let token = mint_join_token(inputs).expect("mint should succeed");

        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_nbf = true;
        let key = DecodingKey::from_secret(secret.as_bytes());
        let decoded = decode::<DecodedClaims>(token.jwt.as_str(), &key, &validation)
            .expect("token decodes with secret");

        assert!(decoded.claims.video.room_join, "a visitor may join");
        assert!(decoded.claims.video.can_subscribe, "a visitor may listen");
        assert!(!decoded.claims.video.can_publish);
        assert!(!decoded.claims.video.can_publish_data);
    }

    #[test]
    fn join_token_nbf_is_backdated_for_clock_skew() {
        // #1140: a LiveKit pod whose clock runs slightly behind ours
        // must still accept a fresh join token. `nbf` is backdated by
        // the shared skew constant; `iat`/`exp` (the actual lifetime)
        // are unchanged, so the TTL is not silently extended.
        let api_key = ApiKey::new("APIxxxxxxxx");
        let secret = ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length");
        let ws_url = WebsocketUrl::new("wss://livekit.waddle.social".parse().expect("valid URL"))
            .expect("valid ws url");
        let call_id = CallId::new("call-skew").expect("valid call id");
        let identity = fixture_identity();

        let before = Utc::now().timestamp();
        let token = mint_join_token(fixture_inputs(
            &api_key, &secret, &ws_url, &call_id, &identity,
        ))
        .expect("mint should succeed");
        let after = Utc::now().timestamp();

        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_nbf = true;
        validation.leeway = 0;
        let key = DecodingKey::from_secret(secret.as_bytes());
        let decoded = decode::<DecodedClaims>(token.jwt.as_str(), &key, &validation)
            .expect("token with zero leeway decodes — nbf is already in the past");
        let claims = decoded.claims;

        let skew = JWT_CLOCK_SKEW.num_seconds();
        assert_eq!(claims.nbf, claims.iat - skew, "nbf backdated by the skew");
        assert!(claims.iat >= before && claims.iat <= after, "iat stays now");
        assert_eq!(
            claims.exp - claims.iat,
            3600,
            "token lifetime (iat→exp) is unchanged by the backdate"
        );
    }

    #[test]
    fn each_mint_emits_a_unique_jti() {
        let api_key = ApiKey::new("APIxxxxxxxx");
        let secret = ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length");
        let ws_url = WebsocketUrl::new("wss://livekit.waddle.social".parse().expect("valid URL"))
            .expect("valid ws url");
        let call_id = CallId::new("call-abc-123").expect("valid call id");
        let identity = fixture_identity();
        let a = mint_join_token(fixture_inputs(
            &api_key, &secret, &ws_url, &call_id, &identity,
        ))
        .unwrap();
        let b = mint_join_token(fixture_inputs(
            &api_key, &secret, &ws_url, &call_id, &identity,
        ))
        .unwrap();
        assert_ne!(a.jti, b.jti, "every issuance must have a fresh jti");
    }

    #[test]
    fn capabilities_round_trip_into_video_grant() {
        let api_key = ApiKey::new("APIxxxxxxxx");
        let secret = ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length");
        let ws_url = WebsocketUrl::new("wss://livekit.waddle.social".parse().expect("valid URL"))
            .expect("valid ws url");
        let call_id = CallId::new("call-abc-123").expect("valid call id");
        let identity = fixture_identity();

        let token = mint_join_token(MintInputs {
            api_key: &api_key,
            api_secret: &secret,
            ws_url: &ws_url,
            call_id: &call_id,
            identity: &identity,
            capabilities: MediaCapabilities {
                can_publish: false,
                can_subscribe: true,
                can_publish_data: false,
            },
            ttl: Duration::seconds(60),
        })
        .expect("mint should succeed");

        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_nbf = true;
        let key = DecodingKey::from_secret(secret.as_bytes());
        let decoded =
            decode::<DecodedClaims>(token.jwt.as_str(), &key, &validation).expect("decode");

        assert!(!decoded.claims.video.can_publish);
        assert!(decoded.claims.video.can_subscribe);
        assert!(!decoded.claims.video.can_publish_data);
    }

    #[test]
    fn token_rejects_wrong_secret() {
        let api_key = ApiKey::new("APIxxxxxxxx");
        let secret = ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length");
        let ws_url = WebsocketUrl::new("wss://livekit.waddle.social".parse().expect("valid URL"))
            .expect("valid ws url");
        let call_id = CallId::new("call-abc-123").expect("valid call id");
        let identity = fixture_identity();

        let token = mint_join_token(fixture_inputs(
            &api_key, &secret, &ws_url, &call_id, &identity,
        ))
        .expect("mint should succeed");

        let wrong = DecodingKey::from_secret(b"definitely-not-the-right-secret-xxxx");
        let validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        let outcome = decode::<DecodedClaims>(token.jwt.as_str(), &wrong, &validation);
        assert!(outcome.is_err(), "wrong secret must fail decode");
    }
}
