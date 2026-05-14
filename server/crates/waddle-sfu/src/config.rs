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
        f.debug_tuple("ApiKey").field(&self.0).finish()
    }
}

/// LiveKit API secret. HMAC-SHA256 signing key for join JWTs.
#[derive(Clone)]
pub struct ApiSecret(Vec<u8>);

impl ApiSecret {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn from_text(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
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
