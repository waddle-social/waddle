//! Configuration for the LiveKit SFU bridge.
//!
//! Values are loaded by [`waddle-server`]'s startup code from
//! environment variables sourced from the `livekit-sfu-api-keys`
//! Kubernetes Secret. Secret material is wrapped in newtypes whose
//! [`std::fmt::Debug`] impl redacts the payload so the values never
//! leak into structured logs.

use std::fmt;

use chrono::Duration;
use url::Url;

/// LiveKit API key (the `iss` claim on minted JWTs).
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKey")
            .field("len", &self.0.len())
            .field("value", &"<redacted>")
            .finish()
    }
}

/// LiveKit API secret. HMAC-SHA256 signing key for join JWTs.
#[derive(Clone)]
pub struct ApiSecret(Vec<u8>);

/// Minimum acceptable length of an HMAC-SHA256 signing key. RFC 2104
/// recommends the key length match the digest length (32 bytes for
/// SHA-256); shorter keys silently degrade signing strength and are
/// rejected at construction.
pub const MIN_API_SECRET_BYTES: usize = 32;

impl ApiSecret {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, WeakSecret> {
        if bytes.len() < MIN_API_SECRET_BYTES {
            return Err(WeakSecret { len: bytes.len() });
        }
        Ok(Self(bytes))
    }

    pub fn from_text(value: &str) -> Result<Self, WeakSecret> {
        Self::from_bytes(value.as_bytes().to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Returned when an [`ApiSecret`] is built from input shorter than
/// [`MIN_API_SECRET_BYTES`]. Carries only the rejected length so the
/// raw secret material never reaches a log line.
#[derive(Debug, thiserror::Error)]
#[error("API secret is too short: {len} bytes (minimum: {MIN_API_SECRET_BYTES})")]
pub struct WeakSecret {
    pub len: usize,
}

impl fmt::Debug for ApiSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiSecret")
            .field("len", &self.0.len())
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Client-facing LiveKit websocket URL (must be `wss://...`).
/// Embedded verbatim into the `urn:waddle:transports:livekit:0`
/// transport when the server fills in a Jingle session-initiate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsocketUrl(Url);

impl WebsocketUrl {
    pub fn new(url: Url) -> Result<Self, InvalidWebsocketUrl> {
        match url.scheme() {
            "wss" | "ws" => Ok(Self(url)),
            other => Err(InvalidWebsocketUrl::UnexpectedScheme(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidWebsocketUrl {
    #[error("expected ws:// or wss:// scheme, got {0}")]
    UnexpectedScheme(String),
}

/// Shared secret for TURN time-limited credentials. Same HMAC-SHA1
/// shape as the coturn `static-auth-secret` LiveKit's chart sets.
#[derive(Clone)]
pub struct TurnSharedSecret(Vec<u8>);

impl TurnSharedSecret {
    pub fn from_text(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for TurnSharedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnSharedSecret")
            .field("len", &self.0.len())
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Full SFU bridge configuration. Constructed once at server start.
#[derive(Debug, Clone)]
pub struct SfuConfig {
    pub api_key: ApiKey,
    pub api_secret: ApiSecret,
    pub ws_url: WebsocketUrl,
    pub turn_host: crate::turn::TurnHost,
    pub turn_tls_port: u16,
    pub turn_udp_port: u16,
    pub turn_shared_secret: TurnSharedSecret,
    /// TTL of every minted join token (default: 1 hour).
    pub token_ttl: Duration,
    /// TTL of every minted TURN credential (default: 1 hour).
    pub turn_ttl: Duration,
}

impl SfuConfig {
    /// Attempt to build a config from environment variables. Returns
    /// `Ok(Some(_))` when all required vars are present, `Ok(None)`
    /// when LiveKit is not configured (so the server starts without
    /// A/V calling), and `Err(_)` only when a partial/invalid set is
    /// supplied.
    ///
    /// Required env vars: `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`,
    /// `LIVEKIT_WS_URL`, `LIVEKIT_TURN_HOST`,
    /// `LIVEKIT_TURN_SHARED_SECRET`.
    /// Optional with defaults: `LIVEKIT_TURN_TLS_PORT` (443),
    /// `LIVEKIT_TURN_UDP_PORT` (3478),
    /// `LIVEKIT_TOKEN_TTL_SECONDS` (3600),
    /// `LIVEKIT_TURN_TTL_SECONDS` (3600).
    pub fn from_env() -> Result<Option<Self>, FromEnvError> {
        let api_key = std::env::var("LIVEKIT_API_KEY").ok();
        let api_secret = std::env::var("LIVEKIT_API_SECRET").ok();
        let ws_url = std::env::var("LIVEKIT_WS_URL").ok();
        let turn_host = std::env::var("LIVEKIT_TURN_HOST").ok();
        let turn_shared_secret = std::env::var("LIVEKIT_TURN_SHARED_SECRET").ok();

        let all_unset = api_key.is_none()
            && api_secret.is_none()
            && ws_url.is_none()
            && turn_host.is_none()
            && turn_shared_secret.is_none();
        if all_unset {
            return Ok(None);
        }

        let api_key = api_key.ok_or(FromEnvError::Missing("LIVEKIT_API_KEY"))?;
        let api_secret = api_secret.ok_or(FromEnvError::Missing("LIVEKIT_API_SECRET"))?;
        let ws_url = ws_url.ok_or(FromEnvError::Missing("LIVEKIT_WS_URL"))?;
        let turn_host = turn_host.ok_or(FromEnvError::Missing("LIVEKIT_TURN_HOST"))?;
        let turn_shared_secret =
            turn_shared_secret.ok_or(FromEnvError::Missing("LIVEKIT_TURN_SHARED_SECRET"))?;

        let parsed_url: url::Url = ws_url.parse().map_err(FromEnvError::InvalidUrl)?;
        let ws_url = WebsocketUrl::new(parsed_url).map_err(FromEnvError::InvalidScheme)?;

        let turn_tls_port = parse_port_env("LIVEKIT_TURN_TLS_PORT", 443)?;
        let turn_udp_port = parse_port_env("LIVEKIT_TURN_UDP_PORT", 3478)?;
        let token_ttl_seconds = parse_seconds_env("LIVEKIT_TOKEN_TTL_SECONDS", 3600)?;
        let turn_ttl_seconds = parse_seconds_env("LIVEKIT_TURN_TTL_SECONDS", 3600)?;

        Ok(Some(Self {
            api_key: ApiKey::new(api_key),
            api_secret: ApiSecret::from_text(&api_secret).map_err(FromEnvError::WeakApiSecret)?,
            ws_url,
            turn_host: crate::turn::TurnHost::new(turn_host),
            turn_tls_port,
            turn_udp_port,
            turn_shared_secret: TurnSharedSecret::from_text(&turn_shared_secret),
            token_ttl: Duration::seconds(token_ttl_seconds),
            turn_ttl: Duration::seconds(turn_ttl_seconds),
        }))
    }
}

fn parse_port_env(name: &'static str, default: u16) -> Result<u16, FromEnvError> {
    match std::env::var(name) {
        Ok(value) => value.parse().map_err(|_| FromEnvError::InvalidNumber(name)),
        Err(_) => Ok(default),
    }
}

fn parse_seconds_env(name: &'static str, default: i64) -> Result<i64, FromEnvError> {
    match std::env::var(name) {
        Ok(value) => value.parse().map_err(|_| FromEnvError::InvalidNumber(name)),
        Err(_) => Ok(default),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FromEnvError {
    #[error("missing required env var: {0}")]
    Missing(&'static str),
    #[error("LIVEKIT_WS_URL is not a valid URL")]
    InvalidUrl(#[source] url::ParseError),
    #[error("LIVEKIT_WS_URL has wrong scheme")]
    InvalidScheme(#[source] InvalidWebsocketUrl),
    #[error("env var {0} is not a valid number")]
    InvalidNumber(&'static str),
    #[error("LIVEKIT_API_SECRET is too weak")]
    WeakApiSecret(#[source] WeakSecret),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_secret_below_min_length_is_rejected() {
        let short = "short";
        let err = ApiSecret::from_text(short).expect_err("short secret should be rejected");
        assert_eq!(err.len, short.len());
    }

    #[test]
    fn api_secret_at_min_length_is_accepted() {
        let exact: String = "x".repeat(MIN_API_SECRET_BYTES);
        ApiSecret::from_text(&exact).expect("32-byte secret should be accepted");
    }

    #[test]
    fn api_key_debug_redacts_value() {
        let key = ApiKey::new("APIxxxxxxxxxxxxxxxxxxxxxx");
        let rendered = format!("{key:?}");
        assert!(
            !rendered.contains("APIxxxxxxxxxxxxxxxxxxxxxx"),
            "ApiKey Debug must not leak the key material: rendered={rendered}"
        );
        assert!(rendered.contains("redacted"));
    }
}
