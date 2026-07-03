//! Server configuration.

use crate::auth::providers::AuthProviderConfig;
use crate::db::DatabaseDriver;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use std::{fmt, str::FromStr};
use tracing::info;
use waddle_extensions::ExtensionConfig;
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode {
    /// Full server mode with HTTP auth broker + XMPP.
    #[default]
    HomeServer,
    /// Standalone XMPP-focused mode.
    Standalone,
}

impl fmt::Display for ServerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerMode::HomeServer => write!(f, "HomeServer"),
            ServerMode::Standalone => write!(f, "Standalone"),
        }
    }
}

impl ServerMode {
    pub fn auth_broker_allowed(&self) -> bool {
        matches!(self, ServerMode::HomeServer)
    }
}

impl FromStr for ServerMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "standalone" | "xmpp" | "xmpp-only" => ServerMode::Standalone,
            _ => ServerMode::HomeServer,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    pub providers: Vec<AuthProviderConfig>,
}

impl AuthConfig {
    pub fn from_env() -> Result<Self, String> {
        let raw = std::env::var("WADDLE_AUTH_PROVIDERS_JSON").unwrap_or_else(|_| "[]".to_string());
        let trimmed = raw.trim();

        let providers = if trimmed.starts_with('[') {
            serde_json::from_str::<Vec<AuthProviderConfig>>(trimmed)
                .map_err(|e| format!("invalid WADDLE_AUTH_PROVIDERS_JSON array: {}", e))?
        } else {
            #[derive(Deserialize)]
            struct Wrapper {
                providers: Vec<AuthProviderConfig>,
            }
            serde_json::from_str::<Wrapper>(trimmed)
                .map_err(|e| format!("invalid WADDLE_AUTH_PROVIDERS_JSON object: {}", e))?
                .providers
        };

        // Validation is strict and fails startup.
        let registry = crate::auth::ProviderRegistry::new(providers.clone())
            .map_err(|e| format!("invalid provider config: {}", e))?;

        if registry.is_empty() {
            info!("No auth providers configured");
        }

        Ok(Self { providers })
    }
}

/// SpiceDB backend configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiceDbConfig {
    pub endpoint: String,
    pub preshared_key: String,
    pub insecure: bool,
}

impl SpiceDbConfig {
    pub fn from_env() -> Result<Option<Self>, String> {
        let endpoint = std::env::var("WADDLE_SPICEDB_ENDPOINT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let preshared_key = std::env::var("WADDLE_SPICEDB_PRESHARED_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        match (endpoint, preshared_key) {
            (None, None) => Ok(None),
            (Some(_), None) => Err(
                "WADDLE_SPICEDB_PRESHARED_KEY is required when WADDLE_SPICEDB_ENDPOINT is set"
                    .to_string(),
            ),
            (None, Some(_)) => Err(
                "WADDLE_SPICEDB_ENDPOINT is required when WADDLE_SPICEDB_PRESHARED_KEY is set"
                    .to_string(),
            ),
            (Some(endpoint), Some(preshared_key)) => {
                let insecure = std::env::var("WADDLE_SPICEDB_INSECURE")
                    .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                    .unwrap_or(false);

                Ok(Some(Self {
                    endpoint,
                    preshared_key,
                    insecure,
                }))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub mode: ServerMode,
    pub base_url: String,
    pub session_key: String,
    pub auth: AuthConfig,
    /// Runtime extension configuration.
    pub extensions: ExtensionConfig,
    /// Operator controls for server-side link-preview enrichment.
    pub link_preview: LinkPreviewConfig,
    /// RFC 7395 §3.8 WebSocket keepalive knobs (issue #1090), parsed
    /// from `WADDLE_WS_KEEPALIVE_*` by [`ws_keepalive_from_vars`].
    pub ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig,
    /// SpiceDB backend configuration.
    /// Runtime startup requires this to be set.
    pub spicedb: Option<SpiceDbConfig>,
    /// Per-deployment HMAC key used to derive XEP-0421 occupant
    /// identifiers. Loaded from `WADDLE_OCCUPANT_ID_SECRET` and shared
    /// across the WebSocket dependencies and `RoomRegistryActor` so
    /// every stamping site reads the same key. Required at startup;
    /// see [`parse_occupant_id_secret`] for the validation rules.
    pub occupant_id_secret: OccupantIdSecret,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewConfig {
    pub enabled: bool,
    pub allowed_hosts: Vec<LinkPreviewHostPattern>,
    pub blocked_hosts: Vec<LinkPreviewHostPattern>,
    /// Maximum bytes fetched while scanning an HTML document for OpenGraph
    /// metadata. The resolver stops shortly after locating `</head>` — it reads
    /// a bounded window past the head (so streaming-SSR frameworks that emit og
    /// tags into the `<body>` are still captured), then stops; well-formed pages
    /// typically read only the head plus that small window. The cap bounds large
    /// pages (e.g. YouTube emits its og tags ~640 KB deep) and acts as a DoS
    /// limit. Does not affect cached-image fetch limits.
    pub max_html_head_bytes: usize,
    pub max_cached_image_bytes: usize,
    pub max_redirects: usize,
    pub fetch_timeout: Duration,
    pub video_enabled: bool,
}

impl Default for LinkPreviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_hosts: Vec::new(),
            blocked_hosts: Vec::new(),
            max_html_head_bytes: 1024 * 1024,
            max_cached_image_bytes: 2 * 1024 * 1024,
            max_redirects: 3,
            fetch_timeout: Duration::from_millis(1_500),
            video_enabled: true,
        }
    }
}

impl LinkPreviewConfig {
    const MAX_FETCH_TIMEOUT: Duration = Duration::from_secs(60);

    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(std::env::vars())
    }

    pub fn from_vars<I, K, V>(vars: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let vars = vars
            .into_iter()
            .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
            .collect::<std::collections::HashMap<_, _>>();
        let fetch_timeout = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS",
            1_500,
        )?);
        if fetch_timeout > Self::MAX_FETCH_TIMEOUT {
            return Err(format!(
                "WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS must be at most {}ms",
                Self::MAX_FETCH_TIMEOUT.as_millis()
            ));
        }

        Ok(Self {
            enabled: parse_bool_var(&vars, "WADDLE_LINK_PREVIEW_ENABLED", true)?,
            allowed_hosts: parse_host_patterns_var(&vars, "WADDLE_LINK_PREVIEW_ALLOWED_HOSTS")?,
            blocked_hosts: parse_host_patterns_var(&vars, "WADDLE_LINK_PREVIEW_BLOCKED_HOSTS")?,
            max_html_head_bytes: parse_usize_var(
                &vars,
                "WADDLE_LINK_PREVIEW_MAX_HTML_HEAD_BYTES",
                1024 * 1024,
            )?,
            max_cached_image_bytes: parse_usize_var(
                &vars,
                "WADDLE_LINK_PREVIEW_MAX_CACHED_IMAGE_BYTES",
                2 * 1024 * 1024,
            )?,
            max_redirects: parse_usize_var(&vars, "WADDLE_LINK_PREVIEW_MAX_REDIRECTS", 3)?,
            fetch_timeout,
            video_enabled: parse_bool_var(&vars, "WADDLE_LINK_PREVIEW_VIDEO_ENABLED", true)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPreviewHostPattern {
    Exact(String),
    DomainSuffix(String),
}

impl LinkPreviewHostPattern {
    pub fn matches(&self, host: &str) -> bool {
        let host = normalize_host_pattern_value(host);
        match self {
            Self::Exact(pattern) => host == *pattern,
            Self::DomainSuffix(suffix) => {
                host == *suffix
                    || host
                        .strip_suffix(suffix)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
        }
    }
}

impl FromStr for LinkPreviewHostPattern {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("host pattern must not be empty".to_string());
        }
        let suffix = trimmed
            .strip_prefix("*.")
            .or_else(|| trimmed.strip_prefix('.'));
        let (suffix_match, value) = match suffix {
            Some(value) => (true, value),
            None => (false, trimmed),
        };
        let normalized = normalize_host_pattern_value(value);
        if normalized.is_empty()
            || normalized.contains('/')
            || normalized.contains(':')
            || normalized.contains('*')
            || normalized.contains(char::is_whitespace)
        {
            return Err(format!("invalid host pattern '{raw}'"));
        }
        if suffix_match {
            Ok(Self::DomainSuffix(normalized))
        } else {
            Ok(Self::Exact(normalized))
        }
    }
}

fn normalize_host_pattern_value(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Validate the `WADDLE_OCCUPANT_ID_SECRET` env var into a typed secret.
///
/// Pure function so the validation logic is unit-testable without
/// mutating process-global env state. Called by [`ServerConfig::from_env`]
/// with the result of `std::env::var(...).ok().as_deref()`.
fn parse_occupant_id_secret(raw: Option<&str>) -> Result<OccupantIdSecret, String> {
    let value = raw.ok_or_else(|| {
        format!(
            "WADDLE_OCCUPANT_ID_SECRET is required (≥{OCCUPANT_ID_SECRET_MIN_BYTES} bytes; \
             generate with: openssl rand -base64 48)"
        )
    })?;
    OccupantIdSecret::new(value.as_bytes().to_vec()).map_err(|e| {
        format!(
            "WADDLE_OCCUPANT_ID_SECRET invalid: {e} \
             (generate with: openssl rand -base64 48)"
        )
    })
}

const SESSION_KEY_MIN_BYTES: usize = 32;

fn parse_session_key(raw: Option<&str>) -> Result<String, String> {
    let value = raw.filter(|value| !value.is_empty()).ok_or_else(|| {
        "WADDLE_SESSION_KEY is required (generate with: openssl rand -base64 48)".to_string()
    })?;
    if value.len() < SESSION_KEY_MIN_BYTES {
        return Err(format!(
            "WADDLE_SESSION_KEY must be at least {SESSION_KEY_MIN_BYTES} bytes \
             (generate with: openssl rand -base64 48)"
        ));
    }
    Ok(value.to_string())
}

fn parse_bool_var(
    vars: &std::collections::HashMap<String, String>,
    key: &str,
    default: bool,
) -> Result<bool, String> {
    let Some(value) = vars.get(key).map(|value| value.trim().to_ascii_lowercase()) else {
        return Ok(default);
    };
    match value.as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!(
            "{key}='{value}' must be a boolean: true/false, yes/no, on/off, or 1/0"
        )),
    }
}

fn parse_usize_var(
    vars: &std::collections::HashMap<String, String>,
    key: &str,
    default: usize,
) -> Result<usize, String> {
    vars.get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|error| format!("{key}='{value}' must be a positive integer: {error}"))
        })
        .unwrap_or(Ok(default))
}

fn parse_u64_var(
    vars: &std::collections::HashMap<String, String>,
    key: &str,
    default: u64,
) -> Result<u64, String> {
    vars.get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|error| format!("{key}='{value}' must be a positive integer: {error}"))
        })
        .unwrap_or(Ok(default))
}

/// Ceiling for `WADDLE_WS_KEEPALIVE_INTERVAL_SECS`.
///
/// On an idle-but-alive connection the probe's pong counts as activity
/// for the following tick, so the worst-case inter-traffic gap on the
/// stream is `2 × interval`. The Cilium/Envoy gateway in front resets
/// idle streams at its ~300s default; capping the interval at 120s
/// bounds the gap at 240s with a 60s margin. This startup guard
/// replaces the "raise gateway idleTimeout" defense-in-depth from
/// issue #1090's original acceptance criteria — a fat-fingered
/// interval fails fast instead of silently reintroducing the ~304s
/// reset storm.
const WS_KEEPALIVE_MAX_INTERVAL_SECS: u64 = 120;

/// Upper bound for `WADDLE_WS_KEEPALIVE_MISS_LIMIT`; beyond this the
/// dead-peer detection is too slow to beat the XEP-0198 unacked-queue
/// cap on busy rooms.
const WS_KEEPALIVE_MAX_MISS_LIMIT: u64 = 10;

/// Parse + validate the RFC 7395 §3.8 keepalive knobs (issue #1090):
///
/// - `WADDLE_WS_KEEPALIVE_INTERVAL_SECS` — probe/tick interval,
///   default 45, valid range 1..=120 (see
///   [`WS_KEEPALIVE_MAX_INTERVAL_SECS`]).
/// - `WADDLE_WS_KEEPALIVE_MISS_LIMIT` — consecutive unanswered probes
///   before the connection is closed, default 2, valid range 1..=10.
///
/// Out-of-range values are startup errors, never clamped: a config
/// that would defeat the keepalive must fail loudly.
pub fn ws_keepalive_from_vars<I, K, V>(
    vars: I,
) -> Result<waddle_xmpp::protocol::KeepaliveConfig, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let vars = vars
        .into_iter()
        .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
        .collect::<std::collections::HashMap<_, _>>();
    let interval_secs = parse_u64_var(&vars, "WADDLE_WS_KEEPALIVE_INTERVAL_SECS", 45)?;
    if !(1..=WS_KEEPALIVE_MAX_INTERVAL_SECS).contains(&interval_secs) {
        return Err(format!(
            "WADDLE_WS_KEEPALIVE_INTERVAL_SECS='{interval_secs}' must be between 1 and \
             {WS_KEEPALIVE_MAX_INTERVAL_SECS}: the worst-case inter-traffic gap is twice the \
             interval and must stay under the gateway's 300s stream-idle timeout"
        ));
    }
    let miss_limit = parse_u64_var(&vars, "WADDLE_WS_KEEPALIVE_MISS_LIMIT", 2)?;
    if !(1..=WS_KEEPALIVE_MAX_MISS_LIMIT).contains(&miss_limit) {
        return Err(format!(
            "WADDLE_WS_KEEPALIVE_MISS_LIMIT='{miss_limit}' must be between 1 and \
             {WS_KEEPALIVE_MAX_MISS_LIMIT}"
        ));
    }
    Ok(waddle_xmpp::protocol::KeepaliveConfig {
        interval_ms: interval_secs * 1_000,
        miss_limit: u32::try_from(miss_limit)
            .map_err(|_| "WADDLE_WS_KEEPALIVE_MISS_LIMIT out of range".to_string())?,
    })
}

/// Env-reading wrapper around [`ws_keepalive_from_vars`].
pub fn ws_keepalive_from_env() -> Result<waddle_xmpp::protocol::KeepaliveConfig, String> {
    ws_keepalive_from_vars(std::env::vars())
}

fn parse_host_patterns_var(
    vars: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<Vec<LinkPreviewHostPattern>, String> {
    let Some(raw) = vars
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry
                .parse::<LinkPreviewHostPattern>()
                .map_err(|error| format!("{key}: {error}"))
        })
        .collect()
}

#[cfg(test)]
const TEST_OCCUPANT_ID_SECRET: &str = "test-occupant-id-secret-32-bytes-long";

#[cfg(test)]
fn test_occupant_id_secret() -> OccupantIdSecret {
    OccupantIdSecret::new(TEST_OCCUPANT_ID_SECRET.as_bytes().to_vec())
        .expect("test secret meets length floor")
}

// `Default` is gated to `#[cfg(test)]`. Production startup MUST go
// through `ServerConfig::from_env`, which enforces the deployment-keyed
// `WADDLE_SESSION_KEY` and `WADDLE_OCCUPANT_ID_SECRET`; a non-test
// `Default` impl could be silently used (e.g. via `..Default::default()`
// in scaffolding) and reintroduce the cross-deployment linkability that
// #283 closes.
#[cfg(test)]
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            mode: ServerMode::default(),
            base_url: "http://localhost:3000".to_string(),
            session_key: "test-session-key-32-bytes-minimum".to_string(),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            link_preview: LinkPreviewConfig::default(),
            ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig::default(),
            spicedb: None,
            occupant_id_secret: test_occupant_id_secret(),
        }
    }
}

impl ServerConfig {
    pub fn from_env() -> Result<Self, String> {
        let mode_str = std::env::var("WADDLE_MODE").unwrap_or_else(|_| "homeserver".to_string());
        let mode = mode_str.parse().unwrap_or_default();

        let base_url = std::env::var("WADDLE_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());

        let session_key = parse_session_key(std::env::var("WADDLE_SESSION_KEY").ok().as_deref())?;
        let auth = AuthConfig::from_env()?;

        let extensions =
            ExtensionConfig::from_env().map_err(|e| format!("invalid extension config: {e}"))?;
        let link_preview = LinkPreviewConfig::from_env()?;
        let ws_keepalive = ws_keepalive_from_env()?;
        let spicedb = SpiceDbConfig::from_env()?;

        let occupant_id_secret =
            parse_occupant_id_secret(std::env::var("WADDLE_OCCUPANT_ID_SECRET").ok().as_deref())?;

        Ok(Self {
            mode,
            base_url,
            session_key,
            auth,
            extensions,
            link_preview,
            ws_keepalive,
            spicedb,
            occupant_id_secret,
        })
    }

    pub fn auth_enabled(&self) -> bool {
        self.mode.auth_broker_allowed() && !self.auth.providers.is_empty()
    }

    pub fn log_config(&self) {
        info!("Running in {} mode", self.mode);
        info!("Base URL: {}", self.base_url);
        info!("Auth providers configured: {}", self.auth.providers.len());
        info!(
            "HTTP auth broker: {}",
            if self.auth_enabled() {
                "enabled"
            } else {
                "disabled"
            }
        );
    }

    #[cfg(test)]
    pub fn test_homeserver() -> Self {
        Self {
            mode: ServerMode::HomeServer,
            base_url: "http://localhost:3000".to_string(),
            session_key: "test-key-32-bytes-long-for-aes!".to_string(),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            link_preview: LinkPreviewConfig::default(),
            ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig::default(),
            spicedb: None,
            occupant_id_secret: test_occupant_id_secret(),
        }
    }

    #[cfg(test)]
    pub fn test_standalone() -> Self {
        Self {
            mode: ServerMode::Standalone,
            base_url: "http://localhost:3000".to_string(),
            session_key: "test-key-32-bytes-long-for-aes!".to_string(),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            link_preview: LinkPreviewConfig::default(),
            ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig::default(),
            spicedb: None,
            occupant_id_secret: test_occupant_id_secret(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_keepalive_defaults_are_45s_2_misses() {
        let config = ws_keepalive_from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(config.interval_ms, 45_000);
        assert_eq!(config.miss_limit, 2);
    }

    #[test]
    fn ws_keepalive_parses_operator_overrides() {
        let config = ws_keepalive_from_vars([
            ("WADDLE_WS_KEEPALIVE_INTERVAL_SECS", "60"),
            ("WADDLE_WS_KEEPALIVE_MISS_LIMIT", "3"),
        ])
        .unwrap();
        assert_eq!(config.interval_ms, 60_000);
        assert_eq!(config.miss_limit, 3);
    }

    #[test]
    fn ws_keepalive_rejects_intervals_that_defeat_the_gateway_timeout() {
        // 2×interval must stay under the gateway's 300s stream-idle
        // timeout; anything above the 120s ceiling fails startup
        // instead of silently reintroducing the ~304s reset storm.
        for bad in ["0", "121", "300"] {
            let err =
                ws_keepalive_from_vars([("WADDLE_WS_KEEPALIVE_INTERVAL_SECS", bad)]).unwrap_err();
            assert!(
                err.contains("WADDLE_WS_KEEPALIVE_INTERVAL_SECS"),
                "error must name the env var; got: {err}"
            );
            assert!(
                err.contains("300s"),
                "error must explain the gateway constraint; got: {err}"
            );
        }
    }

    #[test]
    fn ws_keepalive_rejects_out_of_range_miss_limits() {
        for bad in ["0", "11"] {
            let err =
                ws_keepalive_from_vars([("WADDLE_WS_KEEPALIVE_MISS_LIMIT", bad)]).unwrap_err();
            assert!(
                err.contains("WADDLE_WS_KEEPALIVE_MISS_LIMIT"),
                "error must name the env var; got: {err}"
            );
        }
    }

    #[test]
    fn ws_keepalive_rejects_non_numeric_values() {
        let err =
            ws_keepalive_from_vars([("WADDLE_WS_KEEPALIVE_INTERVAL_SECS", "45s")]).unwrap_err();
        assert!(err.contains("must be a positive integer"));
    }

    #[test]
    fn parse_session_key_rejects_unset() {
        let err = parse_session_key(None).unwrap_err();
        assert!(
            err.contains("WADDLE_SESSION_KEY is required"),
            "error must name the env var; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_session_key_rejects_empty() {
        let err = parse_session_key(Some("")).unwrap_err();
        assert!(
            err.contains("WADDLE_SESSION_KEY is required"),
            "empty key must be treated as unset; got: {err}"
        );
    }

    #[test]
    fn parse_session_key_accepts_value() {
        let value = "test-session-key-32-bytes-minimum";
        assert_eq!(parse_session_key(Some(value)).unwrap(), value);
    }

    #[test]
    fn parse_session_key_rejects_short_value() {
        let err = parse_session_key(Some("short")).unwrap_err();
        assert!(
            err.contains("at least"),
            "error must mention the length floor; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn link_preview_config_parses_operator_policy_vars() {
        let config = LinkPreviewConfig::from_vars([
            ("WADDLE_LINK_PREVIEW_ENABLED", "false"),
            (
                "WADDLE_LINK_PREVIEW_ALLOWED_HOSTS",
                "example.com,*.trusted.example",
            ),
            ("WADDLE_LINK_PREVIEW_BLOCKED_HOSTS", "ads.example"),
            ("WADDLE_LINK_PREVIEW_MAX_HTML_HEAD_BYTES", "4096"),
            ("WADDLE_LINK_PREVIEW_MAX_CACHED_IMAGE_BYTES", "8192"),
            ("WADDLE_LINK_PREVIEW_MAX_REDIRECTS", "2"),
            ("WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS", "250"),
            ("WADDLE_LINK_PREVIEW_VIDEO_ENABLED", "0"),
        ])
        .expect("config");

        assert!(!config.enabled);
        assert_eq!(config.allowed_hosts.len(), 2);
        assert!(config.allowed_hosts[0].matches("example.com"));
        assert!(config.allowed_hosts[1].matches("cdn.trusted.example"));
        assert!(config.blocked_hosts[0].matches("ads.example"));
        assert_eq!(config.max_html_head_bytes, 4096);
        assert_eq!(config.max_cached_image_bytes, 8192);
        assert_eq!(config.max_redirects, 2);
        assert_eq!(config.fetch_timeout, Duration::from_millis(250));
        assert!(!config.video_enabled);
    }

    #[test]
    fn link_preview_host_patterns_reject_non_host_shapes() {
        let error = "https://example.com"
            .parse::<LinkPreviewHostPattern>()
            .expect_err("URL must not parse as host pattern");

        assert!(error.contains("invalid host pattern"));

        let error = "ads.*.example"
            .parse::<LinkPreviewHostPattern>()
            .expect_err("unsupported wildcard position must not parse");

        assert!(error.contains("invalid host pattern"));
    }

    #[test]
    fn link_preview_config_rejects_invalid_boolean_vars() {
        let error = LinkPreviewConfig::from_vars([("WADDLE_LINK_PREVIEW_ENABLED", "ture")])
            .expect_err("typo must fail startup");

        assert!(error.contains("WADDLE_LINK_PREVIEW_ENABLED"));
        assert!(error.contains("must be a boolean"));
    }

    #[test]
    fn link_preview_config_rejects_fetch_timeouts_above_startup_cap() {
        let error =
            LinkPreviewConfig::from_vars([("WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS", "61000")])
                .expect_err("oversized timeout must fail startup");

        assert!(error.contains("WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS"));
        assert!(error.contains("at most"));
    }

    #[test]
    fn parse_occupant_id_secret_rejects_unset() {
        let err = parse_occupant_id_secret(None).unwrap_err();
        assert!(
            err.contains("WADDLE_OCCUPANT_ID_SECRET is required"),
            "error must name the env var; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_occupant_id_secret_rejects_short_value() {
        let err = parse_occupant_id_secret(Some("short")).unwrap_err();
        assert!(
            err.contains("at least"),
            "error must mention the length floor; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_occupant_id_secret_accepts_minimum_length() {
        // Exactly the floor — must succeed.
        let value: String = "x".repeat(OCCUPANT_ID_SECRET_MIN_BYTES);
        let secret = parse_occupant_id_secret(Some(&value)).expect("32 bytes is accepted");
        assert_eq!(secret.key().len(), OCCUPANT_ID_SECRET_MIN_BYTES);
    }
}

#[derive(Debug, Clone)]
pub struct DatabaseRuntimeConfig {
    pub driver: DatabaseDriver,
    pub database_url: String,
}

impl Default for DatabaseRuntimeConfig {
    fn default() -> Self {
        Self {
            driver: DatabaseDriver::Sqlite,
            database_url: "sqlite::memory:".to_string(),
        }
    }
}

impl DatabaseRuntimeConfig {
    pub fn from_env() -> Result<Self, String> {
        let driver = std::env::var("WADDLE_DB_DRIVER")
            .unwrap_or_else(|_| "sqlite".to_string())
            .parse::<DatabaseDriver>()
            .map_err(|e| format!("invalid WADDLE_DB_DRIVER: {}", e))?;

        let database_url = std::env::var("WADDLE_DATABASE_URL").unwrap_or_else(|_| match driver {
            DatabaseDriver::Sqlite => "sqlite::memory:".to_string(),
            DatabaseDriver::Postgres => {
                "postgres://postgres:postgres@localhost:5432/waddle".to_string()
            }
        });

        Ok(Self {
            driver,
            database_url,
        })
    }
}
