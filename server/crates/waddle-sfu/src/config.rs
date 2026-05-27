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
use zeroize::{Zeroize, ZeroizeOnDrop};

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
///
/// The inner `Vec<u8>` is zeroed on drop via [`ZeroizeOnDrop`] so the
/// raw key material does not linger in freed allocations after the
/// secret goes out of scope.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
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
///
/// The inner `Vec<u8>` is zeroed on drop via [`ZeroizeOnDrop`] so the
/// shared secret cannot be recovered from freed heap memory.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
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
    /// Shared secret LiveKit uses to sign webhook deliveries
    /// (`Authorization` JWT, HS256 over the request body's SHA-256
    /// claim). Defaults to [`Self::api_secret`] for dev/test parity
    /// with LiveKit's stock dev configuration; production deployments
    /// should set `LIVEKIT_WEBHOOK_SECRET` independently so a leaked
    /// API secret does not also forge inbound webhook deliveries.
    pub webhook_secret: ApiSecret,
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
    /// `LIVEKIT_TURN_TTL_SECONDS` (3600),
    /// `LIVEKIT_WEBHOOK_SECRET` (defaults to `LIVEKIT_API_SECRET` for
    /// dev parity; production should set both independently so a
    /// leaked API secret can't forge webhook deliveries).
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

        // Validate the API secret first so the dedicated
        // `WeakApiSecret` error still fires when both secrets are
        // weak and `LIVEKIT_WEBHOOK_SECRET` is unset (it defaults to
        // the API secret in that case, so checking webhook first
        // would surface a misleading `WeakWebhookSecret`).
        let api_secret_value =
            ApiSecret::from_text(&api_secret).map_err(FromEnvError::WeakApiSecret)?;
        let webhook_secret = match std::env::var("LIVEKIT_WEBHOOK_SECRET") {
            Ok(raw) => ApiSecret::from_text(&raw).map_err(FromEnvError::WeakWebhookSecret)?,
            Err(_) => api_secret_value.clone(),
        };

        Ok(Some(Self {
            api_key: ApiKey::new(api_key),
            api_secret: api_secret_value,
            webhook_secret,
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
    #[error("LIVEKIT_WEBHOOK_SECRET is too weak")]
    WeakWebhookSecret(#[source] WeakSecret),
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

    // `from_env` reads process-wide env state; serialise the tests
    // with a single-threaded gate to avoid cross-test contamination.
    fn with_env<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
        use std::sync::Mutex;
        static GATE: Mutex<()> = Mutex::new(());
        let _guard = GATE.lock().unwrap();
        // Save & set
        let preserved: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            match v {
                Some(value) => std::env::set_var(k, value),
                None => std::env::remove_var(k),
            }
        }
        let out = f();
        // Restore
        for (k, v) in preserved {
            match v {
                Some(value) => std::env::set_var(&k, value),
                None => std::env::remove_var(&k),
            }
        }
        out
    }

    const REQUIRED_VARS: &[&str] = &[
        "LIVEKIT_API_KEY",
        "LIVEKIT_API_SECRET",
        "LIVEKIT_WS_URL",
        "LIVEKIT_TURN_HOST",
        "LIVEKIT_TURN_SHARED_SECRET",
    ];

    const OPTIONAL_VARS: &[&str] = &[
        "LIVEKIT_TURN_TLS_PORT",
        "LIVEKIT_TURN_UDP_PORT",
        "LIVEKIT_TOKEN_TTL_SECONDS",
        "LIVEKIT_TURN_TTL_SECONDS",
    ];

    fn all_unset_overrides() -> Vec<(&'static str, Option<&'static str>)> {
        REQUIRED_VARS
            .iter()
            .chain(OPTIONAL_VARS.iter())
            .map(|k| (*k, None))
            .collect()
    }

    #[test]
    fn from_env_all_unset_returns_none() {
        let result = with_env(&all_unset_overrides(), SfuConfig::from_env);
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn from_env_full_required_set_produces_config() {
        let mut overrides = all_unset_overrides();
        overrides.extend([
            ("LIVEKIT_API_KEY", Some("APIxxxxxxxx")),
            (
                "LIVEKIT_API_SECRET",
                Some("super-secret-secret-32-bytes-min"),
            ),
            ("LIVEKIT_WS_URL", Some("wss://livekit.test/")),
            ("LIVEKIT_TURN_HOST", Some("turn.test")),
            ("LIVEKIT_TURN_SHARED_SECRET", Some("turn-shared-secret")),
        ]);
        let cfg = with_env(&overrides, SfuConfig::from_env)
            .expect("ok")
            .expect("some");
        assert_eq!(cfg.turn_tls_port, 443);
        assert_eq!(cfg.turn_udp_port, 3478);
        assert_eq!(cfg.token_ttl, Duration::seconds(3600));
        assert_eq!(cfg.turn_ttl, Duration::seconds(3600));
        assert_eq!(cfg.turn_host.as_str(), "turn.test");
    }

    #[test]
    fn from_env_partial_required_set_returns_missing() {
        let mut overrides = all_unset_overrides();
        overrides.extend([
            ("LIVEKIT_API_KEY", Some("APIxxxxxxxx")),
            (
                "LIVEKIT_API_SECRET",
                Some("super-secret-secret-32-bytes-min"),
            ),
            // Other required vars left unset.
        ]);
        let err = with_env(&overrides, SfuConfig::from_env).expect_err("partial input is err");
        match err {
            FromEnvError::Missing(var) => assert_eq!(var, "LIVEKIT_WS_URL"),
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn from_env_invalid_url_returns_typed_error() {
        let mut overrides = all_unset_overrides();
        overrides.extend([
            ("LIVEKIT_API_KEY", Some("APIxxxxxxxx")),
            (
                "LIVEKIT_API_SECRET",
                Some("super-secret-secret-32-bytes-min"),
            ),
            ("LIVEKIT_WS_URL", Some("not a url at all")),
            ("LIVEKIT_TURN_HOST", Some("turn.test")),
            ("LIVEKIT_TURN_SHARED_SECRET", Some("turn-shared-secret")),
        ]);
        let err = with_env(&overrides, SfuConfig::from_env).expect_err("invalid url is err");
        assert!(matches!(err, FromEnvError::InvalidUrl(_)));
    }

    #[test]
    fn from_env_wrong_scheme_returns_typed_error() {
        let mut overrides = all_unset_overrides();
        overrides.extend([
            ("LIVEKIT_API_KEY", Some("APIxxxxxxxx")),
            (
                "LIVEKIT_API_SECRET",
                Some("super-secret-secret-32-bytes-min"),
            ),
            ("LIVEKIT_WS_URL", Some("https://livekit.test/")),
            ("LIVEKIT_TURN_HOST", Some("turn.test")),
            ("LIVEKIT_TURN_SHARED_SECRET", Some("turn-shared-secret")),
        ]);
        let err = with_env(&overrides, SfuConfig::from_env).expect_err("wrong scheme is err");
        assert!(matches!(err, FromEnvError::InvalidScheme(_)));
    }

    #[test]
    fn from_env_invalid_port_returns_typed_error() {
        let mut overrides = all_unset_overrides();
        overrides.extend([
            ("LIVEKIT_API_KEY", Some("APIxxxxxxxx")),
            (
                "LIVEKIT_API_SECRET",
                Some("super-secret-secret-32-bytes-min"),
            ),
            ("LIVEKIT_WS_URL", Some("wss://livekit.test/")),
            ("LIVEKIT_TURN_HOST", Some("turn.test")),
            ("LIVEKIT_TURN_SHARED_SECRET", Some("turn-shared-secret")),
            ("LIVEKIT_TURN_TLS_PORT", Some("not-a-port")),
        ]);
        let err = with_env(&overrides, SfuConfig::from_env).expect_err("invalid port is err");
        assert!(matches!(
            err,
            FromEnvError::InvalidNumber("LIVEKIT_TURN_TLS_PORT")
        ));
    }

    // `Vec<u8>::zeroize` writes zeros into every byte then resets
    // `len = 0`. Both surface-observable effects together — bytes
    // cleared and the secret reduced to empty — are the signal
    // that the derive on the secret newtypes is wired up, since
    // without it the secret would still report its original
    // contents.
    #[test]
    fn api_secret_zeroize_empties_the_secret() {
        use zeroize::Zeroize;
        let mut secret = ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length");
        assert!(secret.as_bytes().iter().any(|b| *b != 0));
        secret.zeroize();
        assert!(
            secret.as_bytes().is_empty(),
            "ApiSecret::zeroize must reduce the secret to an empty buffer"
        );
    }

    #[test]
    fn turn_shared_secret_zeroize_empties_the_secret() {
        use zeroize::Zeroize;
        let mut secret = TurnSharedSecret::from_text("turn-shared-secret-value");
        assert!(secret.as_bytes().iter().any(|b| *b != 0));
        secret.zeroize();
        assert!(
            secret.as_bytes().is_empty(),
            "TurnSharedSecret::zeroize must reduce the secret to an empty buffer"
        );
    }

    #[test]
    fn from_env_weak_api_secret_returns_typed_error() {
        let mut overrides = all_unset_overrides();
        overrides.extend([
            ("LIVEKIT_API_KEY", Some("APIxxxxxxxx")),
            ("LIVEKIT_API_SECRET", Some("short")),
            ("LIVEKIT_WS_URL", Some("wss://livekit.test/")),
            ("LIVEKIT_TURN_HOST", Some("turn.test")),
            ("LIVEKIT_TURN_SHARED_SECRET", Some("turn-shared-secret")),
        ]);
        let err = with_env(&overrides, SfuConfig::from_env).expect_err("weak secret is err");
        assert!(matches!(err, FromEnvError::WeakApiSecret(_)));
    }
}
